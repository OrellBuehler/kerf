import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// What the grade's panel is held to that only a window shows: the wheels are real controls (a tab stop, a name, words,
// keys), every number is a labelled box, the layout is judged from the section's own width, a write is built from the
// clip the gesture began on, and nothing here is keyable. The arithmetic is tested beside the pure modules (`grade-wheel`,
// `curve-edit`, `key-run`, `grade-write`, `lut-problem`).

const EDITOR = 'lib/components/editor/';
const read = (name: string) => readFileSync(join(SRC, EDITOR, `${name}.svelte`), 'utf8');
const sections = read('GradeSections');
const wheel = read('GradeWheel');
const curve = read('CurveEditor');
const field = read('GradeField');
const slider = read('GradeSlider');

/** A short failure: the sources are long, and `toContain` prints all of the one that lacks the text. */
const has = (source: string, text: string, what = text) => expect(source.includes(text), `has ${what}`).toBe(true);
const lacks = (source: string, text: string, what = text) => expect(source.includes(text), `lacks ${what}`).toBe(false);
const matches = (source: string, re: RegExp, what = String(re)) => expect(re.test(source), `matches ${what}`).toBe(true);
const misses = (source: string, re: RegExp, what = String(re)) => expect(re.test(source), `does not match ${what}`).toBe(false);

/** The markup of a source up to its `<style>` block. */
const markup = (source: string) => source.slice(0, source.indexOf('<style>') < 0 ? undefined : source.indexOf('<style>'));

