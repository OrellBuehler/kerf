/* Web Audio preview playback. Decodes clip audio windows to raw PCM via the
   backend, schedules every audio-bearing clip against the timeline with its
   volume / fades / speed applied, and exposes the audio clock so the playhead
   can follow it sample-accurately.

   The graph is a console in miniature, so the Mixer panel can both *move* it live
   and *read* it:

     clip source -> clip gain (volume, fades) -> track bus
     track bus: fader -> left / right pan legs -> merger -> [split -> 2 analysers]
     every bus -> master gain -> [limiter] -> master out
     master out -> speakers, and -> [split -> 2 analysers]

   The track fader and the pan sit on the bus, not folded into each clip's gain:
   the product is the same number (`audio-mix.ts` pins that), and a bus is what a
   fader can move while it is dragged — `liveTrack` / `liveMaster` — without
   re-fetching or re-scheduling anything. `meters()` reads the analysers' latest
   block, which is what the Mixer's meters draw: real measured levels of what is
   playing, sample peak and RMS per channel, per track and on the master.

   Ducking is **not** here. The export ducks a track with a sidechain compressor
   keyed off the rest of the mix; Web Audio's compressor has no sidechain input
   without an AudioWorklet, so the preview plays a ducked track at its fader and the
   Mixer says so on the toggle.

   The master limiter is the export's own: `alimiter`, ported sample for sample
   (`alimiter.ts`) and run in an AudioWorklet (`limiter-worklet.ts`). Its 5 ms
   lookahead makes what is heard that much later, which `outputLatency()` adds while
   it is in the path. The module loads once per context; until it has (and for good
   where a webview cannot run it) the limiter is the compressor approximation
   (`limiterParams`).

   Per-clip effect chains *are* auralized: the backend decodes each window
   through the clip's own ffmpeg chain, so an EQ or compressor is heard rather
   than guessed at. They run before this file's gain envelope where the export
   runs them after the clip gain — audible only to a level-dependent effect, and
   keeping volume here is what lets the fader stay live instead of re-fetching
   PCM on every drag.

   Reverse shuttle is audible too: a negative rate plays each clip's sound
   backwards from where the playhead meets it — its end, or the playhead itself
   inside it — at the shuttle's speed, from a buffer stored the other way round
   (`#buffer`'s `backwards`), with the same gain envelope met in reverse
   (`gainAutomationBack`). Like the forward shuttle it is varispeed: 2× is an
   octave up. */

import { getAudio } from './api';
import type { Clip, MasterBus, Timeline } from './types';
import { along, clipDuration, clipRamp, rateAutomation, sourceDuration, speedMag } from './speed-ramp';
import { panGains } from './mixer';
import {
	clipGainAt,
	crossfadePair,
	fadeInTimes,
	fadeOutOf,
	gainAutomation,
	gainAutomationBack,
	type GainClip,
	limiterParams,
	STEP_LEAD
} from './audio-mix';
import { clipSounds } from './mixer-strips';
import { clampMasterVolume, masterOf, trackRenders } from './levels';
import { readBlock, type Reading } from './meter';
import { toast } from './notifications.svelte';
import { outputLatency } from './output-latency';
import { describeError, logFrontend } from './log';
import type { LimiterKind } from './master-fx';
import { alimiterDelay, LIMITER_PROCESSOR, limiterLimit, type LimiterMessage } from './alimiter';

/** Preview decode rate: mono 32 kHz keeps a minute of PCM under 4 MB on the
 *  wire while staying honest enough to judge a cut. */
const RATE = 32000;
/** Longest audio window fetched per clip, in source seconds — bounds memory on
 *  very long clips (playback inside one clip goes silent past this; reverse
 *  playback, which enters a clip at its end, holds the window there). */
const MAX_WINDOW = 600;
/** Total decoded source seconds kept in the buffer cache before eviction. */
const CACHE_CAP = 1800;
/** The analysers' window, samples: ~43 ms at 48 kHz, so a meter read every frame of a
 *  24 fps-or-better display sees every sample. */
const METER_FFT = 2048;
/** Time constant (s) of a live fader / pan move — short enough to follow a hand,
 *  long enough not to zipper. */
const LIVE_TAU = 0.012;

/** What a meter reads for one strip: a block of each channel. */
export interface StereoReading {
	l: Reading;
	r: Reading;
}

