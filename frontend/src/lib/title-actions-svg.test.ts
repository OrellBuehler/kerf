import { beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import { sampleSvg } from './svg-titles';

// The animated-title actions as the Titles controls, the lane menu and the Inspector run them, over the
// browser harness's backend (which applies kerf-core's rules to the dev timeline). Only the toasts are
// stood in for: `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.

const shown: { kind: string; text: string }[] = [];
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

const { editor } = await import('./state.svelte');
const { ui } = await import('./editor-ui.svelte');
const actions = await import('./title-actions');
const { getHistory, revertTo, setDeliveryFormat } = await import('./api');
const revision = async () => (await getHistory()).find((r) => r.current)!.seq;

const BROKEN = '<html></html>';
const SCRIPTED =
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><script>alert(1)</script><rect width="100" height="50"/></svg>';

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	// `duration` is a derived value, which the runes stand-in computes once, at construction: the cut's.
	(editor as { duration: number }).duration = 20.5;
	ui.svgSourceFocus = null;
	ui.time = 0;
	shown.length = 0;
});

describe('adding', () => {
	test('a preset starts at the playhead, is filled with its sample, and is selected', async () => {
		ui.time = 2.5;
		await actions.addAnimatedTitle('lower_third');
		const [o] = editor.svgOverlays;
		expect(o.start).toBe(2.5);
		expect(o.preset).toMatchObject({ kind: 'lower_third', params: { text: 'Your name', subtitle: 'Your role' } });
		expect(editor.selectedSvgOverlayId).toBe(o.id);
		expect(shown).toEqual([]);
	});

	test('each of the four kinds adds', async () => {
		for (const kind of ['fade_up', 'typewriter', 'lower_third', 'word_pop'] as const) await actions.addAnimatedTitle(kind);
		expect(editor.svgOverlays.map((o) => o.preset?.kind)).toEqual(['fade_up', 'typewriter', 'lower_third', 'word_pop']);
	});

	test('an add is one revision', async () => {
		const before = await revision();
		await actions.addAnimatedTitle('word_pop');
		expect(await revision()).toBe(before + 1);
	});

	test('custom starts from a sample sized to the frame, selected, and asks the Inspector for its source', async () => {
		await actions.addCustomSvgTitle();
		const [o] = editor.svgOverlays;
		expect(o.preset).toBeUndefined();
		expect(o.svg).toBe(sampleSvg(1920, 1080));
		expect(editor.selectedSvgOverlayId).toBe(o.id);
		expect(ui.svgSourceFocus).toBe(o.id);
	});

	test('a vertical project gets a vertical sample', async () => {
		await setDeliveryFormat({ width: 1080, height: 1920, fit: 'cover' });
		await editor.load();
		await actions.addCustomSvgTitle();
		expect(editor.svgOverlays[0].svg).toContain('viewBox="0 0 1080 1920"');
	});

	test('a refusal is a toast, not a throw', async () => {
		for (let i = 0; i < 50; i++) await editor.addSvgOverlay({ custom: sampleSvg(10, 10) }, 0);
		await actions.addAnimatedTitle('fade_up');
		expect(shown.map((n) => n.kind)).toEqual(['error']);
		expect(shown[0].text).toContain('50');
		expect(editor.svgOverlays).toHaveLength(50);
	});
});

describe('picking from the list', () => {
	test('selects, and brings the playhead into the title', async () => {
		await editor.addSvgOverlay({ custom: sampleSvg(1920, 1080) }, 6, 9);
		const [o] = editor.svgOverlays;
		ui.time = 1;
		actions.pickSvgTitle(o);
		expect(editor.selectedSvgOverlayId).toBe(o.id);
		expect(ui.time).toBeGreaterThanOrEqual(6);
		expect(ui.time).toBeLessThanOrEqual(9);
	});

	test('leaves the playhead alone when it is inside already, and picking again lets go', async () => {
		await editor.addSvgOverlay({ custom: sampleSvg(1920, 1080) }, 0, 9);
		const [o] = editor.svgOverlays;
		ui.time = 4;
		actions.pickSvgTitle(o);
		expect(ui.time).toBe(4);
		actions.pickSvgTitle(o);
		expect(editor.selectedSvgOverlayId).toBeNull();
	});
});

