/* The browser harness's stand-in for decoded audio. Outside the desktop app there is
 * no ffmpeg, so `get_audio` has nothing to decode; without a sound the harness's
 * playback is silent and the Mixer's meters and the faders have nothing to act on.
 * This synthesizes a mono signal — a voice-like, syllabic buzz — that is
 *
 *  - a pure function of the *source time*, so two windows of one asset join without
 *    a click, and a seek lands on the same sound it would have played;
 *  - different for every asset (pitch and phrasing follow a hash of its id);
 *  - as loud as the asset's analysis says it is: scaled so that, played as dual
 *    mono, it measures that integrated loudness (a mono signal on both channels is
 *    +3 LU over one channel; K-weighting is ~0 dB at the voice's pitch), with the
 *    crest factor a voice has.
 *
 * It is picture content for the harness, not interface, and nothing in the app
 * depends on it being anything but plausible. */

/** What the analysis says when it says nothing: a typical, unremarkable mix. */
export const FALLBACK_LUFS = -23;

/** BS.1770 reads a mono signal played on both channels 3 LU over one channel's power,
 *  less the K-weighting's 0.7 LU at 1 kHz: RMS dBFS ≈ LUFS − 2.3 for dual mono. */
export const LUFS_TO_RMS_DB = -2.3;

/** FNV-1a, so an asset id is a stable 32-bit number. */
function hash(text: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < text.length; i++) {
		h ^= text.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

/** A synthetic voice's constants, all from the asset id. */
interface Voice {
	f0: number;
	rate: number;
	phase: number;
	/** Linear scale that brings the voice's RMS to the target. */
	gain: number;
}

const HARMONICS = 7;
const TWO_PI = 2 * Math.PI;

/** One period of the voice's harmonic stack, `sin(kθ)/k` summed, tabulated: a
 *  120-second window is nearly four million samples and the harness synthesizes it
 *  on the main thread, so the stack is looked up rather than summed per sample. */
const TABLE_SIZE = 4096;
const TONE: Float64Array = (() => {
	const table = new Float64Array(TABLE_SIZE + 1);
	for (let i = 0; i <= TABLE_SIZE; i++) {
		let tone = 0;
		for (let k = 1; k <= HARMONICS; k++) tone += Math.sin((k * TWO_PI * i) / TABLE_SIZE) / k;
		table[i] = tone;
	}
	return table;
})();

/** The tabulated stack at phase `theta` (radians), linearly interpolated. */
function tone(theta: number): number {
	const x = (theta / TWO_PI - Math.floor(theta / TWO_PI)) * TABLE_SIZE;
	const i = Math.floor(x);
	return TONE[i] + (TONE[i + 1] - TONE[i]) * (x - i);
}

/** The syllable rhythm and phrase swell are slow, so they are evaluated on a 1 ms grid
 *  of *source time* (a function of absolute time, so windows still join). */
const ENV_RATE = 1000;

function envelope(v: Pick<Voice, 'rate' | 'phase'>, step: number): number {
	const t = step / ENV_RATE;
	// A syllable is a raised sine; the phrase lets the whole thing swell and rest.
	const s = Math.sin(Math.PI * v.rate * t + v.phase);
	const syllable = s * s;
	const phrase = 0.55 + 0.45 * Math.sin((TWO_PI * t) / 6.5 + v.phase);
	return (0.08 + 0.92 * syllable * Math.sqrt(syllable)) * phrase;
}

/** The pitch's vibrato: ±1.2% at 5.2 Hz, as a phase (so the pitch is exact, and the
 *  phase does not grow with `t` the way a frequency multiplied into it would). */
const VIB_HZ = 5.2;
const VIB_DEPTH = 0.012;

/** The unscaled signal for `count` samples from source time `start`, roughly ±1. */
function renderRaw(v: Pick<Voice, 'f0' | 'rate' | 'phase'>, start: number, count: number, sampleRate: number, out: ArrayLike<number> & { [i: number]: number }) {
	let step = Number.NaN;
	let env = 0;
	for (let i = 0; i < count; i++) {
		const t = start + i / sampleRate;
		// The epsilon keeps `10 + 8000/8000` and `11 + 0/8000` on the same side of a grid line.
		const s = Math.floor(t * ENV_RATE + 1e-6);
		if (s !== step) {
			step = s;
			env = envelope(v, s);
		}
		const theta = TWO_PI * v.f0 * (t + (VIB_DEPTH / (TWO_PI * VIB_HZ)) * Math.sin(TWO_PI * VIB_HZ * t)) + v.phase;
		out[i] = tone(theta) * env;
	}
}

const voices = new Map<string, Voice>();

/** The voice for `key` at integrated loudness `lufs`: its constants, with the gain that
 *  makes its RMS the target, measured over a whole number of phrases. */
function voiceFor(key: string, lufs: number): Voice {
	const id = `${key}|${lufs}`;
	let v = voices.get(id);
	if (v) return v;
	const h = hash(key);
	const base: Pick<Voice, 'f0' | 'rate' | 'phase'> = {
		f0: 96 + (h % 97), // 96..192 Hz: a speaking voice
		rate: 3.4 + ((h >>> 8) % 20) / 10, // 3.4..5.3 syllables/s
		phase: (((h >>> 16) % 628) / 100) % (2 * Math.PI)
	};
	// Calibrate over 26 s = four 6.5 s phrases, at a rate that resolves the harmonics.
	const rate = 8000;
	const n = 26 * rate;
	const block = new Float64Array(n);
	renderRaw(base, 0, n, rate, block);
	let sum = 0;
	for (let i = 0; i < n; i++) sum += block[i] * block[i];
	const rms = Math.sqrt(sum / n) || 1;
	const target = 10 ** ((lufs + LUFS_TO_RMS_DB) / 20);
	v = { ...base, gain: target / rms };
	voices.set(id, v);
	return v;
}

/**
 * `duration` seconds of mono 16-bit PCM starting at source time `start`, at
 * `sampleRate`, for the asset `key` at integrated loudness `lufs` (default −23).
 * A sample that would pass full scale is clipped, as a hot file would be.
 */
export function synthPcm(key: string, start: number, duration: number, sampleRate: number, lufs?: number | null): Int16Array {
	const n = Math.max(0, Math.floor(duration * sampleRate));
	const v = voiceFor(key, lufs ?? FALLBACK_LUFS);
	const out = new Int16Array(n);
	const block = new Float64Array(Math.min(n, 65536));
	for (let at = 0; at < n; at += block.length) {
		const count = Math.min(block.length, n - at);
		renderRaw(v, start + at / sampleRate, count, sampleRate, block);
		for (let i = 0; i < count; i++) {
			out[at + i] = Math.round(Math.max(-1, Math.min(1, block[i] * v.gain)) * 32767);
		}
	}
	return out;
}
