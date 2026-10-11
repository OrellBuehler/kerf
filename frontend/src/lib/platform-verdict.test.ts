import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { applyPreset, PRESETS, RESOLUTIONS } from './export-presets';
import { VARIANT_PRESETS } from './delivery-formats';
import { report } from './platforms';
import { judgeable, platformVerdict, type TickedShape, type Verdict, type VerdictInput } from './platform-verdict';
import { SRC, uiSources } from './test-sources';
import type { CutSummary, PlatformReport, Timeline } from './types';

const clip = { id: 'c', asset_id: 'a', source_in: 0, source_out: 1, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 };
const track = (kind: 'video' | 'audio', clips: unknown[]) => ({ id: kind, name: kind, kind, clips }) as never;
const withPicture = { tracks: [track('video', [clip]), track('audio', [clip])] } as Timeline;
const soundOnly = { tracks: [track('audio', [clip])] } as Timeline;
const empty = { tracks: [track('video', []), track('audio', [])] } as Timeline;

const cut = (width: number, height: number, duration: number, over: Partial<CutSummary> = {}): CutSummary => ({
	duration,
	width,
	height,
	has_audio: true,
	has_text: false,
	...over
});
const rep = (width: number, height: number, duration = 20, over: Partial<CutSummary> = {}): PlatformReport =>
	report(cut(width, height, duration, over));
const shape = (label: string, duration = 20): TickedShape => {
	const p = VARIANT_PRESETS.find((v) => v.label === label)!;
	return { label, report: rep(p.format!.width, p.format!.height, duration) };
};
const verdict = (input: Partial<VerdictInput> & { file?: PlatformReport }): Verdict => {
	const v = platformVerdict({ file: rep(1920, 1080), project: null, shapes: [], ...input });
	if (!v) throw new Error('expected a verdict');
	return v;
};
const levels = (v: Verdict) => v.groups.flatMap((g) => g.lines.map((l) => l.level));
const texts = (v: Verdict) => v.groups.flatMap((g) => g.lines.map((l) => l.text));

describe('judgeable: the check only speaks about a video export that has a picture', () => {
	test('a video preset over a cut with a picture is judged', () => {
		for (const id of ['web_1080p', 'web_720p', 'vertical_reel', 'square_social', 'youtube_4k', 'av1_web', 'prores_master'])
			expect(judgeable(applyPreset(id), withPicture), id).toBe(true);
	});

	test('audio presets are not: an MP3 is not letterboxed anywhere', () => {
		expect(judgeable(applyPreset('audio_mp3'), withPicture)).toBe(false);
		expect(judgeable(applyPreset('audio_wav'), withPicture)).toBe(false);
		for (const container of ['mp3', 'm4a', 'wav', 'flac'] as const)
			expect(judgeable({ container, video_codec: null }, withPicture), container).toBe(false);
	});

	test('a GIF is not: it is a loop, not an upload', () => {
		expect(judgeable(applyPreset('gif'), withPicture)).toBe(false);
		expect(judgeable({ container: 'mp4', video_codec: 'gif' }, withPicture)).toBe(false);
		expect(judgeable({ container: 'gif', video_codec: null }, withPicture)).toBe(false);
	});

	test('an empty timeline, an empty video track and a sound-only cut have no picture to judge', () => {
		expect(judgeable(applyPreset('web_1080p'), { tracks: [] } as Timeline)).toBe(false);
		expect(judgeable(applyPreset('web_1080p'), empty)).toBe(false);
		expect(judgeable(applyPreset('web_1080p'), soundOnly)).toBe(false);
	});

	test('with no export options (the docked panel) it is whether there is a picture', () => {
		expect(judgeable(null, withPicture)).toBe(true);
		expect(judgeable(null, soundOnly)).toBe(false);
		expect(judgeable(null, empty)).toBe(false);
	});

	test('every preset is judged exactly when it is a video one', () => {
		for (const p of PRESETS)
			expect(judgeable(applyPreset(p.id), withPicture), p.id).toBe(p.group !== 'Audio' && p.id !== 'gif');
	});
});

