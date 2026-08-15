// Export-dialog data tables and pure helpers. The validation / summary logic
// mirrors kerf_core::engine (validate_export, build_export_args) so the UI
// reflects exactly what the backend will do.

import type { Container, ExportOptions, Fit, RateControl } from './types';
import { DEFAULT_EXPORT_OPTIONS } from './types';

export interface ContainerInfo {
	id: Container;
	label: string;
	ext: string;
	video: string[];
	/** Hardware encoders legal in this container (mirrors the Rust allowlist);
	 *  the dialog offers only the ones the backend verified usable. */
	hwVideo: string[];
	audio: string[];
	faststart: boolean;
	audioOnly: boolean;
	videoOnly: boolean;
}

const HW_H26X = ['h264_nvenc', 'hevc_nvenc', 'h264_qsv', 'hevc_qsv', 'h264_videotoolbox', 'hevc_videotoolbox', 'h264_amf', 'hevc_amf'];
const HW_AV1 = ['av1_nvenc', 'av1_qsv'];

export const CONTAINERS: ContainerInfo[] = [
	{ id: 'mp4', label: 'MP4', ext: 'mp4', video: ['libx264', 'libx265', 'libsvtav1'], hwVideo: [...HW_H26X, ...HW_AV1], audio: ['aac', 'alac'], faststart: true, audioOnly: false, videoOnly: false },
	{ id: 'mov', label: 'QuickTime MOV', ext: 'mov', video: ['prores_ks', 'libx264', 'libx265'], hwVideo: HW_H26X, audio: ['aac', 'alac', 'pcm_s16le', 'pcm_s24le'], faststart: true, audioOnly: false, videoOnly: false },
	{ id: 'mkv', label: 'Matroska MKV', ext: 'mkv', video: ['libx264', 'libx265', 'libvpx-vp9', 'libsvtav1'], hwVideo: [...HW_H26X, ...HW_AV1], audio: ['aac', 'libopus', 'libmp3lame', 'flac', 'pcm_s16le'], faststart: false, audioOnly: false, videoOnly: false },
	{ id: 'webm', label: 'WebM', ext: 'webm', video: ['libvpx-vp9', 'libsvtav1'], hwVideo: HW_AV1, audio: ['libopus'], faststart: false, audioOnly: false, videoOnly: false },
	{ id: 'gif', label: 'Animated GIF', ext: 'gif', video: ['gif'], hwVideo: [], audio: [], faststart: false, audioOnly: false, videoOnly: true },
	{ id: 'mp3', label: 'MP3 audio', ext: 'mp3', video: [], hwVideo: [], audio: ['libmp3lame'], faststart: false, audioOnly: true, videoOnly: false },
	{ id: 'm4a', label: 'M4A audio', ext: 'm4a', video: [], hwVideo: [], audio: ['aac', 'alac'], faststart: true, audioOnly: true, videoOnly: false },
	{ id: 'wav', label: 'WAV (PCM)', ext: 'wav', video: [], hwVideo: [], audio: ['pcm_s16le', 'pcm_s24le'], faststart: false, audioOnly: true, videoOnly: false },
	{ id: 'flac', label: 'FLAC', ext: 'flac', video: [], hwVideo: [], audio: ['flac'], faststart: false, audioOnly: true, videoOnly: false }
];

/** Whether a `-c:v` value is a hardware encoder (family read from its suffix,
 *  as in the Rust engine). */
export function isHwCodec(id?: string | null): boolean {
	return !!id && /_(nvenc|qsv|videotoolbox|amf)$/.test(id);
}

export interface VideoCodecInfo {
	id: string;
	label: string;
	crf: [number, number, number] | null; // [min, max, default]; null = no CRF (prores/gif)
	presets: string[] | null; // named presets (x264/x265); null = none / numeric handled separately
	presetKind: 'named' | 'svtav1' | 'cpuused' | null;
	/** The preset to select on codec change; falls back to the kind default. */
	defaultPreset?: string;
	tunes: string[]; // valid -tune values for this encoder ([] = no tune)
	profiles: string[]; // -profile:v choices
	pixFmts: string[];
}