describe('applying source', () => {
	test('asks the backend’s reading first, then writes, and hands back the warnings', async () => {
		await editor.addSvgOverlay({ custom: sampleSvg(1920, 1080) }, 0, 4);
		const [o] = editor.svgOverlays;
		const check = await actions.applySvgSource(o.id, SCRIPTED);
		expect(check.warnings.some((w) => w.includes('script'))).toBe(true);
		expect(editor.svgOverlays[0].svg).toBe(check.svg);
		expect(editor.svgOverlays[0].svg).not.toContain('<script');
	});

	test('source the backend refuses is never written, and is no revision', async () => {
		await editor.addSvgOverlay({ custom: sampleSvg(1920, 1080) }, 0, 4);
		const [o] = editor.svgOverlays;
		const before = await revision();
		await expect(actions.applySvgSource(o.id, BROKEN)).rejects.toThrow('<svg');
		expect(editor.svgOverlays[0].svg).toBe(o.svg);
		expect(await revision()).toBe(before);
	});

	test('an apply is one revision', async () => {
		await editor.addSvgOverlay({ custom: sampleSvg(1920, 1080) }, 0, 4);
		const [o] = editor.svgOverlays;
		const before = await revision();
		await actions.applySvgSource(o.id, sampleSvg(1280, 720));
		expect(await revision()).toBe(before + 1);
	});
});

describe('a preset made custom', () => {
	test('keeps its artwork as the source, drops the preset, and asks for the source box', async () => {
		await actions.addAnimatedTitle('fade_up');
		const [o] = editor.svgOverlays;
		const before = await revision();
		await actions.convertToCustom(o);
		const [now] = editor.svgOverlays;
		expect(now.id).toBe(o.id);
		expect(now.preset).toBeUndefined();
		expect(now.svg).toBe(o.svg);
		expect(ui.svgSourceFocus).toBe(o.id);
		expect(await revision()).toBe(before + 1);
	});
});

describe('changing the preset', () => {
	test('is a new title with the same text, timing, placement and name, in place of the old one', async () => {
		await editor.addSvgOverlay({ preset: { kind: 'fade_up', params: { text: 'Hello', color: 'teal', accent: 'gold' } } }, 2, 7, {
			name: 'Opener',
			x: 0.1,
			y: 0.2,
			width: 0.5,
			height: 0.4,
			opacity: 0.8
		});
		const [old] = editor.svgOverlays;
		await actions.changePreset(old, 'typewriter');
		expect(editor.svgOverlays).toHaveLength(1);
		const [o] = editor.svgOverlays;
		expect(o.id).not.toBe(old.id);
		expect(o).toMatchObject({ name: 'Opener', start: 2, end: 7, x: 0.1, y: 0.2, width: 0.5, height: 0.4, opacity: 0.8 });
		expect(o.preset).toMatchObject({ kind: 'typewriter', params: { text: 'Hello', color: 'teal', accent: 'gold' } });
		expect(editor.selectedSvgOverlayId).toBe(o.id);
	});

	test('the same preset, or a custom title, changes nothing', async () => {
		await actions.addAnimatedTitle('fade_up');
		const [o] = editor.svgOverlays;
		const before = await revision();
		await actions.changePreset(o, 'fade_up');
		await actions.changePreset({ ...o, preset: undefined }, 'word_pop');
		expect(await revision()).toBe(before);
		expect(editor.svgOverlays).toHaveLength(1);
	});
});

describe('the preset listing and the thumbnails', () => {
	test('the listing is asked for once', async () => {
		const first = await actions.svgPresetInfos();
		expect(first.map((i) => i.kind)).toEqual(['fade_up', 'typewriter', 'lower_third', 'word_pop']);
		expect(await actions.svgPresetInfos()).toBe(first);
	});

	test('a thumbnail is an <img> data URL of a preset drawn to hold, once per frame shape', async () => {
		const url = await actions.presetThumbnail('lower_third');
		expect(url.startsWith('data:image/svg+xml,')).toBe(true);
		expect(decodeURIComponent(url.slice('data:image/svg+xml,'.length))).toContain('Your name');
		expect(await actions.presetThumbnail('lower_third')).toBe(url);
	});
});