describe('the default 16:9 export, no shape ticked', () => {
	test('is one neutral line: the file, where it fits, and where to find the rest', () => {
		const v = verdict({});
		expect(v.groups).toHaveLength(1);
		expect(v.groups[0].shape).toBeNull();
		expect(v.groups[0].lines).toEqual([
			{
				level: 'note',
				text: '1920×1080, 16:9, 0:20. Fine for YouTube and the web. Tick a shape under Deliver to for Reels, Shorts or TikTok.'
			}
		]);
	});

	test('states the length as the timeline’s clock does: floored, so 20.5 s is 0:20', () => {
		expect(texts(verdict({ file: rep(1920, 1080, 20.5) }))[0]).toContain('16:9, 0:20.');
		expect(texts(verdict({ file: rep(1920, 1080, 59.9) }))[0]).toContain('16:9, 0:59.');
		expect(texts(verdict({ file: rep(1920, 1080, 3725) }))[0]).toContain('16:9, 62:05.');
	});

	test('carries no amber, no red and no tip: nothing about platforms nobody picked', () => {
		const v = verdict({});
		expect(levels(v)).toEqual(['note']);
		expect(v.tips).toEqual([]);
	});

	test('does not talk about letterboxing or the toolbar', () => {
		for (const t of texts(verdict({}))) {
			expect(t).not.toMatch(/letterbox/i);
			expect(t).not.toMatch(/toolbar/i);
		}
	});

	test('a shorter or longer cut is still only a neutral line: YouTube has no length limit', () => {
		expect(levels(verdict({ file: rep(1920, 1080, 2) }))).toEqual(['note']);
		expect(levels(verdict({ file: rep(1920, 1080, 3 * 3600) }))).toEqual(['note']);
		expect(texts(verdict({ file: rep(1920, 1080, 3 * 3600) }))[0]).toContain('180:00');
	});

	test('no frame in the project and any resolution the dialog offers: never amber on a 20 s cut', () => {
		for (const r of RESOLUTIONS) {
			if (!r.value) continue;
			const v = verdict({ file: rep(r.value[0], r.value[1]) });
			expect(levels(v), r.label).toEqual(['note']);
		}
	});

	test('a low resolution is a folded tip, not a warning', () => {
		const v = verdict({ file: rep(1280, 720) });
		expect(levels(v)).toEqual(['note']);
		expect(texts(v)[0]).toBe('1280×720, 16:9, 0:20. Fine for YouTube and the web. Tick a shape under Deliver to for Reels, Shorts or TikTok.');
		expect(v.tips).toHaveLength(1);
		expect(v.tips[0]).toContain('upscale');
	});
});

describe('a project already cut for a shape, no shape ticked', () => {
	test('a 1:1 cut is not told it is letterboxed: it is fine for the feed and Shorts', () => {
		const v = verdict({ file: rep(1080, 1080), project: null });
		expect(v.groups[0].lines).toEqual([
			{
				level: 'note',
				text: '1080×1080, 1:1, 0:20. Fine for YouTube Shorts and Instagram feed. Tick a shape under Deliver to for Reels or TikTok.'
			}
		]);
		expect(texts(v).join(' ')).not.toMatch(/letterbox|toolbar/i);
	});

	test('a 1:1 project exported as the 1:1 frame it is cut for says the same, with no bars warning', () => {
		const v = verdict({ file: rep(1080, 1080), project: rep(1080, 1080) });
		expect(levels(v)).toEqual(['note']);
	});

	test('a 9:16 cut is fine for the three vertical feeds and suggests nothing', () => {
		const v = verdict({ file: rep(1080, 1920) });
		expect(texts(v)).toEqual(['1080×1920, 9:16, 0:20. Fine for Instagram Reels, YouTube Shorts and TikTok.']);
	});

	test('a 4:5 cut is fine for the feed and Shorts, and suggests the vertical feeds', () => {
		const v = verdict({ file: rep(1080, 1350) });
		expect(texts(v)).toEqual([
			'1080×1350, 4:5, 0:20. Fine for YouTube Shorts and Instagram feed. Tick a shape under Deliver to for Reels or TikTok.'
		]);
	});

	test('a shape no platform takes says so plainly and stays neutral', () => {
		const v = verdict({ file: rep(1440, 1080) });
		expect(v.groups[0].lines).toEqual([
			{
				level: 'note',
				text: '1440×1080, 4:3, 0:20. No platform here takes this shape without bars. Tick a shape under Deliver to for Reels, Shorts or TikTok.'
			}
		]);
	});
});