/** Everything the Mixer's meters read this frame. */
export interface MeterFrame {
	master: StereoReading;
	/** By track id — only tracks that have audio playing through a bus. */
	tracks: Map<string, StereoReading>;
}

/** A pair of analysers on one stereo signal, with their read buffers. */
interface Tap {
	left: AnalyserNode;
	right: AnalyserNode;
	bufL: Float32Array<ArrayBuffer>;
	bufR: Float32Array<ArrayBuffer>;
}

/** One track's bus: its fader, its pan legs and its meter. */
interface Bus {
	input: GainNode;
	left: GainNode;
	right: GainNode;
	merger: ChannelMergerNode;
	split: ChannelSplitterNode;
	tap: Tap;
}

/** The limiter worklet's node, as far as the engine uses it. */
export type LimiterNode = AudioNode & {
	port: { postMessage(message: LimiterMessage): void };
	onprocessorerror?: ((e: ErrorEvent) => void) | null;
};

/** Which limiter is in the master's path. */
type Limiting = 'worklet' | 'compressor' | null;

/** The master bus: gain, the limiter, a final unity stage and the meter. */
interface Master {
	input: GainNode;
	/** The approximation: a compressor and the trim cancelling its makeup. */
	comp: DynamicsCompressorNode;
	trim: GainNode;
	/** The export's limiter, once its worklet has loaded. */
	worklet: LimiterNode | null;
	/** The limit last posted to the worklet, so a fader move that does not change it posts nothing. */
	posted: number | null;
	out: GainNode;
	split: ChannelSplitterNode;
	tap: Tap;
	/** How the nodes are wired now, so a toggle only reconnects when it changes. */
	limiting: Limiting;
	/** The master last applied, which a worklet loading mid-play takes over with. */
	applied: MasterBus | null;
}

type Session = {
	anchorTime: number; // timeline seconds when playback started
	anchorCtx: number; // AudioContext.currentTime when playback started
	rate: number;
	nodes: { src: AudioBufferSourceNode; gain: GainNode }[];
	buses: Map<string, Bus>;
};

/** What the engine needs from its surroundings — the browser's, by default. */
export interface AudioEnv {
	createContext: () => AudioContext;
	fetchPcm: typeof getAudio;
	/** Tell the user a clip's preview audio could not be decoded. */
	report: (message: string) => void;
	/** The limiter worklet's node in `ctx`, or `null` where it cannot run. */
	createLimiter: (ctx: AudioContext, limit: number) => Promise<LimiterNode | null>;
	/** Say why the preview fell back to the limiter's approximation (the console and the app's log). */
	warn: (message: string) => void;
}

/** The limiter worklet: its module (bundled, served from the app's own origin) and a stereo node. */
async function createLimiter(ctx: AudioContext, limit: number): Promise<LimiterNode | null> {
	if (!ctx.audioWorklet || typeof AudioWorkletNode === 'undefined') return null;
	const { default: url } = await import('./limiter-worklet-url');
	await ctx.audioWorklet.addModule(url);
	return new AudioWorkletNode(ctx, LIMITER_PROCESSOR, {
		numberOfInputs: 1,
		numberOfOutputs: 1,
		outputChannelCount: [2],
		channelCount: 2,
		channelCountMode: 'explicit',
		channelInterpretation: 'speakers',
		processorOptions: { limit } satisfies LimiterMessage
	});
}

/** A decoded window, and which way round it is stored. */
interface Oriented {
	buf: AudioBuffer;
	rev: boolean;
}

export class AudioEngine {
	#ctx: AudioContext | null = null;
	#master: Master | null = null;
	#cache = new Map<string, Oriented | Promise<Oriented | null>>();
	#session: Session | null = null;
	#env: AudioEnv;
	/** Cache keys that already reported a decode failure this session — a
	 *  clip's window is retried on every resync, and nobody needs to hear about
	 *  the same broken clip twice. */
	#warned = new Set<string>();
	/** Which limiter the preview plays when the master's is on (`limiterNote` words it). */
	#kind: LimiterKind = 'pending';
	#kindListeners = new Set<(kind: LimiterKind) => void>();

