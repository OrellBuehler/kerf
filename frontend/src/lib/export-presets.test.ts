import { describe, expect, test } from 'bun:test';
import {
	CONTAINERS,
	FRAME_RATES,
	GPU_PRESET_ID,
	HW_VIDEO_CODECS,
	PRESETS,
	applyPreset,
	buildCommandPreview,
	buildSummary,
	containerInfo,
	frameRateOptions,
	gpuPreset,
	hwQp,
	isHwCodec,
	LOUDNESS_TARGETS,
	loudnessChoice,
	presetsFor,
	reconcileContainer,
	validateExport,
	withLoudness
} from './export-presets';
import type { LoudnessPreset } from './types';

describe('the loudness target', () => {
	test('lists the engine presets, in the order the select shows them', () => {
		expect(LOUDNESS_TARGETS.map((t) => t.id)).toEqual(['off', 'youtube', 'spotify', 'apple', 'broadcast']);
		expect(LOUDNESS_TARGETS.map((t) => t.lufs)).toEqual([null, -14, -14, -16, -23]);
		// Every label says the number the engine normalizes to.
		for (const t of LOUDNESS_TARGETS) if (t.lufs != null) expect(t.label).toContain(`${t.lufs}`.replace('-', '−'));
	});

	test('an export is off until a target is chosen', () => {
		expect(loudnessChoice(applyPreset('web_1080p'))).toBe('off');
		expect(loudnessChoice({})).toBe('off');
		expect(loudnessChoice({ loudness: null })).toBe('off');
	});

	test('the old loudnorm flag is the YouTube target, and loudness wins over it as in the engine', () => {
		expect(loudnessChoice({ loudnorm: true })).toBe('youtube');
		expect(loudnessChoice({ loudnorm: false })).toBe('off');
		expect(loudnessChoice({ loudnorm: true, loudness: 'broadcast' })).toBe('broadcast');
		// `null` is "not set", not "off": the flag still speaks (the engine's `Option::or`).
		expect(loudnessChoice({ loudnorm: true, loudness: null })).toBe('youtube');
	});

	test('choosing a target writes it and clears the flag, so Off really is off', () => {
		const flagged = { ...applyPreset('web_1080p'), loudnorm: true };
		for (const id of ['youtube', 'spotify', 'apple', 'broadcast'] as LoudnessPreset[]) {
			const next = withLoudness(flagged, id);
			expect(next.loudness).toBe(id);
			expect(next.loudnorm).toBe(false);
			expect(loudnessChoice(next)).toBe(id);
		}
		const off = withLoudness(flagged, 'off');
		expect(off.loudness).toBeNull();
		expect(loudnessChoice(off)).toBe('off');
		// Nothing else about the options moves.
		expect({ ...off, loudness: undefined, loudnorm: undefined }).toEqual({ ...flagged, loudness: undefined, loudnorm: undefined });
	});

	test('the summary names the target next to the audio', () => {
		const base = applyPreset('web_1080p');
		expect(buildSummary(base, true, true)).not.toContain('LUFS');
		expect(buildSummary(withLoudness(base, 'apple'), true, true)).toContain('-16 LUFS');
		expect(buildSummary({ ...base, loudnorm: true }, true, true)).toContain('-14 LUFS');
		// No sound in the file, no target to name.
		expect(buildSummary(withLoudness(base, 'apple'), true, false)).not.toContain('LUFS');
		expect(buildSummary({ ...withLoudness(base, 'apple'), include_audio: false }, true, true)).not.toContain('LUFS');
	});
});

describe('the frame-rate select', () => {
	// The dialog's select shows `String(fps)` (or "source"); a value with no option renders blank.
	const shown = (fps: number | null | undefined) =>
		frameRateOptions(fps).some((f) => (f.value ? String(f.value) : 'source') === (fps ? String(fps) : 'source'));

	test('every preset opens on a frame rate the select has an option for', () => {
		for (const p of PRESETS) expect(shown(applyPreset(p.id).fps), p.id).toBe(true);
	});

	test('the GIF preset lands on 15, and 12 and 10 are there to go lower', () => {
		const values = FRAME_RATES.map((f) => f.value);
		expect(applyPreset('gif').fps).toBe(15);
		expect(values).toContain(15);
		expect(values).toContain(12);
		expect(values).toContain(10);
	});

	test('a rate the list does not carry is added rather than leaving the select blank', () => {
		expect(shown(18)).toBe(true);
		expect(frameRateOptions(18).at(-1)).toEqual({ label: '18', value: 18 });
		expect(frameRateOptions(18)).toHaveLength(FRAME_RATES.length + 1);
	});

	test('a listed rate, or the source rate, adds nothing', () => {
		expect(frameRateOptions(30)).toBe(FRAME_RATES);
		expect(frameRateOptions(null)).toBe(FRAME_RATES);
		expect(frameRateOptions(undefined)).toBe(FRAME_RATES);
	});
});

