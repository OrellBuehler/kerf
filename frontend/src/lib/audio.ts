/* Web Audio preview playback. Decodes clip audio windows to raw PCM via the
   backend, schedules every audio-bearing clip against the timeline with its
   volume / fades / speed applied, and exposes the audio clock so the playhead
   can follow it sample-accurately.

   The graph is a console in miniature, so the Mixer panel can both *move* it live
   and *read* it:

     clip source -> clip gain (volume, fades) -> track bus
     track bus: fader -> left / right pan legs -> merger -> [split -> 2 analysers]
     every bus -> master gain -> [limiter approximation] -> master out
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
   Mixer says so on the toggle. The master limiter is an approximation too
   (`limiterParams`), and the Mixer says that as well.

   Per-clip effect chains *are* auralized: the backend decodes each window
   through the clip's own ffmpeg chain, so an EQ or compressor is heard rather
   than guessed at. They run before this file's gain envelope where the export
   runs them after the clip gain — audible only to a level-dependent effect, and
   keeping volume here is what lets the fader stay live instead of re-fetching
   PCM on every drag. Reverse shuttle is still silent: this is a preview monitor,
   not the export mix. */

import { getAudio } from './api';
import type { Clip, MasterBus, Timeline } from './types';
import { panGains } from './mixer';
import { clipGainAt, fadeInOf, fadeOutOf, limiterParams } from './audio-mix';
import { clipSounds } from './mixer-strips';
import { clampMasterVolume, masterOf, trackRenders } from './levels';
import { readBlock, type Reading } from './meter';
import { toast } from './notifications.svelte';

/** Preview decode rate: mono 32 kHz keeps a minute of PCM under 4 MB on the
 *  wire while staying honest enough to judge a cut. */
const RATE = 32000;
/** Longest audio window fetched per clip, in source seconds — bounds memory on
 *  very long clips (playback inside one clip goes silent past this). */
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

/** The master bus: gain, the limiter approximation, a final unity stage and the meter. */
interface Master {
	input: GainNode;
	comp: DynamicsCompressorNode;
	trim: GainNode;
	out: GainNode;
	split: ChannelSplitterNode;
	tap: Tap;
	/** How the nodes are wired now, so a toggle only reconnects when it changes. */
	limiting: boolean;
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
}

function speedMag(clip: Clip): number {
	return Math.max(Math.abs(clip.speed ?? 1), 0.01);
}

export class AudioEngine {
	#ctx: AudioContext | null = null;
	#master: Master | null = null;
	#cache = new Map<string, AudioBuffer | Promise<AudioBuffer | null>>();
	#session: Session | null = null;
	#env: AudioEnv;
	/** Cache keys that already reported a decode failure this session — a
	 *  clip's window is retried on every resync, and nobody needs to hear about
	 *  the same broken clip twice. */
	#warned = new Set<string>();