/** A [`VideoCodecInfo`] for one hardware encoder. The backend maps the shared
 *  `crf` field onto each family's own quality knob (-cq / -global_quality /
 *  -q:v / -qp), so the slider means the same thing everywhere. */
function hwCodec(id: string, label: string, crfDefault: number, presets: string[] | null, defaultPreset?: string, profiles: string[] = []): VideoCodecInfo {
	return {
		id,
		label,
		crf: [0, 51, crfDefault],
		presets,
		presetKind: presets ? 'named' : null,
		defaultPreset,
		tunes: [],
		profiles,
		pixFmts: ['yuv420p']
	};
}

const NVENC_PRESETS = ['p1', 'p2', 'p3', 'p4', 'p5', 'p6', 'p7'];
const QSV_PRESETS = ['veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow'];

export const HW_VIDEO_CODECS: Record<string, VideoCodecInfo> = {
	h264_nvenc: hwCodec('h264_nvenc', 'H.264 · NVIDIA GPU (NVENC)', 23, NVENC_PRESETS, 'p4', ['baseline', 'main', 'high']),
	hevc_nvenc: hwCodec('hevc_nvenc', 'HEVC · NVIDIA GPU (NVENC)', 25, NVENC_PRESETS, 'p4', ['main']),
	av1_nvenc: hwCodec('av1_nvenc', 'AV1 · NVIDIA GPU (NVENC)', 30, NVENC_PRESETS, 'p4'),
	h264_qsv: hwCodec('h264_qsv', 'H.264 · Intel GPU (QuickSync)', 23, QSV_PRESETS, 'medium', ['baseline', 'main', 'high']),
	hevc_qsv: hwCodec('hevc_qsv', 'HEVC · Intel GPU (QuickSync)', 25, QSV_PRESETS, 'medium', ['main']),
	av1_qsv: hwCodec('av1_qsv', 'AV1 · Intel GPU (QuickSync)', 30, QSV_PRESETS, 'medium'),
	h264_videotoolbox: hwCodec('h264_videotoolbox', 'H.264 · Apple GPU (VideoToolbox)', 20, null, undefined, ['baseline', 'main', 'high']),
	hevc_videotoolbox: hwCodec('hevc_videotoolbox', 'HEVC · Apple GPU (VideoToolbox)', 22, null, undefined, ['main']),
	h264_amf: hwCodec('h264_amf', 'H.264 · AMD GPU (AMF)', 23, null, undefined, ['baseline', 'main', 'high']),
	hevc_amf: hwCodec('hevc_amf', 'HEVC · AMD GPU (AMF)', 25, null, undefined, ['main'])
};

export const VIDEO_CODECS: Record<string, VideoCodecInfo> = {
	libx264: {
		id: 'libx264',
		label: 'H.264 (libx264)',
		crf: [0, 51, 20],
		presets: ['ultrafast', 'superfast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow'],
		presetKind: 'named',
		tunes: ['film', 'animation', 'grain', 'stillimage', 'zerolatency', 'fastdecode', 'psnr', 'ssim'],
		profiles: ['baseline', 'main', 'high'],
		pixFmts: ['yuv420p', 'yuv422p', 'yuv444p']
	},
	libx265: {
		id: 'libx265',
		label: 'H.265 / HEVC (libx265)',
		crf: [0, 51, 23],
		presets: ['ultrafast', 'superfast', 'veryfast', 'faster', 'fast', 'medium', 'slow', 'slower', 'veryslow'],
		presetKind: 'named',
		tunes: ['psnr', 'ssim', 'grain', 'zerolatency', 'fastdecode', 'animation'],
		profiles: ['main', 'main10'],
		pixFmts: ['yuv420p', 'yuv420p10le']
	},
	'libvpx-vp9': {
		id: 'libvpx-vp9',
		label: 'VP9 (libvpx-vp9)',
		crf: [0, 63, 31],
		presets: ['0', '1', '2', '3', '4', '5', '6', '7', '8'],
		presetKind: 'cpuused',
		tunes: [],
		profiles: [],
		pixFmts: ['yuv420p', 'yuv420p10le']
	},
	libsvtav1: {
		id: 'libsvtav1',
		label: 'AV1 (SVT-AV1)',
		crf: [0, 63, 30],
		presets: ['2', '4', '6', '8', '10', '12'],
		presetKind: 'svtav1',
		tunes: [],
		profiles: [],
		pixFmts: ['yuv420p', 'yuv420p10le']
	},
	prores_ks: {
		id: 'prores_ks',
		label: 'Apple ProRes (prores_ks)',
		crf: null,
		presets: null,
		presetKind: null,
		tunes: [],
		profiles: [],
		pixFmts: ['yuv422p10le', 'yuva444p10le']
	},
	gif: { id: 'gif', label: 'GIF', crf: null, presets: null, presetKind: null, tunes: [], profiles: [], pixFmts: [] },
	...HW_VIDEO_CODECS
};

