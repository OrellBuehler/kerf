import { beforeEach, describe, expect, mock, test } from 'bun:test';
import { dbToGain, panGains } from './mixer';
import { limiterParams } from './audio-mix';
import type { Clip, Timeline, Track } from './types';

// The preview's audio graph, driven over a fake `AudioContext` that records what is
// built and wired. What is pinned here is the contract the Mixer relies on: a bus per
// track that plays sound, the fader and pan on it (and the product *at the speakers*
// equal to what the export's graph gives), the master and its limiter wiring, live
// moves, and the meters' readings — plus that the scheduling the engine always did
// (which tracks and clips play, and when) did not move.

// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(() => {}, { success() {}, warning() {}, error() {}, info() {}, dismiss() {} })
}));
const { AudioEngine } = await import('./audio');

// ---- a fake Web Audio ------------------------------------------------------

type Kind = 'gain' | 'merger' | 'splitter' | 'analyser' | 'compressor' | 'source' | 'destination';
interface Edge {
	to: FakeNode;
	output: number;
	input: number;
}

class FakeParam {
	value: number;
	calls: [string, ...number[]][] = [];
	constructor(value: number) {
		this.value = value;
	}
	setValueAtTime(v: number, t: number) {
		this.calls.push(['set', v, t]);
		this.value = v;
	}
	linearRampToValueAtTime(v: number, t: number) {
		this.calls.push(['ramp', v, t]);
	}
	setTargetAtTime(v: number, t: number, tau: number) {
		this.calls.push(['target', v, t, tau]);
		this.value = v;
	}
}

class FakeNode {
	outs: Edge[] = [];
	disconnected = 0;
	constructor(
		public kind: Kind,
		public ctx: FakeContext
	) {
		ctx.nodes.push(this);
	}
	connect(to: FakeNode, output = 0, input = 0) {
		this.outs.push({ to, output, input });
		return to;
	}
	disconnect() {
		this.outs = [];
		this.disconnected++;
	}
}

class FakeGain extends FakeNode {
	gain = new FakeParam(1);
	constructor(ctx: FakeContext) {
		super('gain', ctx);
	}
}
class FakeCompressor extends FakeNode {
	threshold = new FakeParam(-24);
	knee = new FakeParam(30);
	ratio = new FakeParam(12);
	attack = new FakeParam(0.003);
	release = new FakeParam(0.25);
	constructor(ctx: FakeContext) {
		super('compressor', ctx);
	}
}
class FakeAnalyser extends FakeNode {
	fftSize = 2048;
	/** What the next read returns; the block is padded with silence. */
	signal: number[] = [];
	constructor(ctx: FakeContext) {
		super('analyser', ctx);
	}
	getFloatTimeDomainData(buf: Float32Array) {
		buf.fill(0);
		this.signal.forEach((x, i) => (buf[i] = x));
	}
}
class FakeSource extends FakeNode {
	buffer: FakeBuffer | null = null;
	playbackRate = new FakeParam(1);
	started: [number, number, number][] = [];
	stopped = 0;
	constructor(ctx: FakeContext) {
		super('source', ctx);
	}
	start(when: number, offset: number, duration: number) {
		this.started.push([when, offset, duration]);
	}
	stop() {
		this.stopped++;
	}
}
class FakeBuffer {
	data: Float32Array;
	constructor(
		public channels: number,
		public length: number,
		public sampleRate: number
	) {
		this.data = new Float32Array(length);
	}
	get duration() {
		return this.length / this.sampleRate;
	}
	getChannelData() {
		return this.data;
	}
}

class FakeContext {
	nodes: FakeNode[] = [];
	currentTime = 0;
	state = 'running';
	destination = new FakeNode('destination', this);
	createGain = () => new FakeGain(this);
	createChannelMerger = () => new FakeNode('merger', this);
	createChannelSplitter = () => new FakeNode('splitter', this);
	createAnalyser = () => new FakeAnalyser(this);
	createDynamicsCompressor = () => new FakeCompressor(this);
	createBufferSource = () => new FakeSource(this);
	createBuffer = (ch: number, len: number, rate: number) => new FakeBuffer(ch, len, rate);
	resume = async () => {};
	of<T extends FakeNode>(kind: Kind): T[] {
		return this.nodes.filter((n) => n.kind === kind) as T[];
	}
}

