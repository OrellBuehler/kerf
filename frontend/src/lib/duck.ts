/* Ducking: what the export does to a track flagged Duck, as one pure state machine that the preview's
 * AudioWorklet (`duck-worklet.ts`) and the lane overlay (`duck-envelope.ts`) both run.
 *
 * The export (`filter_graph.rs`, `audio_graph.rs`) mixes the tracks flagged Duck into a bus of their
 * own and dips it under the rest of the mix — the *key* — in one of two ways, chosen by the master's
 * `duck_depth_db`:
 *
 *  - **Compressor** (no depth): `sidechaincompress=threshold=0.05:ratio=8:attack=20:release=400`. The bus
 *    is turned down by how far the key's level is over -26 dBFS, eight to one, with a soft knee: a quiet
 *    voice dips it a little, a loud one a lot. `Compressor` below is that filter's `compressor()` /
 *    `output_gain()` in doubles.
 *  - **Speech gate** (a depth): the bus drops by exactly the depth while the key speaks. The export makes
 *    the gain as a signal — a constant 1 through `sidechaingate` keyed by the rest (threshold 0.03,
 *    10 ms attack, 300 ms release, ratio 9000: an on / off switch), smoothed by a 7 Hz and a 1.2 Hz
 *    one-pole low-pass of which the larger is taken, then `1 - (1 - 10^(depth/20)) * curve`. `Gate`
 *    is that chain.
 *
 * Both run per sample (`Ducker.step`) from the key's left and right samples. The lane overlay has no
 * samples, only the waveform's peaks, so it runs the same machine on a *power* envelope
 * (`Ducker.stepPower`, with `stride` the number of samples one step stands for): the smoothing is the
 * same recursion with its coefficient raised to the stride, so a hop of a few milliseconds draws what
 * the samples would.
 *
 * Where the export is odd, this is odd the same way, on purpose — a preview kinder than the file would be
 * what it is meant not to be:
 *
 *  - the compressor's output **ends with the shorter of its two inputs**, so a ducked track runs only as
 *    far as the last clip of the rest of the mix (`COMPRESSOR_ENDS_WITH_KEY`; `duckPlan` says where). The
 *    speech gate pads its key, so it does not.
 *  - a key of digital silence holds the gate open (the filter's slope stays 0), so the export adds 1e-6 to
 *    the key before the gate, which `GATE_KEY_FLOOR` does here.
 *
 * The numbers are the Rust's; `duck.test.ts` reads them out of `filter_graph.rs` / `audio_graph.rs` and
 * fails when they move, and replays a corpus the real ffmpeg wrote. */

import { clampDuckDepth, DUCK_DEFAULT_DEPTH_DB } from './levels';
import type { MasterBus } from './types';

/** `sidechaincompress=threshold=0.05:ratio=8:attack=20:release=400` (`filter_graph.rs`); the knee and the
 *  rest are the filter's defaults. */
export const COMPRESSOR = { threshold: 0.05, ratio: 8, attackMs: 20, releaseMs: 400, knee: 2.82843 } as const;

/** `sidechaingate=threshold=0.03:range=0:ratio=9000:knee=1:attack=10:release=300` (`duck_gate_chains`). */
export const GATE = { threshold: 0.03, range: 0, ratio: 9000, attackMs: 10, releaseMs: 300 } as const;

/** The two one-pole low-passes (`lowpass=f=7:p=1`, `lowpass=f=1.2:p=1`) the gate's 0 / 1 curve goes through,
 *  Hz: the larger of the two is taken, so the gain eases down with the fast one (about 50 ms) and back
 *  up with the slow one (about 300 ms). */
export const GATE_SMOOTH_HZ = { fast: 7, slow: 1.2 } as const;

/** What `duck_gate_chains` adds to the key before the gate (`aeval=val(0)+0.000001`). */
export const GATE_KEY_FLOOR = 0.000001;

/** The weight of each channel when the export folds the key to mono for the gate (`aformat=channel_layouts=mono`: swresample's
 *  stereo -> mono, which does not renormalise, so a centred signal comes out 3 dB up), as a float, which is what it is. */
export const MONO_FOLD = Math.fround(Math.SQRT1_2);

/** The compressor path ends where its key does (see above). `true` is the export as it is; the day the
 *  engine pads that key too, this goes `false` and the preview follows. */
export const COMPRESSOR_ENDS_WITH_KEY = true;

/** The name the preview's worklet registers its processor under (`duck-worklet.ts`). */
export const DUCK_PROCESSOR = 'kerf-duck';

/** What the main thread posts to the duck worklet (and builds it with). */
export interface DuckMessage {
	mode?: DuckMode;
	/** Start over: nothing heard yet (playback started), exactly as a new instance. */
	reset?: boolean;
	/** The context time, in seconds, from which the bus is silent (where the compressor path ends), or `null` for never. */
	cutAt?: number | null;
}

/** What the worklet posts back: the lowest gain it put on the bus since its last report. */
export interface DuckReport {
	gain: number;
}

