/* Web Audio preview playback. Decodes clip audio windows to raw PCM via the
   backend, schedules every audio-bearing clip against the timeline with its
   volume / fades / speed applied, and exposes the audio clock so the playhead
   can follow it sample-accurately. Per-clip effect chains and reverse shuttle
   are not auralized — this is a preview monitor, not the export mix. */

import { getAudio } from './api';
import type { Clip, Timeline } from './types';

/** Preview decode rate: mono 32 kHz keeps a minute of PCM under 4 MB on the
 *  wire while staying honest enough to judge a cut. */
const RATE = 32000;
/** Longest audio window fetched per clip, in source seconds — bounds memory on
 *  very long clips (playback inside one clip goes silent past this). */
const MAX_WINDOW = 600;
/** Total decoded source seconds kept in the buffer cache before eviction. */
const CACHE_CAP = 1800;

type Session = {
	anchorTime: number; // timeline seconds when playback started
	anchorCtx: number; // AudioContext.currentTime when playback started
	rate: number;
	nodes: { src: AudioBufferSourceNode; gain: GainNode }[];
};

function speedMag(clip: Clip): number {
	return Math.max(Math.abs(clip.speed ?? 1), 0.01);
}

class AudioEngine {
	#ctx: AudioContext | null = null;
	#cache = new Map<string, AudioBuffer | Promise<AudioBuffer | null>>();
	#session: Session | null = null;

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
		const ctx = (this.#ctx ??= new AudioContext());
		if (ctx.state === 'suspended') void ctx.resume();
		const session: Session = { anchorTime: t, anchorCtx: ctx.currentTime, rate, nodes: [] };
		this.#session = session;
		for (const track of timeline.tracks) {
			for (const clip of track.clips) {
				if (!audioAssets.has(clip.asset_id)) continue;
				const dur = (clip.source_out - clip.source_in) / speedMag(clip);
				if (clip.timeline_start + dur <= t) continue;
				void this.#buffer(clip).then((buf) => {
					// Buffers resolve async; only schedule into the session that asked.
					if (buf && this.#session === session) this.#schedule(clip, buf, session);
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
	}

	#schedule(clip: Clip, buf: AudioBuffer, s: Session) {
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
		src.connect(gain).connect(ctx.destination);
		s.nodes.push({ src, gain });

		// Timeline time -> context time under this session's anchor and rate.
		const at = (tl: number) => s.anchorCtx + (tl - s.anchorTime) / s.rate;
		const offsetSrc = clipStart < nowT ? (nowT - clipStart) * mag : 0;
		if (offsetSrc >= buf.duration) return;
		src.start(Math.max(at(clipStart), now), offsetSrc, buf.duration - offsetSrc);

		// Gain envelope: clip volume shaped by fade-in/out. A transition
		// approximates as an extra fade-in (the export folds it in the same way).
		const vol = clip.volume ?? 1;
		const fi = (clip.fade_in ?? 0) + (clip.transition_in?.duration ?? 0);
		const fo = clip.fade_out ?? 0;
		const env = (tl: number) => {
			let v = vol;
			if (fi > 0 && tl < clipStart + fi) v *= Math.max(0, (tl - clipStart) / fi);
			if (fo > 0 && tl > clipEnd - fo) v *= Math.max(0, (clipEnd - tl) / fo);
			return v;
		};
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
		const key = `${clip.asset_id}:${sin.toFixed(3)}:${sout.toFixed(3)}:${rev ? 'r' : 'f'}`;
		const hit = this.#cache.get(key);
		if (hit) return hit instanceof Promise ? hit : hit;

		const pending = (async (): Promise<AudioBuffer | null> => {
			const bytes = await getAudio(clip.asset_id, sin, sout - sin, RATE);
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
		const buf = await pending;
		if (buf) {
			this.#cache.set(key, buf);
			this.#evict();
		} else {
			this.#cache.delete(key);
		}
		return buf;
	}

	#evict() {
		let total = 0;
		for (const v of this.#cache.values()) if (v instanceof AudioBuffer) total += v.duration;
		for (const [k, v] of this.#cache) {
			if (total <= CACHE_CAP) break;
			if (v instanceof AudioBuffer) {
				this.#cache.delete(k);
				total -= v.duration;
			}
		}
	}
}

export const audio = new AudioEngine();
