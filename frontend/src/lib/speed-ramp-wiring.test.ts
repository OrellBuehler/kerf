import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Inspector does not remount the speed-ramp editor per clip, so everything it holds across a selection change
// has to be about the clip it was begun on: a background job that selects its result mid-drag must not write the old
// clip's ramp onto the new one, nor leave "key 3" selected on a clip that has two.

const src = readFileSync(join(import.meta.dir, 'components/editor/SpeedRamp.svelte'), 'utf8');
const bodyOf = (name: string): string => {
	const at = src.indexOf(`function ${name}(`);
	expect(at, name).toBeGreaterThan(-1);
	return src.slice(at, src.indexOf('\n\tfunction ', at + 1));
};

describe('the speed-ramp editor follows its clip', () => {
	test('the selected key is dropped when another clip is shown', () => {
		expect(src).toMatch(/\$effect\(\(\) => \{\s*void clip\.id;\s*untrack\(\(\) => \(sel = null\)\);\s*\}\);/);
	});

	test('a drag writes to the clip it was pressed on, not whichever is shown when it is let go', () => {
		const down = bodyOf('down');
		expect(down).toContain('const id = clip.id;');
		expect(down).toContain('write(d.keys, undefined, id)');
		expect(down).toContain('drag = { clipId: id,');
		const write = bodyOf('write').split('\n')[0];
		expect(write).toContain('id: string = clip.id');
		expect(src).toContain('planRampWrite(editor.timeline, id, next)');
		expect(src).not.toContain('planRampWrite(editor.timeline, clip.id');
	});

	test('the ghost is held until the edit lands, and is only ever drawn on its own clip', () => {
		const commit = bodyOf('down').slice(bodyOf('down').indexOf('commit()'));
		expect(commit).toContain('.finally(() => {');
		expect(commit.indexOf('write(')).toBeLessThan(commit.indexOf('drag = null'));
		expect(src).toContain('const live = $derived(drag && drag.clipId === clip.id ? drag : null);');
		// nothing derived reads the raw ghost
		expect(src.match(/\bdrag\?\./g)).toBeNull();
	});

	test('a key added then selected is only selected while the same clip is on show', () => {
		const add = bodyOf('addAt');
		expect(add).toContain('const id = clip.id;');
		expect(add).toContain('if (clip.id === id) sel =');
	});
});