/** The gain at the speakers, left and right, of everything a source plays — the product
 *  along each path, summed. A merger input puts a mono signal on its channel; the
 *  compressor is taken as transparent (it reduces nothing below its threshold). */
function heard(ctx: FakeContext): [number, number] {
	const total: [number, number] = [0, 0];
	const walk = (node: FakeNode, sig: [number, number]) => {
		if (node.kind === 'destination') {
			total[0] += sig[0];
			total[1] += sig[1];
			return;
		}
		let out = sig;
		if (node instanceof FakeGain) out = [sig[0] * node.gain.value, sig[1] * node.gain.value];
		for (const e of node.outs) {
			let s = out;
			if (e.to.kind === 'merger') {
				const mono = (s[0] + s[1]) / 2;
				s = e.input === 0 ? [mono, 0] : [0, mono];
			}
			walk(e.to, s);
		}
	};
	for (const src of ctx.of<FakeSource>('source')) walk(src, [1, 1]);
	return total;
}

/** Every node `from` reaches. */
function reach(from: FakeNode): Set<FakeNode> {
	const seen = new Set<FakeNode>();
	const visit = (n: FakeNode) => {
		for (const e of n.outs) {
			if (!seen.has(e.to)) {
				seen.add(e.to);
				visit(e.to);
			}
		}
	};
	visit(from);
	return seen;
}

// ---- fixtures --------------------------------------------------------------

const clip = (id: string, asset_id: string, extra: Partial<Clip> & Record<string, unknown> = {}): Clip =>
	({ id, asset_id, source_in: 0, source_out: 10, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const track = (id: string, kind: 'video' | 'audio', clips: Clip[], extra: Partial<Track> = {}): Track =>
	({ id, kind, name: id.toUpperCase(), clips, ...extra }) as Track;
const SOUND = new Set(['voice', 'music']);

/** A fetch that answers every window with 100 ms of silence at the preview rate. */
const pcm = async () => new Int16Array(3200).buffer as ArrayBuffer;

let ctx: FakeContext;
let engine: InstanceType<typeof AudioEngine>;
let reported: string[];

beforeEach(() => {
	ctx = new FakeContext();
	reported = [];
	engine = new AudioEngine({
		createContext: () => ctx as unknown as AudioContext,
		fetchPcm: pcm as never,
		report: (m) => reported.push(m)
	});
});

/** Lets the buffers resolve and the clips schedule. */
const settle = () => new Promise<void>((r) => setTimeout(r, 0));

const play = async (timeline: Timeline, t = 0, rate = 1) => {
	engine.start(timeline, SOUND, t, rate);
	await settle();
};

// ---- the buses -------------------------------------------------------------

describe('a bus per track that plays sound', () => {
	test('is built for audio tracks and for picture tracks that carry sound — not for silent ones', async () => {
		await play({
			tracks: [
				track('v1', 'video', [clip('c1', 'voice')]),
				track('v2', 'video', [clip('c2', 'broll')]),
				track('a1', 'audio', [clip('c3', 'music')])
			]
		});
		expect([...engine.meters()!.tracks.keys()]).toEqual(['v1', 'a1']);
	});

	test('a detached picture adds none — its audio clip carries the sound once', async () => {
		await play({
			tracks: [
				track('v1', 'video', [clip('c1', 'voice', { source_audio: false })]),
				track('a1', 'audio', [clip('c3', 'voice')])
			]
		});
		expect([...engine.meters()!.tracks.keys()]).toEqual(['a1']);
		expect(ctx.of('source')).toHaveLength(1);
	});

	test('sound that is muted, soloed out, disabled or already over is never scheduled', async () => {
		await play(
			{
				tracks: [
					track('a1', 'audio', [clip('c1', 'voice')], { muted: true }),
					track('a2', 'audio', [clip('c2', 'voice')], { solo: true }),
					track('a3', 'audio', [clip('c3', 'voice')]), // shadowed by a2's solo
					track('a4', 'audio', [clip('c4', 'voice', { enabled: false })], { solo: true }),
					track('a5', 'audio', [clip('c5', 'voice', { timeline_start: 0, source_out: 4 })], { solo: true })
				]
			},
			5
		);
		// a2 and a4/a5 are soloed; a4's only clip is disabled and a5's ended at 4 s.
		expect([...engine.meters()!.tracks.keys()]).toEqual(['a2']);
		expect(ctx.of('source')).toHaveLength(1);
	});

	test('a solo shadows tracks of its own kind, as the export does — a picture’s sound too', async () => {
		await play({
			tracks: [
				track('v1', 'video', [clip('c1', 'voice')], { solo: true }),
				track('v2', 'video', [clip('c2', 'voice')]),
				track('a1', 'audio', [clip('c3', 'voice')])
			]
		});
		// v2 is shadowed by v1's solo (its clips are dropped before the export mixes); the audio
		// track is another kind and plays.
		expect([...engine.meters()!.tracks.keys()]).toEqual(['v1', 'a1']);
	});

	test('a reverse shuttle plays nothing', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])] }, 0, -1);
		expect(engine.meters()).toBeNull();
		expect(ctx.of('source')).toHaveLength(0);
	});
});