describe('the colour wheel is a control, not a picture', () => {
	test('the disc is a tab stop with a name, a role description, a value in words and keys', () => {
		has(wheel, 'role="slider"');
		has(wheel, 'tabindex="0"');
		lacks(wheel, 'tabindex="-1"');
		has(wheel, 'aria-label="{label} color"');
		has(wheel, 'aria-roledescription="color wheel"');
		has(wheel, 'aria-valuetext={wheelWords(shown)}');
		has(wheel, '{onkeydown}');
		has(wheel, 'wheelKey(');
	});

	test('a drag is one edit that Escape, pointercancel and a lost focus abandon, and a run of keys is one edit', () => {
		has(wheel, 'beginDrag(');
		has(wheel, 'new KeyRun');
		matches(wheel, /Escape[\s\S]*?run\.cancel\(\)/);
		has(wheel, 'onblur={() => run.finish()}');
	});

	test('Home and a double-click centre the puck; the wheel has a reset of its own', () => {
		has(wheel, 'ondblclick={centre}');
		has(wheel, 'wheelKey(');
		matches(markup(wheel), /Reset \$\{label\.toLowerCase\(\)\} to neutral/);
	});

	test('the group is named by the range, and the lightness slider and numbers by the range too', () => {
		has(wheel, 'role="group" aria-label={label}');
		has(wheel, 'name="{label} lightness"');
		has(wheel, 'name="{label} {CHANNEL_NAME[c]}"');
	});

	test('what is written is a function of the wheel of the clip the edit began on', () => {
		has(wheel, 'onChange: (clipId: string, change: (wheel: Wheel) => Wheel) => Promise<unknown>');
		// no write builds a wheel from the one on screen when it ends
		misses(markup(wheel), /onCommit=\{\(v\) =>/);
		has(markup(wheel), 'scope={clipId}');
	});
});

describe('every number of the grade is a labelled box', () => {
	test('a box has its letter, a name, a spinbutton role and takes the arrows', () => {
		has(field, 'role="spinbutton"');
		has(field, 'aria-label={name}');
		has(field, 'aria-valuetext');
		has(field, "e.key !== 'ArrowUp' && e.key !== 'ArrowDown'");
		has(field, 'new KeyRun');
	});

	test('a typed entry is written to what it began on, and the box is read-only (not disabled) while an edit is in flight', () => {
		has(field, 'pending.begin(');
		has(field, 'leaveEntry(pending, now');
		has(field, 'readonly={disabled || blank}');
		has(field, "aria-disabled={disabled || blank || undefined}");
		misses(markup(field), /\sdisabled=\{/);
	});

	test('the lightness slider is the mixer’s, so a gesture is one edit and Escape drops it', () => {
		has(slider, '<MixSlider');
		for (const handler of ['onreset={reset}', 'oncancel={cancel}', 'oncommit={commit}']) has(slider, handler);
		has(slider, 'pending.end(now)');
	});

	test('the wheels’ boxes are R, G, B and lightness; a curve point’s are In and Out', () => {
		has(wheel, 'label="L"');
		has(wheel, '{#each WHEEL_CHANNELS as c (c)}');
		has(curve, 'label="In"');
		has(curve, 'label="Out"');
	});
});

describe('the layout is the section’s width, not the window’s', () => {
	test('the wheels lay themselves out from a container query on their section', () => {
		matches(sections, /\.wheels\s*\{[^}]*container:\s*wheels\s*\/\s*inline-size/);
		matches(wheel, /@container wheels \(max-width: \d+px\)/);
		matches(sections, /@container wheels \(max-width: \d+px\)/);
		// the same threshold in both: the grid of three and a wheel's own arrangement change together
		const widths = [sections, wheel].map((s) => /@container wheels \(max-width: (\d+)px\)/.exec(s)![1]);
		expect(widths[0]).toBe(widths[1]);
	});

	test('the curve editor’s square fills its column up to a size, and takes a side column when there is room', () => {
		has(curve, 'bind:clientWidth={avail}');
		has(curve, 'curveSide(avail, MAX_SIDE)');
		matches(curve, /\.curve\s*\{[^}]*container:\s*curve\s*\/\s*inline-size/);
		matches(curve, /@container curve \(min-width: \d+px\)/);
		misses(curve, /SIZE = 200/);
	});

	test('no pixel size of the old fixed square or the 84 px disc is left in the sections', () => {
		misses(sections, /\b(?:WHEEL|SIZE)\s*=\s*\d+/);
		lacks(sections, 'rgbText');
	});
});

describe('the curve editor', () => {
	test('is a tab stop with a name, keys, and a note of what they do', () => {
		has(curve, 'role="application"');
		has(curve, 'tabindex="0"');
		has(curve, 'aria-roledescription="curve editor"');
		has(curve, 'aria-describedby="{uid}-keys"');
		has(curve, 'curveKey(');
		has(curve, 'aria-live="polite"');
	});

	test('draws its colours from the theme, again when the theme changes, and the grid and the diagonal are tokens', () => {
		has(curve, 'void settings.theme;');
		for (const token of ['--surface-inset', '--border-default', '--border-strong', '--text-muted', '--surface-panel']) has(curve, `resolveColor('${token}', el)`);
		has(curve, 'ctx.setLineDash([4, 4])');
	});

	test('a gesture is one edit that Escape and pointercancel abandon; a double-click takes a point out', () => {
		has(curve, 'beginDrag(e,');
		has(curve, 'ondblclick={removeAt}');
		has(curve, 'oncontextmenu={removeAt}');
		has(curve, 'DOUBLE_CLICK_MS');
	});

	test('each channel can be reset, and says whether it has points', () => {
		matches(curve, />Reset<\/Btn/);
		has(curve, 'has points');
	});
});

describe('what is written', () => {
	test('every write goes through the grade writer, which reads the clip the gesture began on', () => {
		has(sections, 'gradeWriter({');
		has(sections, 'clipById(editor.timeline, id)');
		has(sections, 'set: (id, g) => editor.setGrade(id, g)');
		// the only call of setGrade is the writer's own
		expect(sections.match(/editor\.setGrade\(/g)?.length).toBe(1);
		// no write merges its change into the grade of the clip on screen
		misses(markup(sections), /\{\s*\.\.\.grade[,\s}]/);
		misses(sections, /write\([^)]*\{\s*\.\.\.grade\b/);
	});

	test('a LUT that cannot be used says what it costs, and the section’s summary says so when it is folded', () => {
		has(sections, 'lutProblem(info.error, lutFile)');
		has(sections, 'const lutLabel');
		has(sections, '<InspectorSection title="LUT" summary={lutLabel}>');
		has(sections, 'Remove LUT');
		has(sections, 'text-overflow: ellipsis');
	});

	test('nothing here is keyable, so no row carries a keyframe button', () => {
		for (const source of [sections, wheel, curve, field, slider]) {
			lacks(source, 'keyframe={');
			lacks(source, 'n="diamond"');
		}
		has(sections, 'Nothing here is keyable');
	});

	test('Reset and Remove are small secondary buttons, never ghost', () => {
		for (const source of [sections, curve]) lacks(source, 'variant="ghost"');
	});
});
