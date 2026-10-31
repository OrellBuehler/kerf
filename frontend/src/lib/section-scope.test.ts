import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// The Inspector's sections are not remounted per clip, so a commit that comes with a blur (a typed number, a
// field, a slider's release, a timer, a file dialog) runs after the selection has moved and reads the *new*
// clip's props. Each section writes to the clip or title the edit began on, built from that one's own values
// as the timeline has them when it lands. These scans hold the sections to it; the pure parts are in
// `grade.test.ts`, `field-edit.test.ts` and `slider-row.test.ts`.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';
/** The source with its comments dropped, so a rule is held to code and not to the prose about it. */
const code = (source: string) =>
	source
		.split('\n')
		.filter((l) => !/^\s*(\/\/|\*|\/\*)/.test(l))
		.join('\n');

describe('the grade sections write to the clip an edit began on', () => {
	const source = code(read(`${EDITOR}GradeSections.svelte`));

	test('no write is built from the grade the panel shows', () => {
		// `grade` is what the panel draws, i.e. whichever clip is selected by the time a blur lands.
		expect(source).not.toMatch(/\.\.\.grade\b/);
		expect(source).not.toMatch(/\bwrite\(\w+(\.id)?, \{/);
		for (const helper of ['curveEdit(', 'wheelEdit(', 'lutEdit(', 'gradeWith(']) expect(source, helper).toContain(helper);
		expect(source).toMatch(/editor\.timeline, clipId/);
	});

	test('the sliders hand back the clip they began on, and a wait or a dialog takes the id before it', () => {
		expect(source.match(/onCommit=\{\(v, clipId\) =>/g)?.length).toBe(2);
		expect(source.match(/scope=\{clip\.id\}/g)?.length).toBe(2);
		const choose = source.slice(source.indexOf('async function chooseLut'));
		expect(choose.indexOf('const clipId = clip.id')).toBeLessThan(choose.indexOf('await pickLutFile()'));
		for (const gesture of ['function curveDown', 'function wheelDown'])
			expect(source.slice(source.indexOf(gesture), source.indexOf(gesture) + 200), gesture).toContain('const clipId = clip.id');
	});

	test('a draft or a puck belongs to a clip, and is not drawn on another', () => {
		expect(source).toMatch(/draft\?\.id === clip\.id/);
		expect(source).toMatch(/puck\?\.id === clip\.id/);
	});
});
