import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { trackFlagCopy, type FlagTrack } from './track-flags';

const track = (over: Partial<FlagTrack> = {}): FlagTrack => ({ name: 'A1', kind: 'audio', ...over });

describe('trackFlagCopy', () => {
	test('every flag is named for its track, so five tracks are not five "Toggle mute"s', () => {
		for (const name of ['A1', 'V2', 'Music']) {
			const copy = trackFlagCopy(track({ name }));
			for (const [which, c] of Object.entries(copy)) expect(c.name, which).toContain(name);
		}
		const a1 = trackFlagCopy(track());
		expect(a1.mute.name).toBe('Mute A1');
		expect(a1.solo.name).toBe('Solo A1');
		expect(a1.lock.name).toBe('Lock A1');
		expect(a1.duck.name).toBe('Duck A1');
		expect(a1.remove.name).toBe('Remove A1');
	});

	test('a picture track is hidden, not muted', () => {
		const v = trackFlagCopy(track({ name: 'V1', kind: 'video' }));
		expect(v.mute.name).toBe('Hide V1');
		expect(v.mute.tip).toBe('Hide this track');
		expect(trackFlagCopy(track({ name: 'V1', kind: 'video', muted: true })).mute.tip).toBe('Hidden — click to show');
	});

	test('the name is the same on and off (the pressed state says which way it is); the tooltip says what a click does now', () => {
		const off = trackFlagCopy(track());
		const on = trackFlagCopy(track({ muted: true, solo: true, locked: true, duck: true }));
		for (const k of ['mute', 'solo', 'lock', 'duck', 'remove'] as const) expect(on[k].name).toBe(off[k].name);
		expect(off.mute.tip).toBe('Mute this track');
		expect(on.mute.tip).toBe('Muted — click to unmute');
		expect(on.solo.tip).toBe('Soloed — click to clear');
		expect(on.lock.tip).toBe('Locked — click to unlock');
		expect(on.duck.tip).toContain('Ducking on');
	});
});

describe('the timeline’s track headers', () => {
	const EDITOR = join(import.meta.dir, 'components/editor');
	const timeline = readFileSync(join(EDITOR, 'Timeline.svelte'), 'utf8');
	const flags = readFileSync(join(EDITOR, 'TrackFlags.svelte'), 'utf8');

	test('the flags are TrackFlags, not five raw buttons that all say "Toggle …"', () => {
		expect(timeline).toContain('<TrackFlags track={t} onremove={onRemoveTrack} />');
		for (const old of ['Toggle mute', 'Toggle solo', 'Toggle lock', 'Toggle ducking', 'aria-label="Remove track"', 'flagBtn'])
			expect(timeline, old).not.toContain(old);
	});

	test('each flag is an IconBtn named from the track and pending while an edit is in flight', () => {
		const buttons = flags.match(/<IconBtn\b[\s\S]*?>/g) ?? [];
		expect(buttons).toHaveLength(5);
		for (const b of buttons) {
			expect(b).toContain('{pending}');
			expect(b).toMatch(/aria-label=\{copy\.\w+\.name\}/);
		}
		expect(flags).toContain('const pending = $derived(editor.locked);');
	});

	test('the header’s two sliders put the value back when a change arrives while an edit is in flight', () => {
		// (that nothing here is natively `disabled`, and that the rename field is named, are `pending-controls` / `a11y-guard`'s)
		expect(timeline.match(/aria-disabled=\{editor\.locked \? 'true' : undefined\}/g)).toHaveLength(2);
		expect(timeline).toContain('if (editor.locked) return void (e.currentTarget.value = String(gainToFader(t.volume ?? 1)));');
		expect(timeline).toContain('if (editor.locked) return void (e.currentTarget.value = String(t.pan ?? 0));');
	});

	test('both "Add a title" buttons are pending, not disabled, while an edit is in flight', () => {
		expect(timeline.match(/aria-label="Add a title"/g)).toHaveLength(2);
		expect(timeline.match(/pending=\{editor\.busy\}/g)).toHaveLength(2);
	});
});