describe('the mix at the speakers', () => {
	test('is exactly what the export’s factors give: clip volume × fader × the balance, per side', async () => {
		const cases = [
			{ volume: 1, fader: 1, pan: 0 },
			{ volume: 0.5, fader: 0.5, pan: -0.5 },
			{ volume: 1.7, fader: 0.3, pan: 1 },
			{ volume: 0.8, fader: 2, pan: 0.25 },
			{ volume: 1, fader: 0, pan: -1 }
		];
		for (const c of cases) {
			ctx = new FakeContext();
			engine = new AudioEngine({ createContext: () => ctx as unknown as AudioContext, fetchPcm: pcm as never });
			await play({ tracks: [track('a1', 'audio', [clip('c', 'voice', { volume: c.volume })], { volume: c.fader, pan: c.pan })] });
			const [gl, gr] = panGains(c.pan);
			const [l, r] = heard(ctx);
			expect(l).toBeCloseTo(c.volume * c.fader * gl, 12);
			expect(r).toBeCloseTo(c.volume * c.fader * gr, 12);
		}
	});

	test('two tracks sum at the speakers', async () => {
		await play({
			tracks: [
				track('a1', 'audio', [clip('c1', 'voice')], { volume: 0.5 }),
				track('a2', 'audio', [clip('c2', 'music')], { volume: 0.25, pan: 1 })
			]
		});
		const [l, r] = heard(ctx);
		expect(l).toBeCloseTo(0.5 + 0, 12);
		expect(r).toBeCloseTo(0.5 + 0.25, 12);
	});

	test('the master fader scales the whole sum', async () => {
		await play({
			tracks: [track('a1', 'audio', [clip('c1', 'voice')], { volume: 0.5 })],
			master: { volume: 2, limiter: false, ceiling_db: -1 }
		});
		const [l, r] = heard(ctx);
		expect(l).toBeCloseTo(1, 12);
		expect(r).toBeCloseTo(1, 12);
	});

	test('a track with no master set plays at unity through it', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c1', 'voice')])] });
		expect(heard(ctx)).toEqual([1, 1]);
	});

	test('the clip’s own gain carries its fades and volume but not the fader', async () => {
		await play({
			tracks: [track('a1', 'audio', [clip('c', 'voice', { volume: 0.5, fade_in: 2 })], { volume: 0.1 })]
		});
		const [src] = ctx.of<FakeSource>('source');
		const gain = src.outs[0].to as FakeGain;
		// Volume 0.5 shaped by a 2 s fade-in that has not begun: silent at the start, then ramping to 0.5.
		expect(gain.gain.calls[0]).toEqual(['set', 0, 0]);
		expect(gain.gain.calls[1]).toEqual(['ramp', 0.5, 2]);
		// The fader is on the bus the clip feeds.
		const bus = gain.outs[0].to as FakeGain;
		expect(bus.gain.value).toBe(0.1);
	});
});

