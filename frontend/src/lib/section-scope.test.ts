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

/** The text of one `function name(` up to the next function at the same indent (or the end of the script). */
function body(source: string, name: string): string {
	const at = source.search(new RegExp(`(?:async )?function ${name}\\(`));
	expect(at, name).toBeGreaterThanOrEqual(0);
	const rest = source.slice(at).split('\n</script>')[0];
	const next = rest.slice(1).search(/\n\t(?:async )?function /);
	return next < 0 ? rest : rest.slice(0, next + 1);
}

describe('the title text section commits to the title and text a field took focus on', () => {
	const source = code(read(`${EDITOR}TextSection.svelte`));

	test('every number field notes its scope on focus and what was typed on input and composition', () => {
		const inputs = source.match(/<input\b[\s\S]*?\/>/g) ?? [];
		expect(inputs.length).toBe(3);
		for (const tag of inputs) {
			const name = tag.replace(/\s+/g, ' ').slice(0, 60);
			expect(tag, name).toContain('onfocus={(e) => noteFocus(e, t)}');
			expect(tag, name).toContain('oninput={noteTyped}');
			expect(tag, name).toContain('oncompositionend={noteTyped}');
		}
		expect(source).toContain('new FieldEdits<Edit, HTMLInputElement>()');
	});

	test('a blur-time commit reads the scope and the typed text, then the text as the cut has it, never `o` or the row’s `t`', () => {
		for (const name of ['commitWidth', 'commitTiming']) {
			const fn = body(source, name);
			expect(fn, name).toContain('edits.end(el, ');
			// The one place the field’s own state is read is the fallback of `edits.end`; after it, only `at` and `typed`.
			const rest = fn.replace(/[^\n]*edits\.end\(el, [^\n]*\n/, '');
			expect(rest, name).not.toMatch(/\bo\.id\b|\bo\.name\b|\bel\.value\b(?!\s*=[^=])/);
			expect(rest, name).toContain('textAt(at)');
		}
		for (const name of ['commit', 'setAnimation']) expect(body(source, name), name).not.toMatch(/\bo\.id\b|\bo\.name\b/);
		// What the number is checked against is the animation of the text it began on, not the row’s.
		expect(body(source, 'commitTiming')).toContain('const current = was?.text.animation');
	});

	test('what the title is called and what the engine is told are read before the await', () => {
		const convert = body(source, 'convert');
		expect(convert.indexOf('const { id, name } = o')).toBeLessThan(convert.indexOf('await editor.convertTitleText'));
		expect(convert).not.toMatch(/convertTitleText\(o\.id/);
	});
});

describe('the custom SVG source box applies to the title it was applied on', () => {
	const fn = body(code(read(`${EDITOR}Inspector.svelte`)), 'applySource');

	test('the result is shown only while that title is still selected, after the await and in the catch', () => {
		const wait = fn.indexOf('await applySvgSource(o.id, draft)');
		expect(wait).toBeGreaterThan(0);
		const after = fn.slice(wait);
		const guard = 'svgTitle?.id !== o.id';
		const first = after.indexOf(guard);
		expect(first, 'after the await').toBeGreaterThan(0);
		expect(first).toBeLessThan(after.indexOf('sourceWarnings = check.warnings'));
		expect(first).toBeLessThan(after.indexOf('draft = check.svg'));
		const caught = after.slice(after.indexOf('catch (e)'));
		expect(caught.indexOf(guard), 'in the catch').toBeGreaterThan(0);
		expect(caught.indexOf(guard)).toBeLessThan(caught.indexOf('sourceError ='));
	});
});

describe('the sound effect section reshapes the clip a change began on', () => {
	const shape = code(read(`${EDITOR}SfxShape.svelte`));
	const section = code(read(`${EDITOR}SoundEffectSection.svelte`));
	const panel = code(read(`${EDITOR}SoundFx.svelte`));

	test('every slider of the shape hands back its scope, and a switch or a take is pressed on the scope it has', () => {
		const rows = shape.match(/<SliderRow\b[^\n]*/g) ?? [];
		expect(rows.length).toBe(5);
		// Length, Pitch, Intensity and Tail change the spec; Gain is the panel’s own (the clip has a Volume section).
		const specRows = rows.filter((r) => /onchange\(/.test(r));
		expect(specRows.length).toBe(4);
		for (const r of specRows) {
			expect(r).toContain('scope={scope as S}');
			expect(r).toMatch(/onCommit=\{\(v, at\) => onchange\(\{ \w+: v \}, at\)\}/);
		}
		expect(shape).toContain('generics="S = undefined"');
		expect(shape).toContain('onchange({ reverse: checked }, scope as S)');
		expect(shape).toContain('onchange({ seed: nextSeed(spec.seed) }, scope as S)');
	});

	test('the Inspector’s section passes the clip; the Sound FX panel has none to pass', () => {
		expect(section).toContain('scope={clip.id}');
		expect(panel).not.toMatch(/<SfxShape[^>]*\bscope=/);
	});

	test('a reshape reads the sound and the clip by the id it was handed, never the section’s props', () => {
		const fn = body(section, 'reshape');
		expect(fn).toContain('async function reshape(patch: Partial<SfxSpec>, clipId: string)');
		expect(fn).toContain('soundOf(editor.timeline, editor.assets, clipId)');
		expect(fn).toContain('editor.updateSoundEffect(clipId, next)');
		expect(fn).toContain('const who = `clip:${clipId}`');
		expect(fn).not.toMatch(/\bclip\.id\b|\bspec\b|\bowner\b(?! === who)/);
	});
});
