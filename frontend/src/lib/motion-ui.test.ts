import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
	MOTION_GROUPS,
	MOTION_MODES,
	clampedNotice,
	lengthText,
	motionOptions,
	motionReason,
	motionTargets,
	parseMotionLength,
	presetTip
} from './motion-ui';
import { motionPresets } from './motion-presets';
import type { Clip, Timeline } from './types';

const clip = (id: string): Clip =>
	({ id, asset_id: 'a', source_in: 0, source_out: 5, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 }) as Clip;

const timeline: Timeline = {
	tracks: [
		{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('p1'), clip('p2')] },
		{ id: 'a1', kind: 'audio', name: 'A1', clips: [clip('s1')] }
	]
};
const pick = (...ids: string[]) => timeline.tracks.flatMap((t) => t.clips).filter((c) => ids.includes(c.id));

describe('which clips a preset goes into', () => {
	test('the picture clips of the selection: a sound has none to move', () => {
		expect(motionTargets(timeline, pick('p1', 's1', 'p2')).map((c) => c.id)).toEqual(['p1', 'p2']);
		expect(motionTargets(timeline, pick('s1'))).toEqual([]);
		expect(motionTargets(timeline, [])).toEqual([]);
	});

	test('the tiles are off, with the reason, until a picture clip is selected', () => {
		expect(motionReason(timeline, [])).toBe('Select a clip on the timeline to give it a motion.');
		expect(motionReason(timeline, pick('s1'))).toBe('This clip is a sound — select a picture clip.');
		expect(motionReason(timeline, [...pick('s1'), { ...clip('s2') }])).toBe('These clips are sounds — select picture clips.');
		expect(motionReason(timeline, pick('p1'))).toBeNull();
		expect(motionReason(timeline, pick('p1', 's1'))).toBeNull();
	});
});

describe('the options a click sends', () => {
	test('leave the defaults to the backend', () => {
		expect(motionOptions('add', null, 1)).toEqual({ mode: 'add' });
		expect(motionOptions('replace', 0.75, 1.5)).toEqual({ mode: 'replace', duration: 0.75, intensity: 1.5 });
	});

	test('the length field: blank is the preset’s own, a number from a hundredth of a second is that, the rest is no entry', () => {
		expect(parseMotionLength('')).toBeNull();
		expect(parseMotionLength('   ')).toBeNull();
		expect(parseMotionLength('0.8')).toBe(0.8);
		expect(parseMotionLength('1,5')).toBe(1.5);
		expect(parseMotionLength('0.01')).toBe(0.01);
		for (const bad of ['0', '-1', '0.001', 'abc', '1e999', 'NaN']) expect(parseMotionLength(bad)).toBeUndefined();
		expect(lengthText(null)).toBe('');
		expect(lengthText(0.5)).toBe('0.5');
		expect(lengthText(1 / 3)).toBe('0.333');
	});

	test('both modes are offered, Add first', () => {
		expect(MOTION_MODES.map((m) => m.value)).toEqual(['add', 'replace']);
		for (const m of MOTION_MODES) expect(m.hint.length).toBeGreaterThan(20);
	});
});

describe('the groups and the tooltips', () => {
	test('three groups hold every preset once, in the backend’s order', () => {
		const list = motionPresets();
		expect(MOTION_GROUPS.map((g) => g.kind)).toEqual(['entrance', 'exit', 'emphasis']);
		const grouped = MOTION_GROUPS.flatMap((g) => list.filter((p) => p.kind === g.kind).map((p) => p.id));
		expect(grouped).toEqual(list.map((p) => p.id));
	});

	test('a tooltip says how long and where, or why the tile is off', () => {
		const list = motionPresets();
		const [fade, out, push] = ['fade_in', 'fade_out', 'push_in'].map((id) => list.find((p) => p.id === id)!);
		expect(presetTip(fade, null, null)).toEqual({
			text: 'Fade in',
			detail: "Opacity rises from 0 to the clip's own. 0.5 s, from the clip’s start."
		});
		expect(presetTip(out, 1.25, null).detail).toBe("Opacity falls from the clip's own to 0. 1.25 s, ending at the clip’s end.");
		expect(presetTip(push, null, null).detail).toContain('Runs over the whole clip.');
		expect(presetTip(fade, null, 'Select a clip on the timeline to give it a motion.')).toEqual({
			text: 'Fade in',
			detail: 'Select a clip on the timeline to give it a motion.'
		});
	});
});

describe('what is said afterwards', () => {
	const window = (start: number, end: number) => ({ clip_id: 'x', start, end });

	test('nothing, unless a clip cut the window short', () => {
		expect(clampedNotice('fade_in', [window(0, 0.5), window(0, 0.5)], null)).toBeNull();
		expect(clampedNotice('slide_out_to_left', [window(3.4, 4)], null)).toBeNull();
		expect(clampedNotice('push_in', [window(0, 1.5)], null)).toBeNull();
		expect(clampedNotice('fade_in', [window(0, 2)], 2)).toBeNull();
	});

	test('a short clip says so, with the length it got', () => {
		expect(clampedNotice('fade_in', [window(0, 0.25)], null)).toBe('Fade in shortened to 0.25 s to fit the clip');
		expect(clampedNotice('fade_out', [window(2, 10)], 20)).toBe('Fade out shortened to 8 s to fit the clip');
		expect(clampedNotice('fade_in', [window(0, 0.5), window(0, 0.3), window(0, 0.2)], null)).toBe(
			'Fade in shortened to fit 2 of 3 clips, down to 0.2 s'
		);
	});
});

describe('the tab', () => {
	const tab = readFileSync(new URL('./components/editor/MotionTab.svelte', import.meta.url), 'utf8');

	test('is on the library rail, hooked to the same applyMotion the harness and the app run', () => {
		const panel = readFileSync(new URL('./components/editor/LibraryPanel.svelte', import.meta.url), 'utf8');
		expect(panel).toContain("tab === 'motion'");
		expect(panel).toContain('<MotionTab />');
		expect(tab).toContain('applyMotion(p.id)');
	});

	test('names its controls and speaks only in tokens', () => {
		for (const name of ['aria-label="Motion length in seconds', 'label="Motion mode"', 'label="Strength"', 'role="group"']) {
			expect(tab).toContain(name);
		}
		expect(tab).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgba?\(/);
	});
});