describe('scheduling is what it was', () => {
	test('a clip starts at its place on the audio clock, from its start', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice', { timeline_start: 2 })])] });
		const [src] = ctx.of<FakeSource>('source');
		// 100 ms of preview audio: the window is the buffer's.
		expect(src.started).toEqual([[2, 0, 0.1]]);
		expect(src.playbackRate.value).toBe(1);
	});

	test('playing from the middle of a clip seeks into it, at the clip’s speed', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice', { timeline_start: 2, speed: 2, source_out: 20 })])] }, 3, 2);
		const [src] = ctx.of<FakeSource>('source');
		// 1 s into a clip at 2x speed is 2 s of source; the shuttle rate multiplies the speed.
		expect(src.started).toHaveLength(0); // its 100 ms buffer ends before a 2 s offset
		expect(src.playbackRate.value).toBe(4);
	});

	test('the clock follows the context, anchored where playback began', async () => {
		ctx.currentTime = 10;
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])] }, 4, 2);
		ctx.currentTime = 11;
		expect(engine.clock()).toBe(6);
		engine.stop();
		expect(engine.clock()).toBeNull();
	});

	test('a window that cannot be decoded is reported once and does not stop the rest', async () => {
		const failing = new AudioEngine({
			createContext: () => ctx as unknown as AudioContext,
			fetchPcm: (async (asset: string) => {
				if (asset === 'voice') throw new Error('no such file');
				return new Int16Array(3200).buffer;
			}) as never,
			report: (m) => reported.push(m)
		});
		const tl = { tracks: [track('a1', 'audio', [clip('c1', 'voice'), clip('c2', 'music')])] };
		failing.start(tl, SOUND, 0, 1);
		await settle();
		failing.start(tl, SOUND, 0, 1); // a resync retries; nobody needs to hear twice
		await settle();
		expect(reported).toHaveLength(1);
		expect(reported[0]).toContain('no such file');
		expect(ctx.of('source').length).toBeGreaterThanOrEqual(1);
	});
});

// ---- the master ------------------------------------------------------------

describe('the master limiter', () => {
	const withMaster = (master: Timeline['master']): Timeline => ({
		tracks: [track('a1', 'audio', [clip('c', 'voice')])],
		master
	});
	const master = () => {
		const [comp] = ctx.of<FakeCompressor>('compressor');
		const gains = ctx.of<FakeGain>('gain');
		// The master's gains are the first the engine builds, in order: input, trim, out.
		return { comp, input: gains[0], trim: gains[1], out: gains[2] };
	};

	test('off: the master gain feeds the output directly and the compressor is out of the path', async () => {
		await play(withMaster(undefined));
		const m = master();
		expect(m.input.outs.map((e) => e.to)).toEqual([m.out]);
		expect(reach(m.input).has(m.comp)).toBe(false);
	});

	test('on: the path is gain → compressor → trim → output, set to the ceiling', async () => {
		await play(withMaster({ volume: 1, limiter: true, ceiling_db: -3 }));
		const m = master();
		expect(m.input.outs.map((e) => e.to)).toEqual([m.comp]);
		expect(m.comp.outs.map((e) => e.to)).toEqual([m.trim]);
		expect(m.trim.outs.map((e) => e.to)).toEqual([m.out]);
		const p = limiterParams(-3);
		expect(m.comp.threshold.value).toBe(-3);
		expect(m.comp.knee.value).toBe(0);
		expect(m.comp.ratio.value).toBe(20);
		expect(m.comp.attack.value).toBe(p.attack);
		expect(m.comp.release.value).toBe(p.release);
		expect(m.trim.gain.value).toBe(p.trim);
	});

	test('toggling it rewires the same nodes rather than building new ones', async () => {
		await play(withMaster({ volume: 1, limiter: true, ceiling_db: -6 }));
		const before = ctx.nodes.length;
		const m = master();
		engine.start(withMaster({ volume: 1, limiter: false, ceiling_db: -6 }), SOUND, 0, 1);
		await settle();
		expect(reach(m.input).has(m.comp)).toBe(false);
		engine.start(withMaster({ volume: 1, limiter: true, ceiling_db: -12 }), SOUND, 0, 1);
		await settle();
		expect(ctx.of('compressor')).toHaveLength(1);
		expect(m.comp.threshold.value).toBe(-12);
		expect(reach(m.input).has(m.comp)).toBe(true);
		// Only the clips and buses are new each start; the master chain persists.
		expect(ctx.of('gain').length).toBeGreaterThan(before - 20);
		expect(ctx.of('destination')).toHaveLength(1);
	});

	test('the output is what is metered, and it is what reaches the speakers', async () => {
		await play(withMaster(undefined));
		const m = master();
		expect(reach(m.out).has(ctx.destination)).toBe(true);
		expect(m.out.outs.some((e) => e.to.kind === 'splitter')).toBe(true);
	});

	test('the master fader is clamped to the engine’s +12 dB', async () => {
		await play(withMaster({ volume: 9, limiter: false, ceiling_db: -1 }));
		expect(master().input.gain.value).toBe(4);
	});
});