	constructor(env: Partial<AudioEnv> = {}) {
		this.#env = {
			createContext: env.createContext ?? (() => new AudioContext()),
			fetchPcm: env.fetchPcm ?? getAudio,
			report: env.report ?? ((m) => void toast.error(m))
		};
	}

	/** Current timeline time by the audio clock, or `null` when not playing. */
	clock(): number | null {
		if (!this.#session || !this.#ctx) return null;
		const s = this.#session;
		return s.anchorTime + (this.#ctx.currentTime - s.anchorCtx) * s.rate;
	}

	/** (Re)start playback of every audio-bearing clip from timeline time `t`.
	 *  Reverse rates stop audio — the playhead falls back to wall-clock. */
	start(timeline: Timeline, audioAssets: ReadonlySet<string>, t: number, rate: number) {
		this.stop();
		if (rate <= 0) return;
		const ctx = (this.#ctx ??= this.#env.createContext());
		if (ctx.state === 'suspended') void ctx.resume();
		const session: Session = { anchorTime: t, anchorCtx: ctx.currentTime, rate, nodes: [], buses: new Map() };
		this.#session = session;
		this.#applyMaster(this.#masterBus(ctx), masterOf(timeline));
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
				return clip.timeline_start + (clip.source_out - clip.source_in) / speedMag(clip) > t;
			});
			if (playing.length === 0) continue;
			const bus = this.#makeBus(ctx, track.volume ?? 1, track.pan ?? 0);
			session.buses.set(track.id, bus);
			for (const clip of playing) {
				void this.#buffer(clip)
					.then((buf) => {
						// Buffers resolve async; only schedule into the session that asked.
						if (buf && this.#session === session) this.#schedule(clip, buf, session, bus);
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
		return (this.#master = { input, comp, trim, out, split, tap, limiting: false });
	}

	/** Set the master from the timeline's: its fader, and the limiter approximation —
	 *  wired in only while the limiter is on, tuned to its ceiling. */
	#applyMaster(m: Master, master: MasterBus, live = false) {
		const ctx = this.#ctx!;
		const volume = clampMasterVolume(master.volume);
		if (live) m.input.gain.setTargetAtTime(volume, ctx.currentTime, LIVE_TAU);
		else m.input.gain.value = volume;
		if (master.limiter) {
			const p = limiterParams(master.ceiling_db);
			m.comp.threshold.value = p.threshold;
			m.comp.knee.value = p.knee;
			m.comp.ratio.value = p.ratio;
			m.comp.attack.value = p.attack;
			m.comp.release.value = p.release;
			m.trim.gain.value = p.trim;
		}
		if (m.limiting === master.limiter) return;
		m.input.disconnect();
		m.comp.disconnect();
		m.trim.disconnect();
		if (master.limiter) {
			m.input.connect(m.comp);
			m.comp.connect(m.trim);
			m.trim.connect(m.out);
		} else {
			m.input.connect(m.out);
		}
		m.limiting = master.limiter;
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

	#schedule(clip: Clip, buf: AudioBuffer, s: Session, bus: Bus) {
		const ctx = this.#ctx!;
		const mag = speedMag(clip);
		const dur = (clip.source_out - clip.source_in) / mag;
		const clipStart = clip.timeline_start;
		const clipEnd = clipStart + dur;
		const now = ctx.currentTime;
		const nowT = s.anchorTime + (now - s.anchorCtx) * s.rate;
		if (clipEnd <= nowT) return;

		const src = ctx.createBufferSource();
		src.buffer = buf;
		src.playbackRate.value = mag * s.rate;
		const gain = ctx.createGain();
		src.connect(gain);
		gain.connect(bus.input);
		s.nodes.push({ src, gain });

		// Timeline time -> context time under this session's anchor and rate.
		const at = (tl: number) => s.anchorCtx + (tl - s.anchorTime) / s.rate;
		const offsetSrc = clipStart < nowT ? (nowT - clipStart) * mag : 0;
		if (offsetSrc >= buf.duration) return;
		src.start(Math.max(at(clipStart), now), offsetSrc, buf.duration - offsetSrc);

		// Gain envelope: clip volume shaped by fade-in/out. A transition
		// approximates as an extra fade-in (the export folds it in the same way).
		// The track fader multiplies the clip's own gain, as it does on export — on the
		// bus, which `clipGainAt` leaves out (its product with the clip's is the same).
		const fi = fadeInOf(clip);
		const fo = fadeOutOf(clip);
		const env = (tl: number) => clipGainAt(clip, tl, clipStart, clipEnd);
		const t0 = Math.max(clipStart, nowT);
		gain.gain.setValueAtTime(env(t0), Math.max(at(t0), now));
		if (fi > 0 && clipStart + fi > t0) gain.gain.linearRampToValueAtTime(env(clipStart + fi), at(clipStart + fi));
		const foStart = Math.max(clipEnd - fo, t0);
		if (fo > 0 && clipEnd > t0) {
			gain.gain.setValueAtTime(env(foStart), at(foStart));
			gain.gain.linearRampToValueAtTime(0, at(clipEnd));
		}
	}

	/** Fetch + decode the clip's source window, cached. Reversed clips get the
	 *  samples stored back-to-front so scheduling stays forward-only. */
	async #buffer(clip: Clip): Promise<AudioBuffer | null> {
		const rev = (clip.speed ?? 1) < 0;
		// Cap the window at MAX_WINDOW source seconds; a reversed clip plays from
		// source_out downward, so its window is anchored at the top instead.
		const sin = rev ? Math.max(clip.source_in, clip.source_out - MAX_WINDOW) : clip.source_in;
		const sout = rev ? clip.source_out : Math.min(clip.source_out, clip.source_in + MAX_WINDOW);
		if (sout - sin <= 0) return null;
		// The effect chain is baked into the decode, so it is part of the identity
		// of the cached buffer: retuning an EQ has to re-fetch rather than keep
		// playing the previous sound.
		const fx = clip.audio?.length ? JSON.stringify(clip.audio) : '';
		const key = `${clip.asset_id}:${sin.toFixed(3)}:${sout.toFixed(3)}:${rev ? 'r' : 'f'}:${fx}`;
		const hit = this.#cache.get(key);
		if (hit) return hit;

		const pending = (async (): Promise<AudioBuffer | null> => {
			const bytes = await this.#env.fetchPcm(clip.asset_id, sin, sout - sin, RATE, clip.id);
			if (!bytes || bytes.byteLength < 2) return null;
			const ctx = this.#ctx;
			if (!ctx) return null;
			const i16 = new Int16Array(bytes, 0, Math.floor(bytes.byteLength / 2));
			const buf = ctx.createBuffer(1, i16.length, RATE);
			const ch = buf.getChannelData(0);
			if (rev) for (let i = 0; i < i16.length; i++) ch[i] = i16[i16.length - 1 - i] / 32768;
			else for (let i = 0; i < i16.length; i++) ch[i] = i16[i] / 32768;
			return buf;
		})();
		this.#cache.set(key, pending);
		try {
			const buf = await pending;
			if (buf) {
				this.#cache.set(key, buf);
				this.#evict();
			} else {
				this.#cache.delete(key);
			}
			return buf;
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

	#evict() {
		let total = 0;
		for (const v of this.#cache.values()) if (!(v instanceof Promise)) total += v.duration;
		for (const [k, v] of this.#cache) {
			if (total <= CACHE_CAP) break;
			if (!(v instanceof Promise)) {
				this.#cache.delete(k);
				total -= v.duration;
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