export const PRORES_PROFILES: { value: number; label: string }[] = [
	{ value: 0, label: 'Proxy' },
	{ value: 1, label: 'LT' },
	{ value: 2, label: 'Standard (422)' },
	{ value: 3, label: 'HQ (422 HQ)' },
	{ value: 4, label: '4444' },
	{ value: 5, label: '4444 XQ' }
];

export interface AudioCodecInfo {
	id: string;
	label: string;
	lossy: boolean;
}

export const AUDIO_CODECS: Record<string, AudioCodecInfo> = {
	aac: { id: 'aac', label: 'AAC', lossy: true },
	libmp3lame: { id: 'libmp3lame', label: 'MP3', lossy: true },
	libopus: { id: 'libopus', label: 'Opus', lossy: true },
	flac: { id: 'flac', label: 'FLAC (lossless)', lossy: false },
	alac: { id: 'alac', label: 'ALAC (lossless)', lossy: false },
	pcm_s16le: { id: 'pcm_s16le', label: 'PCM 16-bit', lossy: false },
	pcm_s24le: { id: 'pcm_s24le', label: 'PCM 24-bit', lossy: false }
};

export const RATE_CONTROLS: { id: RateControl; label: string }[] = [
	{ id: 'crf', label: 'Quality (CRF)' },
	{ id: 'bitrate', label: 'Bitrate' },
	{ id: 'two_pass', label: 'Two-pass' },
	{ id: 'lossless', label: 'Lossless' }
];

export const AUDIO_BITRATES = ['96k', '128k', '160k', '192k', '256k', '320k', '384k'];
export const SAMPLE_RATES = [44100, 48000];
export const FITS: { id: Fit; label: string; hint: string }[] = [
	{ id: 'contain', label: 'Fit (letterbox)', hint: 'Whole frame, black bars where the shapes differ.' },
	{ id: 'cover', label: 'Fill (crop)', hint: 'Fills the frame; the overflow is cropped off.' }
];

export const SCALERS = ['bicubic', 'bilinear', 'lanczos', 'neighbor', 'spline'];
export const GIF_DITHERS = ['bayer', 'sierra2', 'none'];

export interface ResolutionChoice {
	label: string;
	value: [number, number] | null;
}
export const RESOLUTIONS: ResolutionChoice[] = [
	{ label: 'Source', value: null },
	{ label: '3840×2160 (4K)', value: [3840, 2160] },
	{ label: '2560×1440 (1440p)', value: [2560, 1440] },
	{ label: '1920×1080 (1080p)', value: [1920, 1080] },
	{ label: '1280×720 (720p)', value: [1280, 720] },
	{ label: '1080×1920 (9:16)', value: [1080, 1920] },
	{ label: '1080×1080 (1:1)', value: [1080, 1080] },
	{ label: '854×480 (480p)', value: [854, 480] }
];