// ---- live moves ------------------------------------------------------------

describe('moving a control while it is dragged', () => {
	/** The tracks' faders: the gains that feed a pan leg on each side. */
	const busInputs = () => ctx.of<FakeGain>('gain').filter((g) => g.outs.length === 2 && g.outs.every((e) => e.to instanceof FakeGain));
	const tl = (): Timeline => ({
		tracks: [track('a1', 'audio', [clip('c', 'voice')], { volume: 0.5, pan: 0.5 }), track('a2', 'audio', [clip('d', 'music')])],
		master: { volume: 1, limiter: false, ceiling_db: -1 }
	});

	test('a fader move reaches the bus at once, smoothed, and only that track’s', async () => {
		await play(tl());
		const buses = busInputs();
		engine.liveTrack('a1', { volume: 0.25 });
		const a1 = buses[0];
		const a2 = buses[1];
		expect(a1.gain.calls.at(-1)?.[0]).toBe('target');
		expect(a1.gain.value).toBe(0.25);
		expect(a2.gain.value).toBe(1);
		expect(a1.gain.calls.at(-1)?.[3]).toBeGreaterThan(0);
	});

	test('a pan move retunes the two legs to the balance', async () => {
		await play(tl());
		engine.liveTrack('a1', { pan: -0.5 });
		const merger = ctx.of('merger')[0];
		const legs = ctx.of<FakeGain>('gain').filter((g) => g.outs.some((e) => e.to === merger));
		const [gl, gr] = panGains(-0.5);
		expect(legs.map((g) => g.gain.value)).toEqual([gl, gr]);
	});

	test('a track that is not playing is nothing to move', async () => {
		await play(tl());
		expect(() => engine.liveTrack('nope', { volume: 0.1 })).not.toThrow();
		engine.stop();
		expect(() => engine.liveTrack('a1', { volume: 0.1 })).not.toThrow();
	});

	test('the master follows, limiter and ceiling included', async () => {
		await play(tl());
		engine.liveMaster({ volume: 0.5, limiter: true, ceiling_db: -6 });
		const comp = ctx.of<FakeCompressor>('compressor')[0];
		const input = ctx.of<FakeGain>('gain')[0];
		expect(input.gain.value).toBe(0.5);
		expect(comp.threshold.value).toBe(-6);
		expect(input.outs.map((e) => e.to)).toEqual([comp]);
	});

	test('abandoning a gesture puts every control back to the timeline’s', async () => {
		const timeline = tl();
		await play(timeline);
		engine.liveTrack('a1', { volume: 0.01, pan: 1 });
		engine.liveMaster({ volume: 4, limiter: true, ceiling_db: -20 });
		engine.restoreMix(timeline);
		const buses = busInputs();
		expect(buses[0].gain.value).toBe(0.5);
		const merger = ctx.of('merger')[0];
		const legs = ctx.of<FakeGain>('gain').filter((g) => g.outs.some((e) => e.to === merger));
		expect(legs.map((g) => g.gain.value)).toEqual(panGains(0.5));
		expect(ctx.of<FakeGain>('gain')[0].gain.value).toBe(1);
		expect(reach(ctx.of<FakeGain>('gain')[0]).has(ctx.of<FakeCompressor>('compressor')[0])).toBe(false);
	});
});