describe('real limits are the only amber and red', () => {
	test('a 9:16 cut over three minutes: Reels is warned, Shorts rejects, TikTok is fine', () => {
		const v = verdict({ file: rep(1080, 1920, 4.5 * 60) });
		const [head, ...rest] = v.groups[0].lines;
		expect(head).toEqual({ level: 'note', text: '1080×1920, 9:16, 4:30. Fine for TikTok.' });
		expect(rest.map((l) => l.level)).toEqual(['warning', 'error']);
		expect(rest[0].text).toContain('Instagram Reels');
		expect(rest[0].text).toContain('1:30');
		expect(rest[1].text).toContain('YouTube Shorts');
		expect(rest[1].text).toContain('1:30 to fit');
	});

	test('a cut too short for a platform is an error on that platform only', () => {
		const v = verdict({ file: rep(1080, 1920, 2) });
		expect(v.groups[0].lines.map((l) => l.level)).toEqual(['note', 'error', 'error']);
		expect(v.groups[0].lines[0].text).toBe('1080×1920, 9:16, 0:02. Fine for YouTube Shorts.');
		expect(texts(v).slice(1).join(' ')).toContain('Instagram Reels');
		expect(texts(v).slice(1).join(' ')).toContain('TikTok');
	});

	test('past TikTok’s hour it is rejected once, with no reach warning besides', () => {
		const v = verdict({ file: rep(1080, 1920, 70 * 60) });
		const lines = v.groups[0].lines.slice(1);
		expect(lines.filter((l) => l.text.includes('TikTok')).map((l) => l.level)).toEqual(['error']);
	});

	test('the length is the marked range’s, as the check was handed it', () => {
		const long = verdict({ file: rep(1080, 1920, 4.5 * 60) });
		const marked = verdict({ file: report({ ...cut(1080, 1920, 4.5 * 60), duration: 20 }) });
		expect(levels(long)).toContain('error');
		expect(levels(marked)).toEqual(['note']);
		expect(texts(marked)[0]).toBe('1080×1920, 9:16, 0:20. Fine for Instagram Reels, YouTube Shorts and TikTok.');
	});
});

describe('shapes ticked under Deliver to', () => {
	test('9:16 ticked is judged for the platforms that take it, and only those', () => {
		const v = verdict({ shapes: [shape('9:16')] });
		expect(v.groups).toEqual([
			{ shape: '9:16', lines: [{ level: 'ok', text: 'Ready for Instagram Reels · YouTube Shorts · TikTok' }] }
		]);
		expect(texts(v).join(' ')).not.toMatch(/YouTube(?! Shorts)|Instagram feed|letterbox/);
	});

	test('the single file is not judged once shapes are ticked: each shape brings its own frame', () => {
		const v = verdict({ file: rep(1920, 1080), shapes: [shape('9:16')] });
		expect(v.groups).toHaveLength(1);
		expect(texts(v).join(' ')).not.toContain('1920');
	});

	test('several shapes are one group each, in the order given', () => {
		const v = verdict({ shapes: [shape('16:9'), shape('9:16'), shape('1:1'), shape('4:5')] });
		expect(v.groups.map((g) => g.shape)).toEqual(['16:9', '9:16', '1:1', '4:5']);
		expect(v.groups.map((g) => g.lines[0].text)).toEqual([
			'Ready for YouTube',
			'Ready for Instagram Reels · YouTube Shorts · TikTok',
			'Ready for YouTube Shorts · Instagram feed',
			'Ready for YouTube Shorts · Instagram feed'
		]);
		expect(levels(v).every((l) => l === 'ok')).toBe(true);
	});

	test('a long cut keeps what the old line hid: Reels reach and the Shorts cap', () => {
		const v = verdict({ shapes: [shape('9:16', 4.5 * 60)] });
		expect(v.groups[0].lines.map((l) => l.level)).toEqual(['ok', 'warning', 'error']);
		expect(v.groups[0].lines[0].text).toBe('Ready for TikTok');
	});

	test('a short cut is rejected where the platform has a minimum and still ready where it has none', () => {
		const v = verdict({ shapes: [shape('9:16', 2)] });
		expect(v.groups[0].lines.map((l) => l.level)).toEqual(['ok', 'error', 'error']);
		expect(v.groups[0].lines[0].text).toBe('Ready for YouTube Shorts');
	});

	test('a shape every platform rejects has no ready line, only the limits', () => {
		const v = verdict({ shapes: [shape('4:5', 3 * 3600)] });
		expect(v.groups[0].lines.every((l) => l.level === 'error')).toBe(true);
		expect(v.groups[0].lines).toHaveLength(2);
	});

	test('the captions tip folds away and is not about YouTube', () => {
		const feeds = verdict({ shapes: [shape('9:16')] });
		expect(feeds.tips).toHaveLength(1);
		expect(feeds.tips[0]).toContain('muted');
		expect(verdict({ shapes: [shape('16:9')] }).tips).toEqual([]);
		const captioned = platformVerdict({
			file: rep(1920, 1080),
			project: null,
			shapes: [{ label: '9:16', report: rep(1080, 1920, 20, { has_text: true }) }]
		})!;
		expect(captioned.tips).toEqual([]);
	});
});