describe('the hardware encoders', () => {
	const cmd = (video_codec: string, crf: number | null = 20) =>
		buildCommandPreview({ ...applyPreset('web_1080p'), video_codec, crf, preset: null }, true, false);

	test('an AMF or VAAPI constant QP is the CRF plus 6 (AV1: times 4), as the engine maps it', () => {
		// The numbers of `amf_crf_is_offset_onto_the_qp_scale_and_asks_for_quality` in cli.rs.
		expect(hwQp('h264_amf', 20)).toBe(26);
		expect(hwQp('hevc_amf', 0)).toBe(6);
		expect(hwQp('hevc_amf', 51)).toBe(51);
		expect(hwQp('h264_amf', 1e9)).toBe(51);
		expect(hwQp('av1_amf', 30)).toBe(120);
		expect(hwQp('av1_amf', 63)).toBe(252);
		expect(hwQp('av1_amf', 200)).toBe(255);
		expect(hwQp('av1_vaapi', 1e9)).toBe(255);
	});

	test('the command preview spells AMF as the engine does, quality preset included', () => {
		expect(cmd('h264_amf')).toContain('-c:v h264_amf -rc cqp -qp_i 26 -qp_p 26 -quality quality');
		expect(cmd('av1_amf', 30)).toContain('-rc cqp -qp_i 120 -qp_p 120 -quality quality');
		// The other families keep the CRF's own number.
		expect(cmd('h264_nvenc')).toContain('-cq 20');
		expect(cmd('hevc_qsv')).toContain('-global_quality 20');
		expect(cmd('h264_nvenc')).not.toContain('-quality');
	});

	test('a VAAPI export names its device, uploads in the graph and sets no pix_fmt', () => {
		const line = cmd('h264_vaapi');
		expect(line).toContain('-vaapi_device /dev/dri/renderD128');
		expect(line).toContain('format=nv12,hwupload');
		expect(line).toContain('-rc_mode CQP -global_quality 26');
		expect(line).not.toContain('-pix_fmt');
		expect(cmd('hevc_vaapi')).toContain('-tag:v hvc1');
		// Nothing of it on an export that is not VAAPI.
		for (const other of ['libx264', 'h264_amf', 'h264_nvenc']) {
			expect(cmd(other)).not.toContain('vaapi');
			expect(cmd(other)).not.toContain('hwupload');
		}
		expect(cmd('libx264')).toContain('-pix_fmt yuv420p');
	});

	test('VAAPI is an 8-bit 4:2:0 encoder, as the engine validates', () => {
		const base = { ...applyPreset('web_1080p'), video_codec: 'h264_vaapi' };
		expect(validateExport({ ...base, pix_fmt: 'yuv420p' }, true, true)).toEqual([]);
		expect(validateExport({ ...base, pix_fmt: 'nv12' }, true, true)).toEqual([]);
		expect(validateExport({ ...base, pix_fmt: 'yuv420p10le' }, true, true)).toEqual(['VAAPI encoders take 8-bit 4:2:0 (yuv420p) here.']);
		expect(validateExport({ ...base, video_codec: 'libx265', pix_fmt: 'yuv420p10le' }, true, true)).toEqual([]);
	});

	test('every family is a hardware codec, two-pass refused', () => {
		for (const id of Object.keys(HW_VIDEO_CODECS)) expect(isHwCodec(id), id).toBe(true);
		expect(isHwCodec('libx264')).toBe(false);
		const issues = validateExport({ ...applyPreset('web_1080p'), video_codec: 'h264_vaapi', rate_control: 'two_pass', video_bitrate: '8M' }, true, true);
		expect(issues.join(' ')).toContain('Two-pass');
	});

	test('the containers list the encoders the engine allows in them', () => {
		// `Container::video_codecs` in cli.rs: AV1 not in MOV, WebM takes only AV1.
		const hw = (id: Parameters<typeof containerInfo>[0]) => containerInfo(id).hwVideo;
		for (const c of ['mp4', 'mkv'] as const) {
			for (const id of ['h264_amf', 'hevc_amf', 'av1_amf', 'h264_vaapi', 'hevc_vaapi', 'av1_vaapi']) expect(hw(c), `${c} ${id}`).toContain(id);
		}
		expect(hw('mov')).toContain('hevc_vaapi');
		expect(hw('mov')).not.toContain('av1_vaapi');
		expect(hw('mov')).not.toContain('av1_amf');
		expect([...hw('webm')].sort()).toEqual(['av1_amf', 'av1_nvenc', 'av1_qsv', 'av1_vaapi']);
		for (const c of CONTAINERS) for (const id of c.hwVideo) expect(HW_VIDEO_CODECS[id], `${c.id} ${id}`).toBeDefined();
	});

	test('the AMF H.264 profiles are ones AMF has names for', () => {
		// `-profile:v baseline` made h264_amf refuse to open ("Unable to parse option value").
		expect(HW_VIDEO_CODECS.h264_amf.profiles).not.toContain('baseline');
		expect(HW_VIDEO_CODECS.h264_amf.profiles).toEqual(['constrained_baseline', 'main', 'high']);
	});
});