export const FRAME_RATES: { label: string; value: number | null }[] = [
	{ label: 'Source', value: null },
	{ label: '60', value: 60 },
	{ label: '59.94', value: 59.94 },
	{ label: '50', value: 50 },
	{ label: '30', value: 30 },
	{ label: '29.97', value: 29.97 },
	{ label: '25', value: 25 },
	{ label: '24', value: 24 },
	{ label: '23.976', value: 23.976 }
];

export interface Preset {
	id: string;
	label: string;
	description: string;
	opts: Partial<ExportOptions>;
}

export const PRESETS: Preset[] = [
	{
		id: 'web_1080p',
		label: 'Web 1080p',
		description: 'H.264 MP4 that plays everywhere.',
		opts: { container: 'mp4', video_codec: 'libx264', rate_control: 'crf', crf: 20, preset: 'medium', profile_v: 'high', pix_fmt: 'yuv420p', resolution: [1920, 1080], audio_codec: 'aac', audio_bitrate: '192k', faststart: true }
	},
	{
		id: 'youtube_4k',
		label: 'YouTube 4K (HEVC)',
		description: 'Half-size UHD via H.265.',
		opts: { container: 'mp4', video_codec: 'libx265', rate_control: 'crf', crf: 23, preset: 'medium', pix_fmt: 'yuv420p', resolution: [3840, 2160], audio_codec: 'aac', audio_bitrate: '256k', faststart: true }
	},
	{
		id: 'av1_web',
		label: 'AV1 (smallest)',
		description: 'Next-gen efficiency via SVT-AV1.',
		opts: { container: 'mkv', video_codec: 'libsvtav1', rate_control: 'crf', crf: 30, preset: '8', pix_fmt: 'yuv420p', resolution: [1920, 1080], audio_codec: 'libopus', audio_bitrate: '128k' }
	},
	{
		id: 'prores_master',
		label: 'ProRes 422 HQ',
		description: 'Edit-grade intermediate. Large files.',
		opts: { container: 'mov', video_codec: 'prores_ks', prores_profile: 3, pix_fmt: 'yuv422p10le', audio_codec: 'pcm_s24le' }
	},
	{
		id: 'web_720p',
		label: 'Small share (720p)',
		description: 'Compact H.264 for messaging.',
		opts: { container: 'mp4', video_codec: 'libx264', rate_control: 'crf', crf: 26, preset: 'slower', profile_v: 'high', pix_fmt: 'yuv420p', resolution: [1280, 720], audio_codec: 'aac', audio_bitrate: '128k', faststart: true }
	},
	{
		id: 'vertical_reel',
		label: 'Reels / Shorts (9:16)',
		description: '1080×1920 vertical H.264, filled and cropped.',
		opts: { container: 'mp4', video_codec: 'libx264', rate_control: 'crf', crf: 23, preset: 'medium', pix_fmt: 'yuv420p', resolution: [1080, 1920], fit: 'cover', fps: 30, audio_codec: 'aac', audio_bitrate: '128k', faststart: true }
	},
	{
		id: 'square_social',
		label: 'Square (1:1)',
		description: '1080×1080 for the feed, filled and cropped.',
		opts: { container: 'mp4', video_codec: 'libx264', rate_control: 'crf', crf: 23, preset: 'medium', pix_fmt: 'yuv420p', resolution: [1080, 1080], fit: 'cover', fps: 30, audio_codec: 'aac', audio_bitrate: '128k', faststart: true }
	},
	{
		id: 'target_size_2pass',
		label: 'Target size (2-pass)',
		description: 'Hit a specific average bitrate.',
		opts: { container: 'mp4', video_codec: 'libx264', rate_control: 'two_pass', video_bitrate: '8M', preset: 'slow', pix_fmt: 'yuv420p', resolution: [1920, 1080], audio_codec: 'aac', audio_bitrate: '192k', faststart: true }
	},
	{
		id: 'gif',
		label: 'Animated GIF',
		description: 'Looping GIF, optimized palette, no audio.',
		opts: { container: 'gif', video_codec: 'gif', include_audio: false, resolution: [854, 480], fps: 15, gif_dither: 'bayer', gif_loop: true }
	},
	{
		id: 'audio_mp3',
		label: 'Audio · MP3 320k',
		description: 'Just the mixed audio bed.',
		opts: { container: 'mp3', video_codec: null, audio_codec: 'libmp3lame', audio_bitrate: '320k' }
	},
	{
		id: 'audio_wav',
		label: 'Audio · WAV',
		description: 'Uncompressed 24-bit master.',
		opts: { container: 'wav', video_codec: null, audio_codec: 'pcm_s24le', audio_sample_rate: 48000 }
	}
];

