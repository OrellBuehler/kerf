// Looking at one library asset before it is on the timeline: a still at any time (a scrub, or the pointer moving over
// the picture), and playing it — the picture as a stream of the asset alone (`startAssetPlayback`, the timeline
// preview's own machinery), its sound decoded in windows (`getAudio`) on a context of its own. It is not the preview
// engine: no tracks, no gains, no meters. The picture leads; the sound starts where the picture is once its first
// window is decoded.

import { getAudio, getFrame, startAssetPlayback } from './api';
import { jpegDataUrl } from './frame-transport';

export interface SourceAsset {
	id: string;
	duration: number;
	/** A moving picture: it can be played as a stream. */
	video: boolean;
	audio: boolean;
	fps: number;
}

const WIDTH = 640;
const SAMPLE_RATE = 32000;
/** Seconds of sound decoded at a time, and how long before a window runs out the next is asked for. */
const WINDOW = 12;
const AHEAD = 4;
/** A stream that has shown frames and then none for this long has ended, ms. */
const QUIET_END_MS = 400;
const STALLED_MS = 4000;
/** How long the first frame may take (a 4K original with no proxy yet is slow to open), ms. */
const NO_START_MS = 30000;

let mutedPref = false;

export class SourcePlayer {
	time = $state(0);
	playing = $state(false);
	/** The picture: a still, or the stream's latest frame. `null` while there is none (sound only, or not read yet). */
	frame = $state<string | null>(null);
	error = $state<string | null>(null);
	muted = $state(mutedPref);

	#asset: SourceAsset | null = null;
	#run = 0;
	#stopStream: (() => void) | null = null;
	#ctx: AudioContext | null = null;
	#gain: GainNode | null = null;
	#sources: AudioBufferSourceNode[] = [];
	#raf = 0;
	#want: number | null = null;
	#reading = false;

	/** Show another asset (or none): whatever was playing stops, and its first frame is read. `poster` stands in until
	 *  then. */
	load(asset: SourceAsset | null, poster: string | null = null) {
		this.#halt();
		this.#asset = asset;
		this.time = 0;
		this.frame = poster;
		this.error = null;
		this.#want = null;
		if (asset && (asset.video || !asset.audio)) void this.#still(0);
	}