describe('the "Fast (GPU)" preset', () => {
	test('is offered only with a verified H.264 hardware encoder, and never by default', () => {
		expect(gpuPreset([])).toBeNull();
		expect(presetsFor([])).toBe(PRESETS);
		expect(presetsFor()).toBe(PRESETS);
		// HEVC and AV1 alone are not a reason: the preset is H.264.
		expect(gpuPreset(['hevc_amf', 'av1_amf'])).toBeNull();
		expect(PRESETS.some((p) => p.id === GPU_PRESET_ID)).toBe(false);
		expect(applyPreset(GPU_PRESET_ID)).toEqual(applyPreset('nonesuch'));
		expect(applyPreset('web_1080p').video_codec).toBe('libx264');
	});

	test("sits after Web 1080p and takes the first verified encoder in the engine's family order", () => {
		const list = presetsFor(['hevc_amf', 'h264_vaapi', 'h264_amf']);
		expect(list.map((p) => p.id).slice(0, 3)).toEqual(['web_1080p', GPU_PRESET_ID, 'youtube_4k']);
		expect(list).toHaveLength(PRESETS.length + 1);
		expect(gpuPreset(['h264_vaapi', 'h264_amf'])?.opts.video_codec).toBe('h264_amf');
		expect(gpuPreset(['h264_vaapi'])?.opts.video_codec).toBe('h264_vaapi');
		expect(gpuPreset(['h264_amf', 'h264_nvenc'])?.opts.video_codec).toBe('h264_nvenc');
	});

	test("is a legal export in the container it names, at the encoder's own defaults", () => {
		for (const codec of ['h264_nvenc', 'h264_qsv', 'h264_videotoolbox', 'h264_amf', 'h264_vaapi']) {
			const opts = applyPreset(GPU_PRESET_ID, [codec]);
			expect(opts.video_codec, codec).toBe(codec);
			expect(opts.container).toBe('mp4');
			expect(opts.hwaccel).toBe('auto');
			expect(opts.crf).toBe(HW_VIDEO_CODECS[codec].crf?.[2]);
			expect(opts.pix_fmt).toBe('yuv420p');
			expect(opts.profile_v ?? null).toBeNull();
			expect(validateExport(opts, true, true), codec).toEqual([]);
			// A container change keeps the codec: it is legal in MP4 and MKV.
			expect(reconcileContainer({ ...opts, container: 'mkv' }).video_codec).toBe(codec);
		}
		expect(applyPreset(GPU_PRESET_ID, ['h264_nvenc']).preset).toBe('p4');
		expect(applyPreset(GPU_PRESET_ID, ['h264_amf']).preset ?? null).toBeNull();
	});

	test('says what it runs on and what the trade is', () => {
		const p = gpuPreset(['h264_amf'])!;
		expect(p.label).toBe('Fast (GPU)');
		expect(p.description).toContain('AMD GPU (AMF)');
		expect(p.description).toContain('somewhat larger file');
	});
});