/** A full options object for a preset id, layered over the bare default. */
export function applyPreset(id: string): ExportOptions {
	const p = PRESETS.find((x) => x.id === id);
	return { ...DEFAULT_EXPORT_OPTIONS, ...(p?.opts ?? {}) };
}

export function containerInfo(c: Container): ContainerInfo {
	return CONTAINERS.find((x) => x.id === c) ?? CONTAINERS[0];
}

/** The codec-dependent option defaults to apply when the video codec changes,
 *  so a stale value (e.g. an x264 named preset on VP9) never reaches ffmpeg. */
export function videoCodecDefaults(id: string): Partial<ExportOptions> {
	const v = VIDEO_CODECS[id];
	if (!v) return { video_codec: id };
	const preset =
		v.defaultPreset ?? (v.presetKind === 'named' ? 'medium' : v.presetKind === 'svtav1' ? '8' : v.presetKind === 'cpuused' ? '4' : null);
	return { video_codec: id, crf: v.crf ? v.crf[2] : null, preset, pix_fmt: v.pixFmts[0] ?? null, tune: null, profile_v: null };
}

/** Re-map codecs / faststart so they stay legal after a container change. */
export function reconcileContainer(opts: ExportOptions): ExportOptions {
	const info = containerInfo(opts.container);
	const next = { ...opts };
	if (info.audioOnly) {
		next.video_codec = null;
	} else if (!next.video_codec || !(info.video.includes(next.video_codec) || info.hwVideo.includes(next.video_codec))) {
		next.video_codec = info.video[0] ?? null;
	}
	// A forced codec change must re-derive its preset / pix_fmt / crf / tune.
	if (next.video_codec && next.video_codec !== opts.video_codec) {
		Object.assign(next, videoCodecDefaults(next.video_codec));
	}
	if (info.videoOnly) {
		next.include_audio = false;
		next.audio_codec = null;
	} else if (!next.audio_codec || !info.audio.includes(next.audio_codec)) {
		next.audio_codec = info.audio[0] ?? null;
	}
	if (!info.faststart) next.faststart = false;
	return next;
}

function bitrateValid(s?: string | null): boolean {
	if (!s) return false;
	return /^\d+(\.\d+)?[kKmM]?$/.test(s.trim()) && parseFloat(s) > 0;
}

