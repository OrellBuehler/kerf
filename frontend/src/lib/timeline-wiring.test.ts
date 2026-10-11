import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const src = readFileSync(join(import.meta.dir, 'components/editor/Timeline.svelte'), 'utf8');

/** The body of `function name(...)` up to the next top-level function of the script. */
function bodyOf(name: string): string {
	const at = src.indexOf(`function ${name}(`);
	expect(at, name).toBeGreaterThan(-1);
	const next = src.indexOf('\n\tfunction ', at + 1);
	return src.slice(at, next < 0 ? undefined : next);
}

describe('the timeline draws one track order', () => {
	test('headers, lanes and the overview strip all go through displayTracks', () => {
		// The video group is drawn highest layer first: two lists that disagree put a header
		// beside the wrong lane, and an edit on the track the user did not point at.
		expect(src.match(/\{#each drawn as t \(t\.id\)\}/g)).toHaveLength(2);
		expect(src).toContain('tracks={drawn}');
		expect(src.match(/\{#each editor\.timeline\.tracks as /g)).toBeNull();
		expect(src).toContain('const drawn = $derived(displayTracks(editor.timeline.tracks));');
	});
});

describe('a click selects and leaves the playhead where it was parked', () => {
	// Premiere, Resolve and FCP all do: park the playhead, pick a clip, trim to the playhead.
	test('a press on a clip, a trim-tool press that never drags and a title click do not seek', () => {
		for (const fn of ['onClipPointerDown', 'onTrimToolUp', 'onTitleDragUp', 'onPointerUp']) {
			const body = bodyOf(fn);
			expect(body.includes('ui.seek(d.origStart'), fn).toBe(false);
			expect(body.includes('ui.seek(g.pressTime)'), fn).toBe(false);
			expect(body.includes('ui.seek(laneTime('), fn).toBe(false);
		}
	});

	test('the click that clears a selection does not also seek', () => {
		const body = bodyOf('onLaneSeek');
		expect(body.indexOf('clearSelection()')).toBeGreaterThan(-1);
		expect(body.indexOf('clearSelection()')).toBeLessThan(body.indexOf('ui.seek('));
		expect(body).toMatch(/clearSelection\(\);\s*return;/);
	});
});

describe('gestures hand autoscroll the pointer, not an event', () => {
	test('every move handler takes the pointer-like slice an event and a re-read both fit', () => {
		for (const fn of ['onPointerMove', 'onTrimMove', 'onTitleDragMove', 'onTrimToolMove']) {
			expect(bodyOf(fn).split('\n')[0], fn).toContain('PointerLike');
		}
	});
});
