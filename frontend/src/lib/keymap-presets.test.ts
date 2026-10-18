import { describe, expect, test } from 'bun:test';
import {
	ACTIONS,
	KEY_PRESETS,
	actionDef,
	emptyOverrides,
	findCollisions,
	keyPreset,
	matchAction,
	parseChord,
	parseKeyOverrides,
	presetOf,
	presetOverrides,
	presetTable,
	resolveBindings,
	setActionChords,
	type KeyEventLike,
	type Platform
} from './keymap';

const PLATFORMS: Platform[] = ['mac', 'other'];

const ev = (key: string, m: { ctrl?: boolean; meta?: boolean; alt?: boolean; shift?: boolean } = {}): KeyEventLike => ({
	key,
	ctrlKey: !!m.ctrl,
	metaKey: !!m.meta,
	altKey: !!m.alt,
	shiftKey: !!m.shift
});

describe('keyboard presets', () => {
	test('Kerf is first and is the defaults', () => {
		expect(KEY_PRESETS[0].id).toBe('kerf');
		for (const p of PLATFORMS) expect(presetOverrides(KEY_PRESETS[0], p)).toEqual(emptyOverrides());
		expect(new Set(KEY_PRESETS.map((p) => p.id)).size).toBe(KEY_PRESETS.length);
		expect(KEY_PRESETS.map((p) => p.label)).toEqual(['Kerf', 'Premiere Pro', 'DaVinci Resolve', 'Final Cut Pro']);
	});

	test('every preset names actions Kerf has, in chords that read', () => {
		for (const preset of KEY_PRESETS) {
			for (const table of [preset.bindings, preset.mac ?? {}]) {
				for (const [id, chords] of Object.entries(table)) {
					expect([preset.id, id, !!actionDef(id)]).toEqual([preset.id, id, true]);
					for (const p of PLATFORMS) for (const c of chords) expect([preset.id, id, c, parseChord(c, p) !== null]).toEqual([preset.id, id, c, true]);
				}
			}
		}
	});

	test('no preset has a conflict, and resolving one drops nothing', () => {
		for (const preset of KEY_PRESETS) {
			for (const p of PLATFORMS) {
				const bindings = resolveBindings(presetOverrides(preset, p), p);
				expect([preset.id, p, findCollisions(bindings)]).toEqual([preset.id, p, []]);
				const table = presetTable(preset, p);
				for (const a of ACTIONS) {
					const want = table[a.id] ?? a.defaults;
					expect([preset.id, p, a.id, bindings[a.id].length]).toEqual([preset.id, p, a.id, want.length]);
				}
			}
		}
	});

	test('a preset survives being stored and read back, and is recognised', () => {
		for (const preset of KEY_PRESETS) {
			for (const p of PLATFORMS) {
				const o = presetOverrides(preset, p);
				const back = parseKeyOverrides(JSON.parse(JSON.stringify(o)), p);
				expect(back).toEqual(o);
				expect(presetOf(back, p)).toBe(preset.id);
			}
		}
	});

	test('a keyboard changed after a preset is no preset', () => {
		const p: Platform = 'other';
		const premiere = presetOverrides(keyPreset('premiere')!, p);
		const changed = setActionChords(premiere, 'marker.add', [parseChord('F5', p)!], p);
		expect(presetOf(changed, p)).toBeNull();
		expect(presetOf(emptyOverrides(), p)).toBe('kerf');
	});

	test('the keys do what the editor they come from does', () => {
		const premiere = resolveBindings(presetOverrides(keyPreset('premiere')!, 'mac'), 'mac');
		expect(matchAction(ev('m', { shift: true }), premiere)).toBe('marker.next');
		expect(matchAction(ev('i', { alt: true }), premiere)).toBe('range.clearIn');
		expect(matchAction(ev('m', { meta: true }), premiere)).toBe('file.export');
		const premiereWin = resolveBindings(presetOverrides(keyPreset('premiere')!, 'other'), 'other');
		expect(matchAction(ev('i', { ctrl: true, shift: true }), premiereWin)).toBe('range.clearIn');
		expect(matchAction(ev('e', { ctrl: true, shift: true }), premiereWin)).toBe('file.saveCover');

		const resolve = resolveBindings(presetOverrides(keyPreset('resolve')!, 'other'), 'other');
		expect(matchAction(ev('b'), resolve)).toBe('tool.razor');
		expect(matchAction(ev('a'), resolve)).toBe('tool.pointer');
		expect(matchAction(ev('n'), resolve)).toBe('tool.snap');
		// Shift+[ types `{` on a US keyboard.
		expect(matchAction(ev('{', { shift: true }), resolve)).toBe('edit.trimStart');
		expect(matchAction(ev('c'), resolve)).toBeNull();

		const fcp = resolveBindings(presetOverrides(keyPreset('finalcut')!, 'mac'), 'mac');
		expect(matchAction(ev('p', { ctrl: true }), fcp)).toBe('playback.goToTimecode');
		expect(matchAction(ev('Backspace'), fcp)).toBe('edit.rippleDelete');
		expect(matchAction(ev('s', { ctrl: true, shift: true }), fcp)).toBe('edit.detachAudio');
		expect(matchAction(ev('s', { meta: true, shift: true }), fcp)).toBeNull();
	});
});