/** How a change of the gate's depth is followed in the preview, ms: a hand on the depth slider is heard as a move, not a step. */
export const DUCK_GLIDE_MS = 20;

/** How the master asks the tracks flagged Duck to dip. */
export type DuckMode = { kind: 'compressor' } | { kind: 'gate'; depthDb: number };

/** The mode a master asks for: the speech gate while it carries a depth (`MasterBus::safe_duck_depth_db`:
 *  held to -40..-1 dB, and a depth that is not a number is the default one), the compressor otherwise. */
export function duckMode(master: Pick<MasterBus, 'duck_depth_db'>): DuckMode {
	const depth = master.duck_depth_db;
	if (depth === undefined || depth === null) return { kind: 'compressor' };
	return { kind: 'gate', depthDb: Number.isFinite(depth) ? clampDuckDepth(depth) : DUCK_DEFAULT_DEPTH_DB };
}

/** `1 - 10^(depth/20)` as `duck_gate_chains` writes it into the graph (`{cut:.6}`): to six decimals. */
export const gateCut = (depthDb: number): number => Math.round((1 - 10 ** (depthDb / 20)) * 1e6) / 1e6;

/** A one-pole smoother's coefficient (`attack_coeff` / `release_coeff` in the filters: `1 / (ms * rate / 4000)`,
 *  never above 1) for a step that stands for `stride` samples: `1 - (1 - c)^stride`, which is `c` for one. */
function coeff(ms: number, rate: number, stride: number): number {
	const c = Math.min(1, 1 / ((ms * rate) / 4000));
	return stride === 1 ? c : 1 - (1 - c) ** stride;
}

/** `hermite_interpolation` of `af_acompressor.c`: the soft knee's curve. */
function hermite(x: number, x0: number, x1: number, p0: number, p1: number, m0: number, m1: number): number {
	const width = x1 - x0;
	const t = (x - x0) / width;
	const a = m0 * width;
	const b = m1 * width;
	const t2 = t * t;
	const t3 = t2 * t;
	return p0 * (2 * t3 - 3 * t2 + 1) + a * (t3 - 2 * t2 + t) + p1 * (-2 * t3 + 3 * t2) + b * (t3 - t2);
}

/** `sidechaincompress` on a key's power: the gain of the bus, per step. */
class Compressor {
	readonly #attack: number;
	readonly #release: number;
	readonly #thres = Math.log(COMPRESSOR.threshold);
	readonly #lowest = (COMPRESSOR.threshold / Math.sqrt(COMPRESSOR.knee)) ** 2;
	readonly #kneeStart = Math.log(COMPRESSOR.threshold / Math.sqrt(COMPRESSOR.knee));
	readonly #kneeStop = Math.log(COMPRESSOR.threshold * Math.sqrt(COMPRESSOR.knee));
	readonly #compressedStop: number;
	#slope = 0;

	constructor(rate: number, stride: number) {
		this.#attack = coeff(COMPRESSOR.attackMs, rate, stride);
		this.#release = coeff(COMPRESSOR.releaseMs, rate, stride);
		this.#compressedStop = (this.#kneeStop - this.#thres) / COMPRESSOR.ratio + this.#thres;
	}

	reset() {
		this.#slope = 0;
	}