// ---- the meters ------------------------------------------------------------

describe('the meters', () => {
	/** The analysers fed by the splitter that listens to `node`, left then right. */
	const analysersOn = (node: FakeNode): [FakeAnalyser, FakeAnalyser] => {
		const split = node.outs.map((e) => e.to).find((n) => n.kind === 'splitter')!;
		const byOutput = [...split.outs].sort((a, b) => a.output - b.output).map((e) => e.to as FakeAnalyser);
		return [byOutput[0], byOutput[1]];
	};

	test('nothing is being read until something plays', () => {
		expect(engine.meters()).toBeNull();
	});

	test('read the latest block of each channel of each strip, peak and RMS', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')]), track('a2', 'audio', [clip('d', 'music')])] });
		const mergers = ctx.of('merger');
		const [a1l, a1r] = analysersOn(mergers[0]);
		const [a2l] = analysersOn(mergers[1]);
		a1l.signal = [0.5, -0.5, 0.5, -0.5];
		a1r.signal = [0.25];
		a2l.signal = [1, -1];
		const frame = engine.meters()!;
		const a1 = frame.tracks.get('a1')!;
		expect(a1.l.peak).toBeCloseTo(-6.02, 1);
		expect(a1.l.rms).toBeLessThan(a1.l.peak); // 4 of 2048 samples: far under the peak
		expect(a1.r.peak).toBeCloseTo(-12.04, 1);
		expect(frame.tracks.get('a2')!.l.peak).toBeCloseTo(0, 6);
		// An analyser with nothing in it reads as the floor.
		expect(frame.tracks.get('a2')!.r.peak).toBe(-90);
	});

	test('the master is metered after its fader and limiter', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])], master: { volume: 1, limiter: true, ceiling_db: -3 } });
		const out = ctx.of<FakeGain>('gain')[2]; // the master's final stage
		const [l, r] = analysersOn(out);
		l.signal = [0.5];
		r.signal = [0.1];
		const m = engine.meters()!.master;
		expect(m.l.peak).toBeCloseTo(-6.02, 1);
		expect(m.r.peak).toBeCloseTo(-20, 1);
	});

	test('stopping tears the buses down, and the readings stop', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])] });
		const merger = ctx.of('merger')[0];
		const [src] = ctx.of<FakeSource>('source');
		engine.stop();
		expect(engine.meters()).toBeNull();
		expect(src.stopped).toBe(1);
		expect(merger.outs).toEqual([]);
		expect(src.outs).toEqual([]);
	});

	test('a restart gives each track a fresh bus and keeps the master', async () => {
		const tl: Timeline = { tracks: [track('a1', 'audio', [clip('c', 'voice')])] };
		await play(tl);
		const masterGain = ctx.of<FakeGain>('gain')[0];
		engine.start(tl, SOUND, 0, 1);
		await settle();
		expect(ctx.of('merger')).toHaveLength(2);
		expect(ctx.of<FakeGain>('gain')[0]).toBe(masterGain);
		expect(engine.meters()!.tracks.size).toBe(1);
	});
});

describe('unity', () => {
	test('a neutral track and master leave a clip at the gain it always had', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice', { volume: 1.25 })])] });
		const [l, r] = heard(ctx);
		expect(l).toBe(1.25);
		expect(r).toBe(1.25);
		expect(dbToGain(0)).toBe(1);
	});
});