/** Mirrors kerf_core::engine::validate_export. */
export function validateExport(opts: ExportOptions, hasVideo: boolean, hasAudio: boolean): string[] {
	const issues: string[] = [];
	const info = containerInfo(opts.container);
	const wantVideo = hasVideo && !info.audioOnly;
	const wantAudio = hasAudio && !info.videoOnly && opts.include_audio;

	if (info.audioOnly && !hasAudio) issues.push(`${info.ext.toUpperCase()} is audio-only, but the timeline has no audio.`);
	if (info.videoOnly && !hasVideo) issues.push('GIF export needs video, but the timeline has no video.');
	if (!wantVideo && !wantAudio) issues.push('These settings would export nothing.');
	if (wantVideo && opts.video_codec && !(info.video.includes(opts.video_codec) || info.hwVideo.includes(opts.video_codec))) {
		issues.push(`${opts.video_codec} can't go in a .${info.ext} file.`);
	}
	const rateMode = opts.video_codec !== 'prores_ks' && opts.video_codec !== 'gif';
	if (wantVideo && rateMode && (opts.rate_control === 'bitrate' || opts.rate_control === 'two_pass') && !opts.video_bitrate) {
		issues.push('A target video bitrate is required for bitrate / two-pass.');
	}
	if (wantVideo && opts.video_codec && opts.rate_control === 'two_pass' && isHwCodec(opts.video_codec)) {
		issues.push(`Two-pass encoding isn't supported for hardware encoder ${opts.video_codec}; use crf or bitrate.`);
	}
	if (wantVideo && opts.tune && (opts.video_codec === 'libx264' || opts.video_codec === 'libx265') && !VIDEO_CODECS[opts.video_codec].tunes.includes(opts.tune)) {
		issues.push(`tune "${opts.tune}" is not valid for ${opts.video_codec}.`);
	}
	if (opts.video_bitrate && !bitrateValid(opts.video_bitrate)) issues.push(`Invalid video bitrate "${opts.video_bitrate}".`);
	if (opts.max_rate && !bitrateValid(opts.max_rate)) issues.push(`Invalid max rate "${opts.max_rate}".`);
	if (opts.buf_size && !bitrateValid(opts.buf_size)) issues.push(`Invalid buffer size "${opts.buf_size}".`);
	if (wantAudio && opts.audio_codec && !info.audio.includes(opts.audio_codec)) {
		issues.push(`${opts.audio_codec} can't go in a .${info.ext} file.`);
	}
	if (wantAudio && opts.audio_bitrate && !bitrateValid(opts.audio_bitrate)) issues.push(`Invalid audio bitrate "${opts.audio_bitrate}".`);
	return issues;
}

/** A short human summary of the encode, e.g. "MP4 · H.264 · CRF 20 · 1080p · AAC 192k · faststart". */
export function buildSummary(opts: ExportOptions, hasVideo: boolean, hasAudio: boolean): string {
	const info = containerInfo(opts.container);
	const parts: string[] = [info.ext.toUpperCase()];
	const wantVideo = hasVideo && !info.audioOnly;
	const wantAudio = hasAudio && !info.videoOnly && opts.include_audio;

	if (wantVideo) {
		const vc = opts.video_codec ? VIDEO_CODECS[opts.video_codec] : undefined;
		if (vc) parts.push(vc.label.replace(/\s*\(.*\)/, ''));
		if (opts.video_codec === 'prores_ks') {
			parts.push(PRORES_PROFILES.find((p) => p.value === opts.prores_profile)?.label ?? 'HQ');
		} else if (opts.video_codec === 'gif') {
			// palette implied
		} else if (opts.rate_control === 'crf' && opts.crf != null) {
			parts.push(`CRF ${opts.crf}`);
		} else if (opts.rate_control === 'lossless') {
			parts.push('lossless');
		} else if (opts.video_bitrate) {
			parts.push(`${opts.video_bitrate}${opts.rate_control === 'two_pass' ? ' 2-pass' : ''}`);
		}
		parts.push(opts.resolution ? `${opts.resolution[0]}×${opts.resolution[1]}` : 'source');
		// Worth surfacing: at a delivery aspect that differs from the footage it
		// decides between a cropped shot and a letterboxed one.
		if (opts.resolution && opts.fit === 'cover') parts.push('filled');
		if (opts.fps) parts.push(`${opts.fps}fps`);
	}
	if (wantAudio && opts.audio_codec) {
		const ac = AUDIO_CODECS[opts.audio_codec];
		let a = ac?.label.replace(/\s*\(.*\)/, '') ?? opts.audio_codec;
		if (ac?.lossy && opts.audio_bitrate) a += ` ${opts.audio_bitrate}`;
		parts.push(a);
	} else if (!wantAudio && hasAudio && !info.videoOnly) {
		parts.push('no audio');
	}
	if (opts.faststart && info.faststart) parts.push('faststart');
	return parts.join(' · ');
}

/** An approximate `ffmpeg …` command line, mirrored from the Rust builder for
 *  the "show command" disclosure. Not authoritative (the real argv is built in
 *  kerf-core), but accurate for the encode flags. */
