import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { posix, SRC, uiSources } from './test-sources';

// The dialogs, the Inspector's slider rows and the timeline's toolbar used to be drawn by hand,
// once each: six scrims with two z-indexes and two border widths, four slider layouts, five
// "on" looks. They are `Modal`, `SliderRow` and `ToolbarToggle` now; this keeps them that way,
// and holds each dialog to the close policy UI-3 gave it (a run cannot be closed, the export
// and a typed script survive a stray click).

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';
const code = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, '');

const DIALOGS = ['ExportDialog', 'SettingsDialog', 'UpdateDialog', 'VoiceoverDialog', 'FitMusicDialog', 'StemsDialog', 'AboutDialog'];

/** The opening `<Modal …>` tag of a dialog, whatever it spans. */
function modalTag(name: string): string {
	const m = /<Modal\b[^>]*>/.exec(read(`${EDITOR}${name}.svelte`));
	expect(m, `${name} has no <Modal>`).not.toBeNull();
	return m![0];
}

describe('Modal', () => {
	test('every dialog is one, and none draws its own scrim, focus trap or stacking', () => {
		for (const name of DIALOGS) {
			const source = read(`${EDITOR}${name}.svelte`);
			expect(source, name).toContain('<Modal');
			for (const own of ['role="dialog"', 'trapFocus', 'inset:0', 'z-index', 'aria-modal'])
				expect(source, `${name} draws its own ${own}`).not.toContain(own);
		}
	});

	test('nothing else is a modal dialog', () => {
		const modals = uiSources(SRC)
			.filter((f) => f.endsWith('.svelte') && /aria-modal|role="dialog"/.test(readFileSync(f, 'utf8')))
			.map(posix)
			.sort();
		// The notification log is a popover over the title bar, not a modal.
		expect(modals).toEqual([`${EDITOR}Modal.svelte`, `${EDITOR}NotificationCenter.svelte`]);
	});

	test('the close button is named, an icon, and gone while a run cannot be left', () => {
		const modal = read(`${EDITOR}Modal.svelte`);
		expect(modal).toMatch(/<IconBtn title="Close"[\s\S]*?<Icon n="x"/);
		expect(modal).toMatch(/\{#if !busy\}\s*<IconBtn title="Close"/);
		expect(modal).toContain('aria-labelledby={titleId}');
		expect(modal).toContain('id={titleId}');
	});

	test('Escape, the scrim and the button all ask `mayClose`', () => {
		const modal = read(`${EDITOR}Modal.svelte`);
		for (const via of ['escape', 'scrim', 'button']) expect(modal, via).toContain(`close('${via}')`);
		expect(modal).toContain('mayClose(via, rules)');
		expect(modal).toContain('scrimClick(pressedOnScrim');
	});

	test('one scrim, one panel border, one stacking level', () => {
		const modal = read(`${EDITOR}Modal.svelte`);
		expect(modal).toContain('z-index:50');
		expect(modal).toContain('color-mix(in srgb,var(--scrim) 55%,transparent)');
		expect(modal.match(/border[a-z-]*:var\(--line-width\)/g)?.length).toBeGreaterThanOrEqual(3);
		expect(modal).not.toMatch(/border[a-z-]*:\s*1px/);
	});

	test('the export holds its options: a click on the scrim does nothing, Escape and the buttons still close', () => {
		const tag = modalTag('ExportDialog');
		expect(tag).toContain('dismissOnScrim={false}');
		expect(tag).not.toContain('busy');
		expect(tag).toContain('rescue');
	});

	test('the voiceover keeps a typed script, locks during a run and starts on the script', () => {
		const tag = modalTag('VoiceoverDialog');
		expect(tag).toContain('busy={generating}');
		expect(tag).toContain('dismissOnScrim={scrimCloses(text, generating)}');
		expect(tag).toContain('initial="textarea"');
		expect(tag).toContain('rescue');
	});

	test('the others lock while they run and move focus to the live action', () => {
		expect(modalTag('FitMusicDialog')).toContain('busy={applying}');
		expect(modalTag('StemsDialog')).toContain('busy={running}');
		// The update download cannot be walked away from: on Windows the installer takes the app down when
		// it is done, past every close guard, so the dialog holds still (no Hide) and the install is
		// asked about first, with named buttons.
		expect(modalTag('UpdateDialog')).toContain("busy={u.phase === 'downloading'}");
		const update = read(`${EDITOR}UpdateDialog.svelte`);
		expect(update).not.toContain('>Hide<');
		expect(update).toContain('installPrompt(');
		expect(update).toContain('onclick={install}');
		for (const name of ['FitMusicDialog', 'StemsDialog', 'UpdateDialog']) expect(modalTag(name), name).toContain('rescue');
		// Settings has nothing to lock and no footer to swap.
		expect(modalTag('SettingsDialog')).not.toMatch(/busy|rescue|dismissOnScrim/);
	});

	test('a dialog whose footer swaps for a run marks the action focus goes to', () => {
		for (const name of ['ExportDialog', 'VoiceoverDialog', 'FitMusicDialog', 'StemsDialog', 'UpdateDialog'])
			expect(read(`${EDITOR}${name}.svelte`), name).toContain('data-rescue');
	});
});

describe('SliderRow', () => {
	const inspector = read(`${EDITOR}Inspector.svelte`);
	const row = read(`${EDITOR}SliderRow.svelte`);

	test('the Inspector draws no slider of its own', () => {
		expect(inspector).not.toContain('type="range"');
		expect(inspector).not.toContain('liveDrag');
		expect(inspector).toContain('<SliderRow');
	});

	test('every Inspector slider row is on the one grid, with the keyframe slot kept', () => {
		expect(row).toMatch(/grid-template-columns:\s*var\(--label-w\) minmax\(0, 1fr\) var\(--num-w\) 24px/);
		expect(row).toContain('--num-w:{NUMBER_COL}px');
		// The rows that are not sliders keep the same columns.
		expect(inspector).toContain('minmax(0,1fr) ${NUMBER_COL}px 24px');
		expect(row).toContain('reserve = true');
		expect(row).toMatch(/\.slot\s*\{[^}]*width: 24px;[^}]*height: 24px/);
	});

	test('the keyframe button is a 24 px icon button with a diamond, not a text glyph', () => {
		expect(inspector).toMatch(/<IconBtn\s+size=\{24\}/);
		expect(inspector).toContain('<Icon n="diamond"');
		expect(inspector).not.toContain("'◆' : '◇'");
	});

	test('the volume row has a label, so it lines up with the rest', () => {
		expect(inspector).toContain("rangeRow('Level', gain");
	});

	test('the slider cannot keep its row from shrinking, and a native one has a box to hit', () => {
		expect(row).toContain('minmax(0, 1fr)');
		const layout = code(read('routes/layout.css'));
		const rule = /input\[type='range'\]\s*\{([^}]*)\}/.exec(layout);
		expect(rule, 'the range rule').not.toBeNull();
		expect(rule![1]).toMatch(/height:\s*18px/);
		expect(rule![1]).toMatch(/min-width:\s*0/);
	});

	test('the slider is the mixer’s, so a gesture is one edit and Escape drops it', () => {
		expect(row).toContain('<MixSlider');
		expect(row).toContain('onreset={reset}');
		expect(row).toContain('oncancel={cancel}');
		expect(row).toContain('oncommit={commit}');
	});

	/** The text of every `{@render rangeRow(…)}` / `{@render numRow(…)}` call, to its closing brace. */
	function calls(source: string, name: string): string[] {
		const out: string[] = [];
		for (let at = source.indexOf(`{@render ${name}(`); at >= 0; at = source.indexOf(`{@render ${name}(`, at + 1)) {
			let depth = 0;
			let end = at;
			for (; end < source.length; end++) {
				if (source[end] === '{') depth++;
				else if (source[end] === '}' && --depth === 0) break;
			}
			out.push(source.slice(at, end + 1));
		}
		return out;
	}

	test('a write goes to what the edit began on: no row reads the selection when it ends', () => {
		// The click that selects another clip also blurs the field, and a typed entry is written on that blur.
		// `timeRow` is the In / Out and a title's Start / End (typed as timecode or seconds); `numRow` is what is left.
		const rows: Record<string, number> = { rangeRow: 4, numRow: 1, timeRow: 4 };
		for (const [name, least] of Object.entries(rows)) {
			const found = calls(inspector, name);
			expect(found.length, name).toBeGreaterThanOrEqual(least);
			for (const call of found) {
				// What a row hands back is the `at` it was given, never the selection as it is by then.
				const from = call.search(/\(v, at\) =>|setGain/);
				expect(from, call.slice(0, 60)).toBeGreaterThan(0);
				expect(call.slice(from), call.slice(0, 60)).not.toMatch(/\bclip[.!]|\bo\.id|ui\.time|selectedClip|\boverlay\b/);
			}
		}
		expect(inspector).toContain('{scope}');
		expect(inspector).toContain('fields.focus(e.currentTarget, scope)');
		expect(row).toContain('pending.end(');
		expect(row).toContain('onCommit(v, at)');
		expect(row).toContain('generics="S');
	});

	test('a row reads the clip it was told, and keys at the playhead it was told', () => {
		for (const helper of ['setTf', 'setCol', 'setGain', 'setCam', 'patchMask', 'setFade', 'moveTitle'])
			expect(inspector, helper).toMatch(new RegExp(`(function ${helper}|const ${helper} = )\\(?[^)]*at: Edit`));
		expect(inspector).toMatch(/function keyAtPlayhead\([^)]*playhead: number = ui\.time\)/);
	});

	test('a plain field commits what was typed into it, to what it took focus on', () => {
		// By the blur the selection is the other clip's and the field has been re-rendered with that
		// clip's value: reading either writes the other clip's number onto this one.
		const inputs = (inspector.match(/<input\b[\s\S]*?\/>/g) ?? []).filter((tag) => /type="(text|number)"/.test(tag));
		expect(inputs.length).toBeGreaterThanOrEqual(5);
		for (const tag of inputs) {
			const name = tag.replace(/\s+/g, ' ').slice(0, 70);
			expect(tag, name).toContain('onfocus={noteFocus}');
			expect(tag, name).toContain('oninput={noteTyped}');
			// An IME's last composition may not end in an `input` event.
			expect(tag, name).toContain('oncompositionend={noteTyped}');
			expect(tag, name).toMatch(/endEdit\(|patchTitle\(|patchSvg\(/);
			expect(tag, name).not.toMatch(/currentTarget\.value(?!\s*=[^=])/);
			expect(tag, name).not.toMatch(/\bclip[.!]|\bo\.id|selectedClip|\boverlay\b/);
		}
		// The title's colour pickers, select and checkbox change at once, but go through the same door.
		const pickers = (inspector.match(/<input\b[\s\S]*?\/>/g) ?? []).filter((tag) => /type="color"/.test(tag));
		expect(pickers.length).toBe(2);
		for (const picker of pickers) expect(picker).toContain('onfocus={noteFocus}');
		expect(pickers.filter((tag) => tag.includes('patchTitle(e.currentTarget'))).toHaveLength(1);
		expect(pickers.filter((tag) => tag.includes('patchSvg('))).toHaveLength(1);
		expect(inspector).not.toMatch(/updateOverlay\(o\.id/);
		expect(inspector.match(/patchTitle\(e\.currentTarget/g)?.length).toBe(5);
		for (const helper of ['setVideoFxParam', 'setAudioFxParam'])
			expect(inspector, helper).toMatch(new RegExp(`function ${helper}\\([^)]*at: Edit[^)]*\\) \\{\\s*const c = clipOf\\(at\\)`));
		expect(inspector).toContain('onParam: (i: number, key: string, value: unknown, at: Edit)');
	});

	test('the Transitions tab’s length field is held to the same rule', () => {
		const tab = read(`${EDITOR}TransitionsTab.svelte`);
		const field = tab.slice(tab.indexOf('onfocus={(e) => length.focus('), tab.indexOf('style="width:58px'));
		expect(field).toContain('length.input(');
		expect(field).toContain('oncompositionend=');
		expect(field).toContain('length.end(');
		expect(field).not.toMatch(/\bclip[.!?]|current\.kind/);
	});

	test('the rest values are the model’s defaults, not copies of them', () => {
		for (const name of ['DEFAULT_TRANSFORM', 'DEFAULT_COLOR', 'DEFAULT_REFRAME', 'DEFAULT_MASK'])
			expect(inspector, name).toMatch(new RegExp(`neutral: ${name}\\.`));
	});

	test('the dialogs’ slider rows are the same component', () => {
		expect(read(`${EDITOR}VoiceoverDialog.svelte`)).toContain('<SliderRow');
		expect(read(`${EDITOR}ExportDialog.svelte`)).toContain('<SliderRow');
	});
});

describe('ToolbarToggle', () => {
	const timeline = read(`${EDITOR}Timeline.svelte`);
	const from = timeline.indexOf('<!-- Timeline toolbar.');
	const to = timeline.indexOf('<!-- The whole cut on one strip');
	const toolbar = timeline.slice(from, to);
	const css = code(read('lib/styles/controls.css'));

	test('the toolbar is found', () => {
		expect(from).toBeGreaterThan(0);
		expect(to).toBeGreaterThan(from);
	});

	test('every button in the toolbar is one', () => {
		expect(toolbar).not.toContain('<button');
		expect(toolbar.match(/<ToolbarToggle/g)?.length).toBeGreaterThanOrEqual(12);
		const snippet = /\{#snippet toolBtn[\s\S]*?\{\/snippet\}/.exec(timeline)![0];
		expect(snippet).toContain('<ToolbarToggle');
		expect(snippet).not.toContain('<button');
	});

	test('snapping is a pressed magnet, not a word that changes width', () => {
		expect(toolbar).toMatch(/icon="magnet"[\s\S]{0,80}pressed=\{ui\.snap\}/);
		expect(toolbar).not.toContain('snap on');
		expect(toolbar).not.toContain('snap off');
	});

	test('one height and one target: 24 px', () => {
		const rule = /\.k-tt\s*\{([^}]*)\}/.exec(css);
		expect(rule, '.k-tt').not.toBeNull();
		expect(rule![1]).toMatch(/height:\s*24px/);
		expect(rule![1]).toMatch(/min-width:\s*24px/);
		expect(/\.k-tt\[data-square\]\s*\{([^}]*)\}/.exec(css)![1]).toMatch(/width:\s*24px/);
	});

	test('one "on": the amber outline and wash, whatever the button is', () => {
		const on = /\.k-tt\[data-on\]\s*\{([^}]*)\}/.exec(css);
		expect(on, 'on').not.toBeNull();
		expect(on![1]).toContain('background: var(--selection-fill)');
		expect(on![1]).toContain('border-color: var(--kerf-400)');
		expect(on![1]).toContain('color: var(--kerf-300)');
		// Lit by a toggle's state or by a menu's choice, through the same attribute.
		const source = read(`${EDITOR}ToolbarToggle.svelte`);
		expect(source).toContain('data-on={lit || undefined}');
		expect(source).toContain('aria-pressed={pressed ?? undefined}');
	});

	test('it reacts to the pointer and has a disabled look of its own', () => {
		expect(css).toMatch(/\.k-tt:hover:not\(:disabled\)\s*\{/);
		expect(css).toMatch(/\.k-tt:active:not\(:disabled\)\s*\{/);
		const off = /\.k-tt:disabled\s*\{([^}]*)\}/.exec(css);
		expect(off![1]).toContain('color: var(--text-disabled)');
		expect(off![1]).toContain('opacity: 1');
		// …and the veil that plain buttons get leaves it alone, or it would be drawn over twice.
		expect(css).not.toMatch(/button:not\(:disabled\):not\(\.k-btn\)(?![^{]*\.k-tt)[^{]*:hover/);
	});

	test('it carries no color inline', () => {
		const source = read(`${EDITOR}ToolbarToggle.svelte`);
		expect([...source.matchAll(/<button\b[^>]*>/g)].map((m) => m[0]).join(' ')).not.toMatch(/(background|color|border)[a-z-]*:/);
	});
});