	/** A still at `t` without moving the position (the pointer over the picture); `null` goes back to the position. */
	hover(t: number | null) {
		if (this.playing || !this.#asset) return;
		void this.#still(t ?? this.time);
	}

	seek(t: number) {
		const a = this.#asset;
		if (!a) return;
		const at = Math.min(Math.max(0, t), a.duration);
		if (this.playing) {
			this.#halt();
			this.time = at;
			this.play();
			return;
		}
		this.time = at;
		void this.#still(at);
	}

	setMuted(muted: boolean) {
		this.muted = muted;
		mutedPref = muted;
		if (this.#gain) this.#gain.gain.value = muted ? 0 : 1;
	}

	toggle() {
		if (this.playing) this.pause();
		else this.play();
	}

	pause() {
		if (!this.playing) return;
		this.#halt();
		if (this.#asset?.video) void this.#still(this.time);
	}

	play() {
		const a = this.#asset;
		if (!a || this.playing || (!a.video && !a.audio)) return;
		const from = this.time >= a.duration - 0.05 ? 0 : this.time;
		const run = ++this.#run;
		this.playing = true;
		this.error = null;
		this.time = from;
		const began = performance.now();
		let lastFrame = 0;
		/** The sound's clock: source time `at` is heard at context time `when`. */
		let anchor: { when: number; at: number } | null = null;
		const sound = () => void this.#sound(run, a, (set) => (anchor = set));

		if (a.video) {
			const fps = a.fps > 30 ? (a.fps > 60 ? a.fps / 4 : a.fps / 2) : null;
			this.#stopStream = startAssetPlayback(
				a.id,
				from,
				{ fps, width: WIDTH },
				(f) => {
					if (run !== this.#run) return;
					const first = lastFrame === 0;
					lastFrame = performance.now();
					this.frame = jpegDataUrl(f.jpeg);
					this.time = Math.min(a.duration, Math.max(0, f.time));
					if (first) sound();
				},
				(message) => {
					if (run !== this.#run) return;
					this.#halt();
					this.error = message;
				}
			);
		} else sound();

		const step = () => {
			if (run !== this.#run) return;
			const now = performance.now();
			if (a.video) {
				// The bridge has no end event: frames and then none, near the end, is the end. A file whose sound outlasts its
				// picture stops short of `duration`, which the longer wait catches.
				const quiet = lastFrame > 0 ? now - lastFrame : 0;
				const atEnd = this.time >= a.duration - 1 && quiet > QUIET_END_MS;
				if (atEnd || quiet > STALLED_MS) return this.#finish(atEnd);
				if (lastFrame === 0 && now - began > NO_START_MS) {
					this.#halt();
					this.error = 'no picture arrived';
					return;
				}
			} else if (anchor && this.#ctx) {
				const t = anchor.at + (this.#ctx.currentTime - anchor.when);
				if (t >= a.duration) return this.#finish(true);
				this.time = Math.max(from, t);
			}
			this.#raf = requestAnimationFrame(step);
		};
		this.#raf = requestAnimationFrame(step);
	}

	dispose() {
		this.#halt();
		this.#asset = null;
		void this.#ctx?.close().catch(() => {});
		this.#ctx = null;
		this.#gain = null;
	}

	#finish(ended: boolean) {
		const a = this.#asset;
		this.#halt();
		if (ended && a) this.time = a.duration;
	}

	#halt() {
		this.#run++;
		this.playing = false;
		cancelAnimationFrame(this.#raf);
		this.#stopStream?.();
		this.#stopStream = null;
		for (const s of this.#sources.splice(0)) {
			try {
				s.stop();
			} catch {
				// never started
			}
			s.disconnect();
		}
	}

	/** One read at a time, the newest request winning: a pointer crossing the picture asks for more frames than can be
	 *  decoded. */
	async #still(t: number) {
		this.#want = t;
		if (this.#reading) return;
		this.#reading = true;
		try {
			while (this.#want !== null) {
				const a = this.#asset;
				const at = this.#want;
				this.#want = null;
				if (!a) break;
				const url = await getFrame(a.id, Math.min(at, Math.max(0, a.duration - 0.05)), WIDTH, false).catch(() => null);
				if (this.#asset === a && url && !this.playing) this.frame = url;
			}
		} finally {
			this.#reading = false;
		}
	}

	async #sound(run: number, a: SourceAsset, anchored: (anchor: { when: number; at: number }) => void) {
		if (!a.audio) return;
		try {
			const ctx = (this.#ctx ??= new AudioContext());
			if (!this.#gain) {
				this.#gain = ctx.createGain();
				this.#gain.connect(ctx.destination);
			}
			this.#gain.gain.value = this.muted ? 0 : 1;
			if (ctx.state === 'suspended') await ctx.resume();
			let pos = this.time;
			let when = 0;
			let first = true;
			while (run === this.#run && pos < a.duration - 0.01) {
				const length = Math.min(WINDOW, a.duration - pos);
				const bytes = await getAudio(a.id, pos, length, SAMPLE_RATE);
				if (run !== this.#run || !bytes) return;
				const pcm = new Int16Array(bytes, 0, Math.floor(bytes.byteLength / 2));
				if (pcm.length === 0) return;
				const buffer = ctx.createBuffer(1, pcm.length, SAMPLE_RATE);
				const ch = buffer.getChannelData(0);
				for (let i = 0; i < pcm.length; i++) ch[i] = pcm[i] / 32768;
				const source = ctx.createBufferSource();
				source.buffer = buffer;
				source.connect(this.#gain);
				if (first) {
					// The picture went on while this was decoded: the sound joins it where it is now.
					const skip = a.video ? Math.max(0, this.time - pos) : 0;
					if (skip >= buffer.duration) {
						pos += length;
						continue;
					}
					when = ctx.currentTime + 0.02;
					source.start(when, skip);
					anchored({ when, at: pos + skip });
					when += buffer.duration - skip;
					first = false;
				} else {
					source.start(when);
					when += buffer.duration;
				}
				this.#sources.push(source);
				pos += length;
				while (run === this.#run && ctx.currentTime < when - AHEAD) await new Promise((r) => setTimeout(r, 250));
			}
		} catch {
			// No sound is not a reason to stop the picture.
		}
	}
}