describe('nothing to judge', () => {
	test('an empty cut says nothing, single file or shapes', () => {
		expect(platformVerdict({ file: rep(1920, 1080, 0), project: null, shapes: [] })).toBeNull();
		expect(platformVerdict({ file: rep(1920, 1080, 0), project: null, shapes: [{ label: '9:16', report: rep(1080, 1920, 0) }] })).toBeNull();
	});
});

describe('a project cut for a shape, exported wider', () => {
	const vertical = rep(1080, 1920);

	test('says what the report says, in the platforms’ words, with the way out', () => {
		const v = verdict({ file: rep(1920, 1080), project: vertical });
		const warn = v.groups[0].lines.filter((l) => l.level === 'warning');
		expect(warn).toEqual([
			{
				level: 'warning',
				text: 'Instagram Reels, YouTube Shorts and TikTok are vertical (9:16). This 16:9 cut would be letterboxed. Tick 9:16 under Deliver to to export a vertical version.'
			}
		]);
	});

	test('does not also send the user to Deliver to for the same thing', () => {
		const v = verdict({ file: rep(1920, 1080), project: vertical });
		expect(v.groups[0].lines[0].text).toBe('1920×1080, 16:9, 0:20. Fine for YouTube and the web.');
	});

	test('never mentions the toolbar', () => {
		expect(texts(verdict({ file: rep(1920, 1080), project: vertical })).join(' ')).not.toMatch(/toolbar/i);
	});

	test('a square project exported as 16:9 names the platforms that take square', () => {
		const v = verdict({ file: rep(1920, 1080), project: rep(1080, 1080) });
		expect(v.groups[0].lines.at(-1)).toEqual({
			level: 'warning',
			text: 'YouTube Shorts and Instagram feed take 1:1. This 16:9 cut would be letterboxed. Tick 1:1 under Deliver to to export a square version.'
		});
	});

	test('a 16:9 project exported 9:16 is a reframe the user asked for, not a warning', () => {
		const v = verdict({ file: rep(1080, 1920), project: rep(1920, 1080) });
		expect(levels(v)).toEqual(['note']);
	});

	test('a project exported at its own shape, or a narrower one, is silent', () => {
		expect(levels(verdict({ file: rep(1080, 1920), project: vertical }))).toEqual(['note']);
		expect(levels(verdict({ file: rep(1080, 1920), project: rep(1080, 1350) }))).toEqual(['note']);
	});

	test('a wider shape that some of the platforms still take names only the ones it bars', () => {
		const v = verdict({ file: rep(1080, 1350), project: vertical });
		expect(v.groups[0].lines.at(-1)?.text).toBe(
			'Instagram Reels and TikTok are vertical (9:16). This 4:5 cut would be letterboxed. Tick 9:16 under Deliver to to export a vertical version.'
		);
	});

	test('a project shape no platform takes has nothing to say', () => {
		expect(levels(verdict({ file: rep(1920, 1080), project: rep(1440, 1080) }))).toEqual(['note']);
	});
});

describe('where the check is wired', () => {
	const read = (path: string) => readFileSync(join(SRC, 'lib/components/editor', path), 'utf8');

	test('the dialog draws it only for a judgeable export, and the panel only for a picture', () => {
		const dialog = read('ExportDialog.svelte');
		expect(dialog).toContain("judgeable(opts, editor.timeline)");
		expect(dialog.match(/<Readiness/g)).toHaveLength(1);
		expect(dialog).toMatch(/\{#if judged\}\s*<Readiness/);
		expect(read('DeliverPanel.svelte')).toMatch(/\{#if picture\}\s*<Readiness/);
	});

	test('the dialog hands it the frame and the range it will write', () => {
		expect(read('ExportDialog.svelte')).toContain('<Readiness frame={opts.resolution ?? null} range={useRange ? marks : null} />');
	});

	test('the old heading and the toolbar pointer are gone everywhere', () => {
		const offenders = uiSources().filter((f) => /Where it is going|Pick a delivery frame in the toolbar/.test(readFileSync(f, 'utf8')));
		expect(offenders).toEqual([]);
		expect(read('Readiness.svelte')).toContain("heading = 'Platform check'");
	});
});
