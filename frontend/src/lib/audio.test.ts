import { beforeEach, describe, expect, mock, test } from 'bun:test';
import { dbToGain, panGains } from './mixer';
import { limiterParams } from './audio-mix';
import { alimiterDelay, limiterLimit, type LimiterMessage } from './alimiter';
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

type Kind = 'gain' | 'merger' | 'splitter' | 'analyser' | 'compressor' | 'worklet' | 'source' | 'destination';
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
class FakeWorklet extends FakeNode {
	posted: LimiterMessage[] = [];
	port = { postMessage: (m: LimiterMessage) => void this.posted.push(m) };
	constructor(ctx: FakeContext) {
		super('worklet', ctx);
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
	sampleRate = 48000;
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
});

// ---- reverse shuttle ---------------------------------------------------------

describe('reverse shuttle', () => {
	/** The preview's decode rate (`audio.ts`). */
	const RATE = 32000;

	/** An engine whose fetch answers each window with a ramp that names its samples — the `i`-th of
	 *  the window is `i + 1` — so what plays can be read back as the source time it came from; `kept`
	 *  of the window is answered, the way a decode ends early when the source's sound does. Records
	 *  each window asked for. */
	const ramped = (kept = 1) => {
		const asked: [string, number, number][] = [];
		const e = new AudioEngine({
			createContext: () => ctx as unknown as AudioContext,
			fetchPcm: (async (asset: string, start: number, dur: number) => {
				asked.push([asset, start, dur]);
				const n = Math.round(dur * RATE * kept);
				return Int16Array.from({ length: n }, (_, i) => i + 1).buffer;
			}) as never,
			report: (m) => reported.push(m)
		});
		return { e, asked };
	};

	/** The source time `src` plays at offset `offset` into its buffer, read off the ramp. */
	const heardAt = (src: FakeSource, windowStart: number, offset: number) => {
		const data = src.buffer!.data;
		const index = Math.min(data.length - 1, Math.round(offset * RATE));
		return windowStart + (Math.round(data[index] * 32768) - 1) / RATE;
	};

	/** The clip gain node a source feeds. */
	const clipGain = (src: FakeSource) => src.outs[0].to as FakeGain;

	test('plays the clips behind the playhead, nearest first, each from its end at the shuttle speed', async () => {
		const { e, asked } = ramped();
		ctx.currentTime = 10;
		e.start(
			{
				tracks: [
					track('a1', 'audio', [
						clip('first', 'voice', { timeline_start: 0, source_out: 0.5 }),
						clip('second', 'music', { timeline_start: 1, source_out: 0.5 }),
						clip('ahead', 'voice', { timeline_start: 6, source_out: 0.5 })
					])
				]
			},
			SOUND,
			5,
			-2
		);
		await settle();
		// Only what is behind the playhead is asked for, the one it meets first first.
		expect(asked.map(([asset]) => asset)).toEqual(['music', 'voice']);
		const sources = ctx.of<FakeSource>('source');
		expect(sources).toHaveLength(2);
		// The second clip ends at 1.5 s, which a 2× backwards shuttle from 5 s reaches 1.75 s later.
		const second = sources.find((s) => s.started[0][0] === 11.75)!;
		expect(second.started[0][1]).toBe(0);
		expect(second.started[0][2]).toBeCloseTo(0.5, 9);
		expect(second.playbackRate.value).toBe(2);
		ctx.currentTime = 10.5;
		expect(e.clock()).toBe(4);
	});

	test('each clip is heard at the source time its timeline time maps to (offset oracle)', async () => {
		for (const speed of [1, 2, 0.5, -1, -2])
			for (const source_in of [0, 0.25])
				for (const rate of [-1, -3])
					for (const fraction of [0.1, 0.5, 0.9])
						for (const kept of [1, 0.85]) {
							ctx = new FakeContext();
							const { e, asked } = ramped(kept);
							const start = 1.5;
							const source_out = source_in + 0.8;
							const mag = Math.abs(speed);
							const t = start + (0.8 / mag) * fraction;
							const c = clip('c', 'voice', { timeline_start: start, source_in, source_out, speed });
							e.start({ tracks: [track('a1', 'audio', [c])] }, SOUND, t, rate);
							await settle();
							const expected = speed > 0 ? source_in + (t - start) * mag : source_out - (t - start) * mag;
							// A stream that ended early has nothing to say about the source past its end.
							if (expected - asked[0][1] >= 0.8 * kept - 2 / RATE) continue;
							const [src] = ctx.of<FakeSource>('source');
							const label = `speed ${speed}, in ${source_in}, rate ${rate}, at ${fraction}, kept ${kept}`;
							expect(src.started, label).toHaveLength(1);
							expect(Math.abs(heardAt(src, asked[0][1], src.started[0][1]) - expected), label).toBeLessThan(2 / RATE);
							expect(src.playbackRate.value, label).toBeCloseTo(mag * Math.abs(rate), 9);
						}
	});

	test('one window is fetched once, and turned rather than held twice when the shuttle turns', async () => {
		const { e, asked } = ramped();
		const tl = { tracks: [track('a1', 'audio', [clip('c', 'voice', { source_out: 0.5 })])] };
		e.start(tl, SOUND, 0.25, 1);
		await settle();
		e.start(tl, SOUND, 0.25, -1);
		await settle();
		e.start(tl, SOUND, 0.25, 1);
		await settle();
		expect(asked).toHaveLength(1);
		const [forward, backward, again] = ctx.of<FakeSource>('source');
		// The same source moment from all three: forwards from the start, backwards from the end.
		expect(heardAt(forward, 0, forward.started[0][1])).toBeCloseTo(0.25, 4);
		expect(heardAt(backward, 0, backward.started[0][1])).toBeCloseTo(0.25, 4);
		expect(heardAt(again, 0, again.started[0][1])).toBeCloseTo(0.25, 4);
	});

	test('a window asked for the way round it is held is the very buffer held, not converted again', async () => {
		const tl = { tracks: [track('a1', 'audio', [clip('c', 'voice', { source_out: 0.5 })])] };
		for (const rate of [1, -1]) {
			engine.start(tl, SOUND, 0.25, rate);
			await settle();
			engine.start(tl, SOUND, 0.25, rate);
			await settle();
			const [first, second] = ctx.of<FakeSource>('source').slice(-2);
			expect(second.buffer).toBe(first.buffer);
		}
	});

	test('past 600 s of source the window is held at the end the playhead enters by', async () => {
		const asked: [string, number, number][] = [];
		const e = new AudioEngine({
			createContext: () => ctx as unknown as AudioContext,
			fetchPcm: (async (asset: string, start: number, dur: number) => {
				asked.push([asset, start, dur]);
				return new Int16Array(3200).buffer;
			}) as never,
			report: (m) => reported.push(m)
		});
		const long = { tracks: [track('a1', 'audio', [clip('fwd', 'voice', { source_out: 1000 })])] };
		const reversed = { tracks: [track('a1', 'audio', [clip('rev', 'music', { source_out: 1000, speed: -1 })])] };
		// Forwards a clip is entered at its start: its source's start, or a reversed clip's source end.
		e.start(long, SOUND, 1, 1);
		await settle();
		e.start(reversed, SOUND, 1, 1);
		await settle();
		// Backwards at its end: its source's end, or a reversed clip's source start.
		e.start(long, SOUND, 999, -1);
		await settle();
		e.start(reversed, SOUND, 999, -1);
		await settle();
		expect(asked).toEqual([
			['voice', 0, 600],
			['music', 400, 600],
			['voice', 400, 600],
			['music', 0, 600]
		]);
	});

	test('a keyed volume is met the other way round, a hold stepped from above', async () => {
		// 0.2 held for 1 s, then 1.0.
		const keyed = clip('c', 'voice', {
			source_out: 2,
			channels: [
				{
					prop: 'volume',
					keys: [
						{ time: 0, value: 0.2, easing: 'hold' },
						{ time: 1, value: 1 }
					]
				}
			]
		});
		await play({ tracks: [track('a1', 'audio', [keyed])] }, 2, -1);
		const calls = clipGain(ctx.of<FakeSource>('source')[0]).gain.calls;
		// From 2 s on the context's 0: at 1 s the ramp arrives on 1.0, steps down to the hold's
		// 0.2, and holds it to the clip's start.
		expect(calls.map(([kind, , at]) => [kind, at])).toEqual([
			['set', 0],
			['ramp', 1],
			['set', 1],
			['ramp', 2]
		]);
		expect(calls[0][1]).toBeCloseTo(1, 6);
		expect(calls[1][1]).toBeCloseTo(1, 6);
		expect(calls[2][1]).toBeCloseTo(0.2, 6);
		expect(calls[3][1]).toBeCloseTo(0.2, 6);
	});

	test('a fade is met the other way round: silent at the clip’s end, up to its level at the fade’s start', async () => {
		await play(
			{ tracks: [track('a1', 'audio', [clip('c', 'voice', { timeline_start: 0, source_out: 4, volume: 0.5, fade_out: 1 })])] },
			4,
			-1
		);
		const [first, ...rest] = clipGain(ctx.of<FakeSource>('source')[0]).gain.calls;
		expect(first[0]).toBe('set');
		expect(first[1]).toBeCloseTo(0, 6);
		expect(rest[0]).toEqual(['ramp', 0.5, 1]);
		expect(rest.at(-1)).toEqual(['ramp', 0.5, 4]);
	});

	test('turning J into L re-anchors where the clock stands, and the backwards session is torn down', async () => {
		const { e } = ramped();
		const tl = { tracks: [track('a1', 'audio', [clip('c', 'voice', { source_out: 1 })])] };
		e.start(tl, SOUND, 0.9, -1);
		await settle();
		const [back] = ctx.of<FakeSource>('source');
		ctx.currentTime = 0.5;
		const at = e.clock()!;
		expect(at).toBeCloseTo(0.4, 9);
		e.start(tl, SOUND, at, 1);
		await settle();
		expect(back.stopped).toBe(1);
		expect(back.outs).toHaveLength(0);
		const forward = ctx.of<FakeSource>('source').at(-1)!;
		expect(forward.started[0][0]).toBe(0.5);
		expect(forward.started[0][1]).toBeCloseTo(0.4, 9);
		expect(heardAt(forward, 0, forward.started[0][1])).toBeCloseTo(0.4, 4);
		ctx.currentTime = 1;
		expect(e.clock()).toBeCloseTo(0.9, 9);
	});

	test('stopping a backwards session stops and unhooks everything it built', async () => {
		await play(
			{ tracks: [track('a1', 'audio', [clip('a', 'voice', { source_out: 2 })]), track('a2', 'audio', [clip('b', 'music', { source_out: 2 })])] },
			2,
			-1
		);
		const sources = ctx.of<FakeSource>('source');
		expect(sources).toHaveLength(2);
		const gains = sources.map(clipGain);
		engine.stop();
		for (const s of sources) {
			expect(s.stopped).toBe(1);
			expect(s.outs).toHaveLength(0);
		}
		for (const g of gains) expect(g.outs).toHaveLength(0);
		expect(engine.clock()).toBeNull();
		expect(engine.meters()).toBeNull();
	});

	test('a playhead at the very start has nothing behind it to play', async () => {
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])] }, 0, -1);
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

	test('says how late the sound is: 0 before there is a context, then what the context measures', async () => {
		expect(engine.outputLatency()).toBe(0);
		await play({ tracks: [track('a1', 'audio', [clip('c', 'voice')])] });
		// The fake knows nothing of its device.
		expect(engine.outputLatency()).toBe(0);
		Object.assign(ctx, { currentTime: 5.06, getOutputTimestamp: () => ({ contextTime: 5, performanceTime: 1 }) });
		expect(engine.outputLatency()).toBeCloseTo(0.06, 9);
		// The clock is the context's and stays so: the latency is for the caller to subtract.
		expect(engine.clock()).toBeCloseTo(5.06, 9);
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

describe('the export’s limiter in a worklet', () => {
	const on = (ceiling_db: number): Timeline => ({
		tracks: [track('a1', 'audio', [clip('c', 'voice')])],
		master: { volume: 1, limiter: true, ceiling_db }
	});
	let asked: number[];
	let load: (node: FakeWorklet | null) => void;
	let fail: (e: Error) => void;
	beforeEach(() => {
		asked = [];
		engine = new AudioEngine({
			createContext: () => ctx as unknown as AudioContext,
			fetchPcm: pcm as never,
			report: (m) => reported.push(m),
			createLimiter: (_ctx, limit) => {
				asked.push(limit);
				return new Promise((resolve, reject) => {
					load = (n) => resolve(n as never);
					fail = reject;
				});
			}
		});
	});
	const input = () => ctx.of<FakeGain>('gain')[0];
	const out = () => ctx.of<FakeGain>('gain')[2];

	test('the compressor stands in until the module has loaded, then the worklet takes its place', async () => {
		await play(on(-3));
		const [comp] = ctx.of<FakeCompressor>('compressor');
		expect(input().outs.map((e) => e.to)).toEqual([comp]);
		expect(engine.outputLatency()).toBe(0);
		const node = new FakeWorklet(ctx);
		load(node);
		await settle();
		expect(input().outs.map((e) => e.to)).toEqual([node]);
		expect(node.outs.map((e) => e.to)).toEqual([out()]);
		expect(reach(input()).has(comp)).toBe(false);
		expect(node.posted.at(-1)).toEqual({ limit: limiterLimit(-3) });
	});

	test('its lookahead is added to the latency while it is in the path, and only then', async () => {
		await play(on(-1.5));
		load(new FakeWorklet(ctx));
		await settle();
		expect(engine.outputLatency()).toBe(239 / 48000);
		expect(engine.outputLatency()).toBe(alimiterDelay(48000));
		engine.liveMaster({ volume: 1, limiter: false, ceiling_db: -1.5 });
		expect(engine.outputLatency()).toBe(0);
		expect(input().outs.map((e) => e.to)).toEqual([out()]);
	});

	test('a ceiling moved live is posted to it, and the module is loaded once per context', async () => {
		await play(on(-1.5));
		const node = new FakeWorklet(ctx);
		load(node);
		await settle();
		engine.liveMaster({ volume: 1, limiter: true, ceiling_db: -9 });
		expect(node.posted.at(-1)).toEqual({ limit: limiterLimit(-9) });
		await play(on(-6));
		expect(node.posted.at(-1)).toEqual({ limit: limiterLimit(-6) });
		expect(asked).toHaveLength(1);
		expect(ctx.of('worklet')).toHaveLength(1);
	});

	test('a webview that cannot run it keeps the compressor, with no lookahead to add', async () => {
		await play(on(-3));
		fail(new Error('refused'));
		await settle();
		const [comp] = ctx.of<FakeCompressor>('compressor');
		expect(input().outs.map((e) => e.to)).toEqual([comp]);
		expect(engine.outputLatency()).toBe(0);
		expect(reported).toEqual([]);
	});

	test('nor without an AudioWorklet, by default', async () => {
		engine = new AudioEngine({ createContext: () => ctx as unknown as AudioContext, fetchPcm: pcm as never });
		await play(on(-3));
		await settle();
		expect(input().outs.map((e) => e.to)).toEqual(ctx.of('compressor'));
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