export function buildCommandPreview(opts: ExportOptions, hasVideo: boolean, hasAudio: boolean, outPath = 'output.' + containerInfo(opts.container).ext): string {
	const info = containerInfo(opts.container);
	const wantVideo = hasVideo && !info.audioOnly;
	const wantAudio = hasAudio && !info.videoOnly && opts.include_audio;
	const hwaccel = opts.hwaccel && opts.hwaccel !== 'none' ? ['-hwaccel', opts.hwaccel] : [];
	const a: string[] = ['ffmpeg', '-y', ...hwaccel, '-i', '…inputs…', '-filter_complex', '…graph…'];
	if (wantVideo) a.push('-map', '[outv]');
	if (wantAudio) a.push('-map', '[outa]');

	const vc = opts.video_codec;
	if (wantVideo && vc) {
		const h264 = vc === 'libx264' || vc.startsWith('h264_');
		const hevc = vc === 'libx265' || vc.startsWith('hevc_');
		a.push('-c:v', vc);
		if (vc === 'prores_ks') {
			a.push('-profile:v', String(opts.prores_profile ?? 3));
		} else if (vc !== 'gif') {
			if (opts.rate_control === 'crf') {
				// Hardware families spell the constant-quality knob differently.
				const q = opts.crf != null ? String(opts.crf) : null;
				if (vc.endsWith('_nvenc')) a.push('-rc', 'vbr', ...(q ? ['-cq', q] : []), '-b:v', '0');
				else if (vc.endsWith('_qsv')) q && a.push('-global_quality', q);
				else if (vc.endsWith('_videotoolbox'))
					opts.crf != null && a.push('-q:v', String(Math.min(100, Math.max(1, Math.round((1 - Math.min(opts.crf, 51) / 51) * 100)))));
				else if (vc.endsWith('_amf')) a.push('-rc', 'cqp', ...(q ? ['-qp_i', q, '-qp_p', q] : []));
				else {
					if (q) a.push('-crf', q);
					if (vc === 'libvpx-vp9') a.push('-b:v', '0');
				}
			} else if (opts.rate_control === 'bitrate' && opts.video_bitrate) {
				a.push('-b:v', opts.video_bitrate);
				if (opts.max_rate) a.push('-maxrate', opts.max_rate);
				if (opts.buf_size) a.push('-bufsize', opts.buf_size);
			} else if (opts.rate_control === 'two_pass' && opts.video_bitrate) {
				a.push('-b:v', opts.video_bitrate, '-pass', '1/2');
			} else if (opts.rate_control === 'lossless') {
				a.push(vc === 'libvpx-vp9' ? '-lossless 1' : '-crf 0');
			}
			if (vc === 'libvpx-vp9') a.push('-cpu-used', opts.preset ?? '4');
			else if (opts.preset) a.push('-preset', opts.preset);
			if ((vc === 'libx264' || vc === 'libx265') && opts.tune) a.push('-tune', opts.tune);
			if ((h264 || hevc) && opts.profile_v) a.push('-profile:v', opts.profile_v);
			if (hevc && (opts.container === 'mp4' || opts.container === 'mov')) a.push('-tag:v', 'hvc1');
			a.push('-pix_fmt', opts.pix_fmt ?? 'yuv420p');
		}
	}
	const ac = opts.audio_codec;
	if (wantAudio && ac) {
		a.push('-c:a', ac);
		if (AUDIO_CODECS[ac]?.lossy && opts.audio_bitrate) a.push('-b:a', opts.audio_bitrate);
		if (ac === 'flac' && opts.flac_compression != null) a.push('-compression_level', String(opts.flac_compression));
	}
	if (hasAudio && !wantAudio && !info.videoOnly) a.push('-an');
	if (opts.faststart && info.faststart) a.push('-movflags', '+faststart');
	if (opts.container === 'gif') a.push('-loop', opts.gif_loop ? '0' : '-1');
	if (opts.metadata_title) a.push('-metadata', `title=${opts.metadata_title}`);
	a.push(outPath);
	return a.join(' ');
}
