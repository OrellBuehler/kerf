import { describe, expect, test } from 'bun:test';
import { Auditioner, type AuditionState, type PlayerContext, type PlayerSource } from './sfx-audition';

class FakeSource implements PlayerSource {
	buffer: unknown = null;
	onended: (() => void) | null = null;
	connected: unknown = null;
	started = 0;
	stopped = 0;
	connect(d: unknown) {
		this.connected = d;
	}
	start() {
		this.started++;
	}
	stop() {
		this.stopped++;
	}
	finish() {
		this.onended?.();
	}
}

function fake(opts: { state?: string; decode?: (b: ArrayBuffer) => Promise<unknown>; resume?: () => Promise<void>; wait?: number } = {}) {
	const sources: FakeSource[] = [];
	const ctx: PlayerContext & { made: number; resumed: number } = {
		state: opts.state ?? 'running',
		destination: 'speakers',
		made: 0,
		resumed: 0,
		async resume() {
			this.resumed++;
			if (opts.resume) return opts.resume();
			this.state = 'running';
		},
		decodeAudioData: opts.decode ?? (async (b) => ({ decoded: b.byteLength })),
		createBufferSource() {
			const s = new FakeSource();
			sources.push(s);
			return s;
		}
	};
	let made = 0;
	const player = new Auditioner(() => {
		made++;
		return ctx;
	}, opts.wait);
	const seen: AuditionState[] = [];
	player.onchange = (s) => seen.push(s);
	return { player, ctx, sources, seen, made: () => made };
}

const bytes = (n = 8) => new ArrayBuffer(n);

describe('the auditioner', () => {
	test('plays a decoded sound once, to the speakers, and says so', async () => {
		const { player, sources, seen } = fake();
		await player.play(bytes(16));
		expect(player.state).toBe('playing');
		expect(sources).toHaveLength(1);
		expect(sources[0].buffer).toEqual({ decoded: 16 });
		expect(sources[0].connected).toBe('speakers');
		expect(sources[0].started).toBe(1);
		expect(seen).toEqual(['playing']);
	});

	test('a sound that runs out leaves it idle', async () => {
		const { player, sources, seen } = fake();
		await player.play(bytes());
		sources[0].finish();
		expect(player.state).toBe('idle');
		expect(seen).toEqual(['playing', 'idle']);
	});

	test('stop silences it and is quiet when nothing plays', async () => {
		const { player, sources, seen } = fake();
		player.stop();
		expect(seen).toEqual([]);
		await player.play(bytes());
		player.stop();
		expect(sources[0].stopped).toBe(1);
		expect(player.state).toBe('idle');
		expect(seen).toEqual(['playing', 'idle']);
		sources[0].finish();
		expect(seen).toEqual(['playing', 'idle']);
	});

	test('a new sound replaces the one playing, and the old one’s ending does not stop the new one', async () => {
		const { player, sources } = fake();
		await player.play(bytes());
		await player.play(bytes());
		expect(sources[0].stopped).toBe(1);
		expect(player.state).toBe('playing');
		sources[0].finish();
		expect(player.state).toBe('playing');
		sources[1].finish();
		expect(player.state).toBe('idle');
	});

	test('a stop made while a sound decodes wins: nothing is played from it', async () => {
		let release!: () => void;
		const gate = new Promise<void>((r) => (release = r));
		const { player, sources } = fake({
			decode: async () => {
				await gate;
				return {};
			}
		});
		const pending = player.play(bytes());
		player.stop();
		release();
		await pending;
		expect(sources).toHaveLength(0);
		expect(player.state).toBe('idle');
	});

	test('of two sounds decoding, only the later one plays', async () => {
		const gates: (() => void)[] = [];
		let n = 0;
		const { player, sources } = fake({
			decode: () => {
				const id = n++;
				return new Promise((r) => gates.push(() => r({ id })));
			}
		});
		const first = player.play(bytes());
		const second = player.play(bytes());
		await Promise.resolve();
		gates[1]();
		gates[0]();
		await Promise.all([first, second]);
		expect(sources).toHaveLength(1);
		expect(sources[0].buffer).toEqual({ id: 1 });
	});

	test('a context the platform suspended is resumed, and the context is made once', async () => {
		const { player, ctx, made } = fake({ state: 'suspended' });
		await player.play(bytes());
		await player.play(bytes());
		expect(ctx.resumed).toBe(1);
		expect(made()).toBe(1);
	});

	test('a sound that cannot be decoded rejects and leaves it idle', async () => {
		const { player, sources } = fake({ decode: async () => Promise.reject(new Error('EncodingError')) });
		await expect(player.play(bytes())).rejects.toThrow('EncodingError');
		expect(sources).toHaveLength(0);
		expect(player.state).toBe('idle');
	});

	test('no context is made until something is played', () => {
		const { player, made } = fake();
		player.stop();
		expect(made()).toBe(0);
	});

	test('prime makes the context and asks it to run at once, before anything is decoded, and only the once', async () => {
		const { player, ctx, made } = fake({ state: 'suspended' });
		player.prime();
		expect(made()).toBe(1);
		expect(ctx.resumed).toBe(1);
		await Promise.resolve();
		expect(ctx.state).toBe('running');
		player.prime();
		expect(made()).toBe(1);
		expect(ctx.resumed).toBe(1);
		await player.play(bytes());
		expect(made()).toBe(1);
		expect(player.state).toBe('playing');
	});

	test('prime on a platform with no audio says nothing, and play says why', async () => {
		const player = new Auditioner(() => {
			throw new Error('AudioContext is not defined');
		});
		expect(() => player.prime()).not.toThrow();
		await expect(player.play(bytes())).rejects.toThrow('AudioContext is not defined');
		expect(player.state).toBe('idle');
	});

	test('a resume that is never answered does not hold the render up for ever: it gives up and says so', async () => {
		const { player, sources } = fake({ state: 'suspended', resume: () => new Promise<void>(() => {}), wait: 15 });
		const started = Date.now();
		await expect(player.play(bytes())).rejects.toThrow('audio output would not start');
		expect(Date.now() - started).toBeLessThan(1000);
		expect(sources).toHaveLength(0);
		expect(player.state).toBe('idle');
	});

	test('a context that comes back while the sound decodes plays it, however late the resume answers', async () => {
		let answer!: () => void;
		const ctxState = fake({
			state: 'suspended',
			resume: () =>
				new Promise<void>((r) => {
					answer = r;
				}),
			wait: 500
		});
		const pending = ctxState.player.play(bytes());
		await Promise.resolve();
		ctxState.ctx.state = 'running';
		answer();
		await pending;
		expect(ctxState.player.state).toBe('playing');
		expect(ctxState.sources).toHaveLength(1);
	});
});
