import { beforeEach, describe, expect, test } from 'bun:test';
import './test-runes';
import { getHistory, revertTo, setRippleMode, unlinkClips } from './api';

// The editor's animated-title actions and selection over the browser harness's cut — V1 `c1 [0, 12.5)`
// `c2 [12.5, 20.5)` over A1 `c3 [0, 12.5)` — which applies kerf-core's rules to them. An animated title is
// a third kind of subject beside a clip and a text title: one at a time, and gone from the selection
// when the edit that removes it lands.

const { editor } = await import('./state.svelte');
const revision = async () => (await getHistory()).find((r) => r.current)!.seq;

const CUSTOM = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50"/></svg>';
const preset = (text = 'Hello') => ({ preset: { kind: 'fade_up' as const, params: { text } } });

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
	await unlinkClips(['c1']);
	await editor.load();
	editor.clearSelection();
});

describe('animated titles in the editor', () => {
	test('start absent, and the list follows the timeline', async () => {
		expect(editor.svgOverlays).toEqual([]);
		await editor.addSvgOverlay(preset(), 1);
		expect(editor.svgOverlays.map((o) => o.preset?.params.text)).toEqual(['Hello']);
	});

	test('adding one is one revision, and addSvgTitle selects what it made', async () => {
		const before = await revision();
		const made = await editor.addSvgTitle(preset('Intro'), 2, 5, { name: 'Opener' });
		expect(await revision()).toBe(before + 1);
		expect(made).toMatchObject({ name: 'Opener', start: 2, end: 5 });
		expect(editor.selectedSvgOverlayId).toBe(made!.id);
		expect(editor.selectedSvgOverlay?.id).toBe(made!.id);
	});

	test('an update is one revision and keeps the selection', async () => {
		const made = (await editor.addSvgTitle(preset(), 0))!;
		const before = await revision();
		await editor.updateSvgOverlay(made.id, { x: 0.25, y: 0.5, width: 0.5, height: 0.25 });
		expect(await revision()).toBe(before + 1);
		expect(editor.selectedSvgOverlay).toMatchObject({ x: 0.25, y: 0.5, width: 0.5, height: 0.25 });
	});

	test('a refused update changes nothing and writes no revision', async () => {
		const made = (await editor.addSvgTitle(preset(), 0))!;
		const before = await revision();
		await expect(editor.updateSvgOverlay(made.id, { width: 0 })).rejects.toThrow();
		expect(await revision()).toBe(before);
		expect(editor.selectedSvgOverlay?.width).toBe(1);
	});

	test('removing the selected one clears the selection', async () => {
		const made = (await editor.addSvgTitle(preset(), 0))!;
		await editor.removeSvgOverlay(made.id);
		expect(editor.svgOverlays).toEqual([]);
		expect(editor.selectedSvgOverlayId).toBeNull();
	});

	test('removing another one leaves the selection alone', async () => {
		const a = (await editor.addSvgTitle(preset('A'), 0))!;
		const b = (await editor.addSvgTitle(preset('B'), 1))!;
		editor.selectSvgOverlay(a.id);
		await editor.removeSvgOverlay(b.id);
		expect(editor.selectedSvgOverlayId).toBe(a.id);
	});
});

describe('one subject at a time', () => {
	test('selecting an animated title drops the clips and the text title', async () => {
		await editor.addTitle('Text', 0, 3);
		const svg = (await editor.addSvgTitle(preset(), 0))!;
		editor.selectClip('c2');
		editor.selectOverlay(editor.overlays[0].id);
		expect(editor.selectedSvgOverlayId).toBeNull();
		editor.selectSvgOverlay(svg.id);
		expect(editor.selectedSvgOverlayId).toBe(svg.id);
		expect(editor.selectedClipIds).toEqual([]);
		expect(editor.selectedClipId).toBeNull();
		expect(editor.selectedOverlayId).toBeNull();
	});

	test('selecting a clip, several, all, or a text title drops the animated one', async () => {
		const svg = (await editor.addSvgTitle(preset(), 0))!;
		for (const pick of [
			() => editor.selectClip('c2'),
			() => editor.selectClips(['c2', 'c3']),
			() => editor.selectAll(),
			() => editor.selectOverlay('x'),
			() => editor.clearSelection()
		]) {
			editor.selectSvgOverlay(svg.id);
			pick();
			expect(editor.selectedSvgOverlayId).toBeNull();
		}
	});

	test('selecting nothing is allowed', async () => {
		const svg = (await editor.addSvgTitle(preset(), 0))!;
		editor.selectSvgOverlay(null);
		expect(editor.selectedSvgOverlayId).toBeNull();
		expect(editor.svgOverlays.map((o) => o.id)).toEqual([svg.id]);
	});
});

describe('a selected title that is gone', () => {
	test('an undo that takes it away takes it out of the selection', async () => {
		const svg = (await editor.addSvgTitle(preset(), 0))!;
		expect(editor.selectedSvgOverlayId).toBe(svg.id);
		await editor.undo();
		expect(editor.svgOverlays).toEqual([]);
		expect(editor.selectedSvgOverlayId).toBeNull();
		expect(editor.selectedSvgOverlay).toBeUndefined();
	});

	test('a refresh that no longer has it does the same', async () => {
		const svg = (await editor.addSvgTitle({ custom: CUSTOM }, 0))!;
		await revertTo(0);
		await editor.refreshTimeline();
		expect(editor.selectedSvgOverlayId).toBeNull();
		expect(editor.svgOverlays.some((o) => o.id === svg.id)).toBe(false);
	});
});

describe('work that exists only in memory', () => {
	test('an animated title alone is work', async () => {
		await editor.addSvgOverlay(preset(), 0);
		const there = editor.timeline;
		editor.timeline = { tracks: [], svg_overlays: there.svg_overlays };
		editor.assets = [];
		expect(editor.hasUnsavedWork).toBe(true);
		editor.timeline = { tracks: [] };
		expect(editor.hasUnsavedWork).toBe(false);
	});
});
