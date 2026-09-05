// What the media bin says about an asset: the tech specs a row shows at a
// glance and the facts its context menu lists. Pure, so the phrasing is testable
// and the two surfaces cannot drift apart.

import type { Asset, AssetAnalysis, StreamInfo, Timeline } from './types';

export interface MediaInfo {
	/** `video` / `audio` / `image` — what the thumbnail and badge lead with. */
	kind: 'video' | 'audio' | 'image';
	/** `1920×1080`, or `null` for audio. */
	resolution: string | null;
	/** `29.97 fps`, or `null` when unknown / a still. */
	fps: string | null;
	/** `h264` — the primary video codec, else the audio one. */
	codec: string | null;
	/** `48 kHz stereo aac`, or `null` when the asset has no audio. */
	audio: string | null;
	/** `360 equirect`-style tag, or `null` for flat footage. */
	projection: string | null;
	/** `16:9` reduced from the video frame, or `null`. */
	aspect: string | null;
	/** How many clips on the timeline read from this asset. */
	uses: number;
	/** The clock the row shows; hours appear only past an hour. */
	duration: string;
	/** True when this is a derived file (a stitched Insta360 pair). */
	stitched: boolean;
}

export function videoStream(a: Pick<Asset, 'streams'>): StreamInfo | undefined {
	return a.streams.find((s) => s.kind === 'video');
}

export function audioStream(a: Pick<Asset, 'streams'>): StreamInfo | undefined {
	return a.streams.find((s) => s.kind === 'audio');
}

/** `m:ss` under an hour, `h:mm:ss` above; tenths only under ten seconds. */
export function fmtDuration(secs: number): string {
	const s = Math.max(0, secs);
	if (s < 10) return `${s.toFixed(1)}s`;
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	const sec = Math.floor(s % 60);
	const mmss = `${h ? m.toString().padStart(2, '0') : m}:${sec.toString().padStart(2, '0')}`;
	return h ? `${h}:${mmss}` : mmss;
}

export function fmtFps(fps: number): string {
	const rounded = Math.round(fps * 100) / 100;
	return `${Number.isInteger(rounded) ? rounded : rounded.toFixed(2)} fps`;
}

/** `16:9`, `9:16`, `1:1`, `4:5`, `2:1`; anything else falls back to `1.78:1`. */
export function fmtAspect(w: number, h: number): string {
	if (w <= 0 || h <= 0) return '';
	const known: [number, number][] = [
		[16, 9],
		[9, 16],
		[1, 1],
		[4, 5],
		[4, 3],
		[3, 4],
		[2, 1],
		[21, 9]
	];
	const r = w / h;
	for (const [a, b] of known) if (Math.abs(r - a / b) < 0.01) return `${a}:${b}`;
	return `${(Math.round(r * 100) / 100).toFixed(2)}:1`;
}

export function fmtAudio(s: StreamInfo): string {
	const parts: string[] = [];
	if (s.sample_rate) parts.push(`${Math.round(s.sample_rate / 100) / 10} kHz`);
	if (s.channels) parts.push(s.channels === 1 ? 'mono' : s.channels === 2 ? 'stereo' : `${s.channels}ch`);
	parts.push(s.codec);
	return parts.join(' ');
}

export function usesOf(assetId: string, timeline: Pick<Timeline, 'tracks'>): number {
	let n = 0;
	for (const t of timeline.tracks) for (const c of t.clips) if (c.asset_id === assetId) n++;
	return n;
}

export function mediaInfo(a: Asset, timeline: Pick<Timeline, 'tracks'>): MediaInfo {
	const v = videoStream(a);
	const au = audioStream(a);
	const image = a.streams.some((s) => s.image);
	const kind = image ? 'image' : v ? 'video' : 'audio';
	const projection = v?.projection && v.projection !== 'flat' ? `360 ${v.projection.replace('_', ' ')}` : null;
	return {
		kind,
		resolution: v?.width && v?.height ? `${v.width}×${v.height}` : null,
		fps: v?.fps && !image ? fmtFps(v.fps) : null,
		codec: v?.codec ?? au?.codec ?? null,
		audio: au ? fmtAudio(au) : null,
		projection,
		aspect: v?.width && v?.height ? fmtAspect(v.width, v.height) : null,
		uses: usesOf(a.id, timeline),
		duration: fmtDuration(a.duration),
		stitched: (a.source_paths?.length ?? 0) > 0
	};
}

/** The one-line spec under the name: `1920×1080 · 29.97 fps · h264 · stereo`. */
export function specLine(info: MediaInfo): string {
	const parts: string[] = [];
	if (info.resolution) parts.push(info.resolution);
	if (info.fps) parts.push(info.fps);
	if (info.codec) parts.push(info.codec);
	if (info.kind !== 'audio' && info.audio) parts.push(info.audio.includes('mono') ? 'mono' : 'stereo');
	if (info.kind === 'audio' && info.audio) {
		const rest = info.audio.split(' ').filter((w) => w !== info.codec);
		if (rest.length) parts.push(rest.join(' '));
	}
	return parts.join(' · ');
}

export interface AnalysisFact {
	label: string;
	value: string;
}

/** What analysis found, phrased for a list; empty when nothing has run. */
export function analysisFacts(an: AssetAnalysis | null | undefined, duration: number): AnalysisFact[] {
	if (!an) return [];
	const out: AnalysisFact[] = [];
	if (an.audio_class) {
		const c = an.audio_class;
		out.push({ label: 'Audio', value: `${c.class} (${Math.round(c.confidence * 100)}%)` });
	}
	if (an.loudness) {
		const l = an.loudness;
		out.push({
			label: 'Loudness',
			value: `${l.integrated_lufs.toFixed(1)} LUFS · peak ${l.true_peak_dbtp.toFixed(1)} dBTP`
		});
	}
	if (an.tempo) {
		out.push({
			label: 'Tempo',
			value: `${Math.round(an.tempo.bpm)} BPM (${Math.round(an.tempo.confidence * 100)}%)`
		});
	}
	if (an.silence_segments.length) {
		const silent = an.silence_segments.reduce((s, r) => s + Math.max(0, r.end - r.start), 0);
		const share = duration > 0 ? ` · ${Math.round((silent / duration) * 100)}%` : '';
		out.push({
			label: 'Silence',
			value: `${an.silence_segments.length} ${an.silence_segments.length === 1 ? 'gap' : 'gaps'} · ${fmtDuration(silent)}${share}`
		});
	}
	if (an.scene_changes.length) out.push({ label: 'Scenes', value: `${an.scene_changes.length + 1} shots` });
	if (an.transcript.length) {
		const words = an.transcript.reduce((n, s) => n + s.text.split(/\s+/).filter(Boolean).length, 0);
		out.push({ label: 'Transcript', value: `${an.transcript.length} lines · ${words} words` });
	} else if (an.audio_class && an.audio_class.class !== 'music') {
		out.push({ label: 'Transcript', value: 'no speech' });
	}
	return out;
}

/** The last two path segments, so a long path still says where it lives. */
export function shortPath(path: string): string {
	const parts = path.split(/[\\/]/).filter(Boolean);
	if (parts.length <= 2) return path;
	return `…/${parts.slice(-2).join('/')}`;
}

/** Where a preview thumbnail is sampled: 10% in, so a fade-up does not read as black. */
export function thumbTime(duration: number): number {
	return duration > 2 ? duration * 0.1 : 0;
}