	constructor(env: Partial<AudioEnv> = {}) {
		this.#env = {
			createContext: env.createContext ?? (() => new AudioContext()),
			fetchPcm: env.fetchPcm ?? getAudio,
			report: env.report ?? ((m) => void toast.error(m)),
			createLimiter: env.createLimiter ?? createLimiter,
			warn:
				env.warn ??
				((m) => {
					console.warn(m);
					logFrontend('warn', m, 'preview limiter');
				})
		};
	}

	/** Which limiter plays when the master's is on: `pending` until playback has loaded the worklet,
	 *  `exact` (the export's own) once it has, `approximate` (the compressor) where it cannot run. */
	limiterKind(): LimiterKind {
		return this.#kind;
	}

	/** Be told when `limiterKind()` changes. Returns the way to stop. */
	onLimiterKind(listener: (kind: LimiterKind) => void): () => void {
		this.#kindListeners.add(listener);
		return () => void this.#kindListeners.delete(listener);
	}

	#setKind(kind: LimiterKind) {
		if (kind === this.#kind) return;
		this.#kind = kind;
		for (const listener of this.#kindListeners) listener(kind);
	}

	/** Seconds between the audio clock and the sound coming out of the speakers
	 *  ([`outputLatency`](./output-latency.ts)), plus the limiter's lookahead while the worklet is in
	 *  the path: the picture of GPU playback is asked for the clock *less* this, so it is not ahead
	 *  of what is heard. 0 until a context exists. */
	outputLatency(): number {
		const ctx = this.#ctx;
		const device = outputLatency(ctx);
		return ctx && this.#master?.limiting === 'worklet' ? device + alimiterDelay(ctx.sampleRate) : device;
	}

	/** Current timeline time by the audio clock, or `null` when not playing. */
	clock(): number | null {
		if (!this.#session || !this.#ctx) return null;
		const s = this.#session;
		return s.anchorTime + (this.#ctx.currentTime - s.anchorCtx) * s.rate;
	}

	/** (Re)start playback of every audio-bearing clip from timeline time `t` at `rate` — negative
	 *  plays backwards (reverse shuttle), and the clock then counts down. */
	start(timeline: Timeline, audioAssets: ReadonlySet<string>, t: number, rate: number) {
		this.stop();
		if (rate === 0 || !Number.isFinite(rate)) return;
		const back = rate < 0;
		const ctx = (this.#ctx ??= this.#env.createContext());
		if (ctx.state === 'suspended') void ctx.resume();
		const session: Session = { anchorTime: t, anchorCtx: ctx.currentTime, rate, nodes: [], buses: new Map() };
		this.#session = session;
		const m = this.#masterBus(ctx);
		// What the limiter held from before (its lookahead, a gain reduction) is not this run's.
		if (!this.#applyMaster(m, masterOf(timeline)) && m.limiting === 'worklet') m.worklet!.port.postMessage({ reset: true });
		for (const track of timeline.tracks) {
			// `Timeline::track_renders` in kerf-core, so what is heard matches what would
			// export: a muted track is silent, and while any track of a kind is soloed only
			// the soloed ones of that kind play.
			if (!trackRenders(timeline, track)) continue;
			const playing = track.clips.filter((clip) => {
				if (clip.enabled === false) return false;
				// A picture whose sound was detached is silent — its audio clip plays it
				// (`clip_sounds` in the export graph): scheduling both is the doubling.
				if (!clipSounds(clip, audioAssets)) return false;
				// Still ahead of the playhead in the direction it runs.
				if (back) return clip.timeline_start < t;
				return clip.timeline_start + clipDuration(clip) > t;
			});
			if (playing.length === 0) continue;
			const bus = this.#makeBus(ctx, track.volume ?? 1, track.pan ?? 0);
			session.buses.set(track.id, bus);
			// Asked for in the order the playhead meets them: backwards, the nearest is the latest.
			if (back) playing.sort((a, b) => b.timeline_start - a.timeline_start);
			for (const clip of playing) {
				void this.#buffer(clip, back)
					.then((buf) => {
						// Buffers resolve async; only schedule into the session that asked.
						if (buf && this.#session === session) this.#schedule(clip, buf, session, bus, crossfadePair(track.clips, clip));
					})
					.catch(() => {
						// #buffer() already evicted the cache entry and reported the
						// failure once; this just keeps the rejection from going
						// unhandled (start()/resync() fire on every edit while playing).
					});
			}
		}
	}

	stop() {
		const s = this.#session;
		this.#session = null;
		if (!s) return;
		for (const { src, gain } of s.nodes) {
			try {
				src.stop();
			} catch {
				/* never started or already ended */
			}
			src.disconnect();
			gain.disconnect();
		}
		for (const bus of s.buses.values()) {
			bus.input.disconnect();
			bus.left.disconnect();
			bus.right.disconnect();
			bus.merger.disconnect();
			bus.split.disconnect();
		}
	}

	// ---- the console ---------------------------------------------------------

	/** Two analysers on the two channels of `source`, which must carry a stereo signal. */
	#tap(ctx: AudioContext, source: AudioNode): { split: ChannelSplitterNode; tap: Tap } {
		const split = ctx.createChannelSplitter(2);
		const left = ctx.createAnalyser();
		const right = ctx.createAnalyser();
		left.fftSize = right.fftSize = METER_FFT;
		source.connect(split);
		split.connect(left, 0);
		split.connect(right, 1);
		return {
			split,
			tap: { left, right, bufL: new Float32Array(METER_FFT), bufR: new Float32Array(METER_FFT) }
		};
	}

	/** A track's bus: the fader, the pan as the export's balance (one gain per side into a
	 *  merger — `get_audio` hands back mono, so the two legs *are* the stereo pair, and a
	 *  StereoPannerNode's constant-power law would quietly disagree with the file), a meter,
	 *  and onward to the master. */
	#makeBus(ctx: AudioContext, volume: number, pan: number): Bus {
		const master = this.#masterBus(ctx);
		const input = ctx.createGain();
		input.gain.value = volume;
		const left = ctx.createGain();
		const right = ctx.createGain();
		const [gl, gr] = panGains(pan);
		left.gain.value = gl;
		right.gain.value = gr;
		const merger = ctx.createChannelMerger(2);
		input.connect(left);
		input.connect(right);
		left.connect(merger, 0, 0);
		right.connect(merger, 0, 1);
		merger.connect(master.input);
		const { split, tap } = this.#tap(ctx, merger);
		return { input, left, right, merger, split, tap };
	}

	/** The master bus, built once per context and kept across restarts. */
	#masterBus(ctx: AudioContext): Master {
		if (this.#master) return this.#master;
		const input = ctx.createGain();
		const comp = ctx.createDynamicsCompressor();
		const trim = ctx.createGain();
		const out = ctx.createGain();
		out.connect(ctx.destination);
		input.connect(out);
		const { split, tap } = this.#tap(ctx, out);
		const m: Master = { input, comp, trim, worklet: null, posted: null, out, split, tap, limiting: null, applied: null };
		void this.#loadLimiter(ctx, m);
		return (this.#master = m);
	}

	/** Load the limiter worklet and put it in the compressor's place, wired at once when the limiter
	 *  is on. A webview that cannot (no AudioWorklet, a module refused) keeps the compressor. */
	async #loadLimiter(ctx: AudioContext, m: Master) {
		let node: LimiterNode | null;
		try {
			node = await this.#env.createLimiter(ctx, limiterLimit(m.applied?.ceiling_db ?? Number.NaN));
		} catch (e) {
			this.#env.warn(`The preview's limiter is an approximation: its worklet did not load (${describeError(e)}).`);
			this.#setKind('approximate');
			return;
		}
		if (this.#master !== m) return;
		if (!node) {
			this.#env.warn("The preview's limiter is an approximation: this webview has no AudioWorklet.");
			this.#setKind('approximate');
			return;
		}
		// A processor that throws stops making sound for good: the compressor takes over rather than
		// leaving the master silent.
		node.onprocessorerror = () => this.#limiterFailed(m, node);
		m.worklet = node;
		this.#setKind('exact');
		if (m.applied) this.#applyMaster(m, m.applied);
	}

	#limiterFailed(m: Master, node: LimiterNode) {
		if (m.worklet !== node) return;
		this.#env.warn("The preview's limiter is an approximation from now on: its worklet stopped with an error.");
		node.disconnect();
		m.worklet = null;
		m.posted = null;
		this.#setKind('approximate');
		if (m.applied) this.#applyMaster(m, m.applied);
	}

	/** Set the master from the timeline's: its fader, and the limiter — wired in only while it is
	 *  on, set to its ceiling: the worklet once it has loaded, the compressor until then. Returns whether
	 *  the limiter was rewired (a worklet put in the path starts over: `reset`). */
	#applyMaster(m: Master, master: MasterBus, live = false): boolean {
		const ctx = this.#ctx!;
		m.applied = master;
		const volume = clampMasterVolume(master.volume);
		if (live) m.input.gain.setTargetAtTime(volume, ctx.currentTime, LIVE_TAU);
		else m.input.gain.value = volume;
		const limiting: Limiting = !master.limiter ? null : m.worklet ? 'worklet' : 'compressor';
		const limit = limiterLimit(master.ceiling_db);
		if (limiting === 'worklet' && m.limiting === 'worklet' && limit !== m.posted) {
			m.worklet!.port.postMessage({ limit });
			m.posted = limit;
		}
		if (limiting === 'compressor') {
			const p = limiterParams(master.ceiling_db);
			m.comp.threshold.value = p.threshold;
			m.comp.knee.value = p.knee;
			m.comp.ratio.value = p.ratio;
			m.comp.attack.value = p.attack;
			m.comp.release.value = p.release;
			m.trim.gain.value = p.trim;
		}
		if (m.limiting === limiting) return false;
		m.input.disconnect();
		m.comp.disconnect();
		m.trim.disconnect();
		m.worklet?.disconnect();
		if (limiting === 'worklet') {
			m.worklet!.port.postMessage({ limit, reset: true });
			m.posted = limit;
			m.input.connect(m.worklet!);
			m.worklet!.connect(m.out);
		} else if (limiting === 'compressor') {
			m.input.connect(m.comp);
			m.comp.connect(m.trim);
			m.trim.connect(m.out);
		} else {
			m.input.connect(m.out);
		}
		m.limiting = limiting;
		return true;
	}

	/** Move a track's fader and / or pan *now*, while a gesture is still in progress and
	 *  nothing has been written: the sound follows the hand. A no-op for a track that is
	 *  not playing. */
	liveTrack(trackId: string, mix: { volume?: number; pan?: number }) {
		const bus = this.#session?.buses.get(trackId);
		const ctx = this.#ctx;
		if (!bus || !ctx) return;
		const now = ctx.currentTime;
		if (mix.volume !== undefined) bus.input.gain.setTargetAtTime(Math.max(0, mix.volume), now, LIVE_TAU);
		if (mix.pan !== undefined) {
			const [gl, gr] = panGains(mix.pan);
			bus.left.gain.setTargetAtTime(gl, now, LIVE_TAU);
			bus.right.gain.setTargetAtTime(gr, now, LIVE_TAU);
		}
	}

	/** The same for the master: fader, limiter on / off, ceiling. */
	liveMaster(master: MasterBus) {
		if (!this.#master || !this.#ctx) return;
		this.#applyMaster(this.#master, master, true);
	}

	/** Put every live-moved control back to what the timeline says — a gesture that was
	 *  abandoned. */
	restoreMix(timeline: Timeline) {
		const s = this.#session;
		if (!s) return;
		for (const track of timeline.tracks) this.liveTrack(track.id, { volume: track.volume ?? 1, pan: track.pan ?? 0 });
		this.liveMaster(masterOf(timeline));
	}

	/** What every meter reads right now — the latest block through each analyser — or
	 *  `null` when nothing is playing. */
	meters(): MeterFrame | null {
		const s = this.#session;
		const m = this.#master;
		if (!s || !m) return null;
		const tracks = new Map<string, StereoReading>();
		for (const [id, bus] of s.buses) tracks.set(id, readTap(bus.tap));
		return { master: readTap(m.tap), tracks };
	}

	#schedule(clip: Clip, buf: AudioBuffer, s: Session, bus: Bus, powerPair = false) {
		const ctx = this.#ctx!;
		const mag = speedMag(clip);
		const keys = clipRamp(clip);
		const dur = clipDuration(clip);
		const clipStart = clip.timeline_start;
		const clipEnd = clipStart + dur;
		const now = ctx.currentTime;
		const nowT = s.anchorTime + (now - s.anchorCtx) * s.rate;
		const back = s.rate < 0;
		if (back ? clipStart >= nowT : clipEnd <= nowT) return;

		const src = ctx.createBufferSource();
		src.buffer = buf;
		if (!keys) src.playbackRate.value = mag * Math.abs(s.rate);
		const gain = ctx.createGain();
		src.connect(gain);
		gain.connect(bus.input);
		s.nodes.push({ src, gain });

		// Timeline time -> context time under this session's anchor and rate (a negative rate
		// meets the later times first).
		const at = (tl: number) => s.anchorCtx + (tl - s.anchorTime) / s.rate;
		// Where the buffer begins on the timeline: the clip's start, or — played backwards, from a
		// buffer stored the other way round — its end.
		const entry = back ? clipEnd : clipStart;
		const inside = back ? clipEnd > nowT : clipStart < nowT;
		const into = inside ? Math.abs(nowT - entry) : 0;
		// A ramp is run as the player's rate: the source's position is the area under the speed curve.
		const offsetSrc = !inside ? 0 : keys ? (back ? sourceDuration(clip) - along(clip, dur - into) : along(clip, into)) : into * mag;
		if (offsetSrc >= buf.duration) return;
		const startAt = Math.max(at(entry), now);
		src.start(startAt, offsetSrc, buf.duration - offsetSrc);
		if (keys) {
			const rate = src.playbackRate;
			const k = Math.abs(s.rate);
			const [from, to] = back ? [0, dur - into] : [into, dur];
			const [first, ...rest] = rateAutomation(keys, from, to, back);
			rate.setValueAtTime(first.speed * k, startAt);
			for (const p of rest) {
				const when = at(clipStart + p.time);
				if (p.step) rate.setValueAtTime(p.speed * k, when);
				else rate.linearRampToValueAtTime(p.speed * k, when);
			}
		}

		// Gain envelope: clip volume shaped by fade-in/out. A transition
		// approximates as an extra fade-in (the export folds it in the same way).
		// The track fader multiplies the clip's own gain, as it does on export — on the
		// bus, which `clipGainAt` leaves out (its product with the clip's is the same).
		const shaped: GainClip = powerPair ? { ...clip, power_pair: true } : clip;
		const fo = fadeOutOf(clip);
		const env = (tl: number) => clipGainAt(shaped, tl, clipStart, clipEnd);
		if (back) {
			// Backwards the envelope is met from the end down: the gain just below the playhead,
			// then every point of it in the order it is reached.
			const t0 = Math.min(clipEnd, nowT);
			gain.gain.setValueAtTime(env(t0 - STEP_LEAD), Math.max(at(t0), now));
			for (const p of gainAutomationBack(shaped, clipStart, clipEnd, t0)) {
				if (p.jump) gain.gain.setValueAtTime(p.value, at(p.time));
				else gain.gain.linearRampToValueAtTime(p.value, at(p.time));
			}
			return;
		}
		const t0 = Math.max(clipStart, nowT);
		gain.gain.setValueAtTime(env(t0), Math.max(at(t0), now));
		const keyed = gainAutomation(shaped, clipStart, clipEnd, t0);
		if (keyed.length) {
			// A keyed volume: ramp through the curve's points and the fade edges, and jump where
			// the curve steps (a hold) instead of ramping across the whole segment.
			for (const p of keyed) {
				if (p.jump) gain.gain.setValueAtTime(p.value, at(p.time));
				else gain.gain.linearRampToValueAtTime(p.value, at(p.time));
			}
			return;
		}
		// A constant-power fade-in is drawn as a run of short ramps along its curve.
		for (const t of fadeInTimes(shaped, clipStart).filter((t) => t > t0)) gain.gain.linearRampToValueAtTime(env(t), at(t));
		const foStart = Math.max(clipEnd - fo, t0);
		if (fo > 0 && clipEnd > t0) {
			gain.gain.setValueAtTime(env(foStart), at(foStart));
			gain.gain.linearRampToValueAtTime(0, at(clipEnd));
		}
	}

	/** Fetch + decode the clip's source window, cached, stored in the order the playhead meets it:
	 *  `backwards` (reverse shuttle) from the clip's end. A reversed clip's samples are stored
	 *  back-to-front, and played backwards they are front-to-back again, so scheduling stays
	 *  forward-only either way. One window is held one way round at a time: asked for the other,
	 *  the cached copy is turned in memory and replaces it, rather than fetched and held twice. */
	async #buffer(clip: Clip, backwards = false): Promise<AudioBuffer | null> {
		const rev = ((clip.speed ?? 1) < 0) !== backwards;
		// Cap the window at MAX_WINDOW source seconds where the playhead enters the clip: a
		// reversed clip plays from source_out downward, and so does a forward one played
		// backwards, so their window is anchored at the top instead.
		const sin = rev ? Math.max(clip.source_in, clip.source_out - MAX_WINDOW) : clip.source_in;
		const sout = rev ? clip.source_out : Math.min(clip.source_out, clip.source_in + MAX_WINDOW);
		if (sout - sin <= 0) return null;
		const samples = Math.round((sout - sin) * RATE);
		// The effect chain is baked into the decode, so it is part of the identity
		// of the cached buffer: retuning an EQ has to re-fetch rather than keep
		// playing the previous sound.
		const fx = clip.audio?.length ? JSON.stringify(clip.audio) : '';
		const key = `${clip.asset_id}:${sin.toFixed(3)}:${sout.toFixed(3)}:${fx}`;
		const hit = this.#cache.get(key);
		if (hit) {
			const held = await hit;
			if (!held) return null;
			if (held.rev === rev) return held.buf;
			const turned = this.#turned(held, samples);
			if (turned) this.#cache.set(key, turned);
			return turned?.buf ?? null;
		}

		const pending = (async (): Promise<Oriented | null> => {
			const bytes = await this.#env.fetchPcm(clip.asset_id, sin, sout - sin, RATE, clip.id);
			if (!bytes || bytes.byteLength < 2) return null;
			const i16 = new Int16Array(bytes, 0, Math.floor(bytes.byteLength / 2));
			return this.#decoded(i16, rev, samples);
		})();
		this.#cache.set(key, pending);
		try {
			const held = await pending;
			if (held) {
				this.#cache.set(key, held);
				this.#evict();
			} else {
				this.#cache.delete(key);
			}
			return held?.buf ?? null;
		} catch (e) {
			// A rejected decode must not squat on the cache forever — a moved
			// file or a transient ffmpeg error would otherwise leave this clip
			// silently muted for the rest of the session. Delete it so the next
			// resync retries, and say so once (this fires on every resync while
			// playing, and nobody needs to hear about the same broken clip twice).
			this.#cache.delete(key);
			if (!this.#warned.has(key)) {
				this.#warned.add(key);
				this.#env.report(`Couldn't decode preview audio for a clip — ${e instanceof Error ? e.message : String(e)}`);
			}
			throw e;
		}
	}

	/** A decoded window (`pcm`, in source order) as a buffer the given way round, written straight
	 *  into the buffer. Back-to-front it is laid out over the window's own length (`samples`): the
	 *  decode ends early when the source's sound does, and the buffer's first sample has to be the
	 *  window's end or everything after it plays late. */
	#decoded(pcm: Int16Array, rev: boolean, samples: number): Oriented | null {
		const length = rev ? samples : pcm.length;
		const buf = this.#createBuffer(length);
		if (!buf) return null;
		const ch = buf.getChannelData(0);
		const n = Math.min(pcm.length, length);
		if (rev) for (let i = 0; i < n; i++) ch[length - 1 - i] = pcm[i] / 32768;
		else for (let i = 0; i < n; i++) ch[i] = pcm[i] / 32768;
		return { buf, rev };
	}

	/** A held window the other way round, written straight from its samples. Back-to-front it is laid
	 *  out over the window's own length, as `#decoded` does; front-to-back it is the held one's whole
	 *  length, its padding (silence at the window's end) included. */
	#turned(held: Oriented, samples: number): Oriented | null {
		const from = held.buf.getChannelData(0);
		const rev = !held.rev;
		const length = rev ? samples : from.length;
		const buf = this.#createBuffer(length);
		if (!buf) return null;
		const ch = buf.getChannelData(0);
		if (rev) for (let i = 0, n = Math.min(from.length, length); i < n; i++) ch[length - 1 - i] = from[i];
		else for (let i = 0, last = from.length - 1; i < length; i++) ch[i] = from[last - i];
		return { buf, rev };
	}

	#createBuffer(length: number): AudioBuffer | null {
		const ctx = this.#ctx;
		return ctx && length > 0 ? ctx.createBuffer(1, length, RATE) : null;
	}

	#evict() {
		let total = 0;
		for (const v of this.#cache.values()) if (!(v instanceof Promise)) total += v.buf.duration;
		for (const [k, v] of this.#cache) {
			if (total <= CACHE_CAP) break;
			if (!(v instanceof Promise)) {
				this.#cache.delete(k);
				total -= v.buf.duration;
			}
		}
	}
}

export const audio = new AudioEngine();

/** The latest block of a tap, read as peak and RMS per channel. */
function readTap(tap: Tap): StereoReading {
	tap.left.getFloatTimeDomainData(tap.bufL);
	tap.right.getFloatTimeDomainData(tap.bufR);
	return { l: readBlock(tap.bufL), r: readBlock(tap.bufR) };
}