	/** `power`: the key's detection value, its level squared (rms detection). */
	step(power: number): number {
		const s = (this.#slope += (power - this.#slope) * (power > this.#slope ? this.#attack : this.#release));
		if (!(s > 0) || !(s > this.#lowest)) return 1;
		const slope = Math.log(s) * 0.5;
		const delta = 1 / COMPRESSOR.ratio;
		const gain =
			slope < this.#kneeStop
				? hermite(slope, this.#kneeStart, this.#kneeStop, this.#kneeStart, this.#compressedStop, 1, delta)
				: this.#thres + (slope - this.#thres) * delta;
		return Math.exp(gain - slope);
	}
}

/** `sidechaingate` on a constant 1, the two low-passes and `1 - cut * max(fast, slow)`: the gain of the bus. */
class Gate {
	readonly #attack: number;
	readonly #release: number;
	readonly #fast: number;
	readonly #slow: number;
	readonly #thres = Math.log(GATE.threshold);
	readonly #limit = GATE.threshold * GATE.threshold;
	#slope = 0;
	#f = 0;
	#s = 0;

	constructor(rate: number, stride: number) {
		this.#attack = coeff(GATE.attackMs, rate, stride);
		this.#release = coeff(GATE.releaseMs, rate, stride);
		// `lowpass=f=F:p=1`: y = (1 - e^-w) x + e^-w y', w = 2 pi F / rate.
		this.#fast = Math.exp((-2 * Math.PI * GATE_SMOOTH_HZ.fast * stride) / rate);
		this.#slow = Math.exp((-2 * Math.PI * GATE_SMOOTH_HZ.slow * stride) / rate);
	}

	reset() {
		this.#slope = 0;
		this.#f = 0;
		this.#s = 0;
	}

	/** `power`: the key's squared level (rms detection), the 1e-6 floor in it already. */
	step(power: number, cut: number): number {
		const s = (this.#slope += (power - this.#slope) * (power > this.#slope ? this.#attack : this.#release));
		// Open from the threshold up, closed below it (the ratio of 9000 leaves a sliver between); a key whose
		// slope is still 0 — digital silence, nothing heard yet — holds it open.
		let open = 1;
		if (s > 0 && s < this.#limit) open = Math.max(GATE.range, Math.exp((Math.log(s) - 2 * this.#thres) * (GATE.ratio - 1)));
		this.#f = (1 - this.#fast) * open + this.#fast * this.#f;
		this.#s = (1 - this.#slow) * open + this.#slow * this.#s;
		return 1 - cut * Math.max(this.#f, this.#s);
	}
}

/** The export's duck on one bus: the gain a key puts on it. */
export class Ducker {
	readonly rate: number;
	readonly stride: number;
	#mode: DuckMode;
	/** How fast a depth change is followed, `0..1` per step (1: at once). A hand on the depth slider is heard as
	 *  a glide, not a step. */
	readonly #glide: number;
	#compressor: Compressor | null = null;
	#gate: Gate | null = null;
	/** The cut the gate is applying now, gliding toward the mode's. */
	#cut = 0;

	/** `rate` is the sample rate the key is at; `stride` the samples one `stepPower` stands for (1 for per-sample
	 *  `step`); `glideMs` the time a change of depth takes to be followed (0: immediately — the export has none). */
	constructor(rate: number, mode: DuckMode, stride = 1, glideMs = 0) {
		this.rate = rate;
		this.stride = stride;
		this.#glide = glideMs > 0 ? 1 - Math.exp(-(stride * 1000) / (rate * glideMs)) : 1;
		this.#mode = mode;
		this.#build();
	}

	get mode(): DuckMode {
		return this.#mode;
	}

	#build() {
		const m = this.#mode;
		if (m.kind === 'compressor') {
			this.#compressor = new Compressor(this.rate, this.stride);
			this.#gate = null;
		} else {
			this.#gate = new Gate(this.rate, this.stride);
			this.#cut = gateCut(m.depthDb);
			this.#compressor = null;
		}
	}

	/** Start over: nothing heard yet, no gain reduction — what the export starts a file with. */
	reset() {
		this.#compressor?.reset();
		this.#gate?.reset();
		if (this.#mode.kind === 'gate') this.#cut = gateCut(this.#mode.depthDb);
	}

	/** Change how it ducks. The same kind of mode keeps what it has heard (a new depth glides in); another kind starts afresh. */
	configure(mode: DuckMode) {
		const was = this.#mode;
		this.#mode = mode;
		if (was.kind !== mode.kind) this.#build();
	}

	/** One step on the detection value `power` of the key (its level squared, for the gate with the floor in it already). */
	stepPower(power: number): number {
		const m = this.#mode;
		if (m.kind === 'compressor') return this.#compressor!.step(power);
		const target = gateCut(m.depthDb);
		this.#cut += (target - this.#cut) * this.#glide;
		return this.#gate!.step(power, this.#cut);
	}

	/** One sample of the key, left and right: the gain to put on the ducked bus's sample. The compressor reads the key's
	 *  channels as `link=average` does (the mean of their magnitudes); the gate reads the mono the export folds it to
	 *  (`MONO_FOLD`) and its floor. */
	step(l: number, r: number): number {
		if (this.#mode.kind === 'compressor') {
			const a = (Math.abs(l) + Math.abs(r)) * 0.5;
			return this.stepPower(a * a);
		}
		const m = (l + r) * MONO_FOLD + GATE_KEY_FLOOR;
		return this.stepPower(m * m);
	}
}

/** The gain, per step, a key of `power` (mean square of the mono key per `hop` seconds) puts on a ducked bus: the same machine
 *  the preview plays, run over a power envelope — what the lane overlay draws. */
export function duckEnvelope(power: ArrayLike<number>, hop: number, mode: DuckMode, rate = 48000): Float32Array {
	const stride = Math.max(1, Math.round(hop * rate));
	const ducker = new Ducker(rate, mode, stride);
	const out = new Float32Array(power.length);
	// A power envelope has no sign to fold and no digital silence to lift: the floor is the power of the 1e-6 offset.
	const floor = mode.kind === 'gate' ? GATE_KEY_FLOOR * GATE_KEY_FLOOR : 0;
	for (let i = 0; i < power.length; i++) out[i] = ducker.stepPower(power[i] + floor);
	return out;
}

/** A gain as dB; -Infinity for a gain of 0 (the export's ducked bus never gets there). */
export const duckDb = (gain: number): number => (gain > 0 ? 20 * Math.log10(gain) : -Infinity);

/** How far down a gain is, in dB, positive (0 for unity or more). */
export const reductionDb = (gain: number): number => (gain >= 1 ? 0 : gain > 0 ? -20 * Math.log10(gain) : Infinity);
