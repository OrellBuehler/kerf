import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// How the animated-title pieces are wired into the components that have no test of their own: the titles
// lane, the preview's placement box, the page's Delete. Read off the source, as `timeline-wiring.test.ts`
// does; the logic they call is tested where it lives (`svg-titles`, `title-actions`, `state-svg`).

const read = (path: string) => readFileSync(join(import.meta.dir, path), 'utf8');
const timeline = read('components/editor/Timeline.svelte');
const preview = read('components/editor/Preview.svelte');
const inspector = read('components/editor/Inspector.svelte');
const page = read('../routes/+page.svelte');

/** The body of `function name(...)` up to the next top-level function of the script. */
function bodyOf(src: string, name: string): string {
	const at = src.indexOf(`function ${name}(`);
	expect(at, name).toBeGreaterThan(-1);
	const next = src.indexOf('\n\t', src.indexOf('\n\t}', at) + 1);
	return src.slice(at, next < 0 ? undefined : next);
}

describe('the titles lane', () => {
	test('packs text and animated titles into the same rows', () => {
		expect(timeline).toContain('packRows([...editor.overlays, ...editor.svgOverlays])');
	});

	test('draws an animated title as its own kind of block, with the lane’s drag, trim and context menu', () => {
		const block = timeline.slice(timeline.indexOf('{#each editor.svgOverlays as o (o.id)}'));
		expect(block.slice(0, 1200)).toContain('data-kind="svg"');
		expect(block).toContain("onpointerdown={(e) => onTitlePointerDown(e, o, 'move', 'svg')}");
		expect(block).toContain("onpointerdown={(e) => onTitlePointerDown(e, o, 'l', 'svg')}");
		expect(block).toContain("onpointerdown={(e) => onTitlePointerDown(e, o, 'r', 'svg')}");
		expect(block).toContain('oncontextmenu={(e) => onSvgContextMenu(e, o)}');
		expect(block).toContain('data-diff={diff?.titles.get(o.id)}');
	});

	test('its edges are magnets for a title drag, and its drag snaps to everyone else’s', () => {
		const others = bodyOf(timeline, 'otherTitleEdges');
		expect(others).toContain('editor.svgOverlays');
		expect(others).toContain('editor.overlays');
		expect(bodyOf(timeline, 'titleSnapPoints')).toContain('otherTitleEdges(id)');
		expect(timeline).toContain('const others = otherTitleEdges(td.id);');
	});

	test('a gesture writes one updateSvgOverlay on release and never seeks', () => {
		const up = bodyOf(timeline, 'onTitleDragUp');
		expect(up.match(/updateSvgOverlay\(/g)).toHaveLength(1);
		expect(up).toContain("d.kind === 'svg'");
		expect(up).toContain('{ start: d.start, end: d.end }');
		expect(up).not.toContain('ui.seek(');
	});

	test('a press selects the title it is on, exclusive with the others', () => {
		const down = bodyOf(timeline, 'onTitlePointerDown');
		expect(down).toContain('editor.selectSvgOverlay(o.id)');
		expect(down).toContain('editor.selectOverlay(o.id)');
	});

	test('a click on bare lane space clears an animated title’s selection too, without seeking', () => {
		const body = bodyOf(timeline, 'onLaneSeek');
		expect(body).toContain('editor.selectedSvgOverlayId');
		expect(body).toMatch(/clearSelection\(\);\s*return;/);
	});

	test('the lane counts and collapses by both kinds, and its menu offers the four presets and a custom one', () => {
		expect(timeline).toContain('const titlesEmpty = $derived(titleCount === 0);');
		expect(timeline).toContain('Titles · {titleCount}');
		const menu = bodyOf(timeline, 'onTitleLaneContextMenu');
		expect(menu).toContain('SVG_PRESETS.map');
		expect(menu).toContain('addAnimatedTitle(p.kind)');
		expect(menu).toContain('addCustomSvgTitle()');
	});

	test('its menu removes through the editor and can open a custom title’s source', () => {
		const menu = bodyOf(timeline, 'onSvgContextMenu');
		expect(menu).toContain('editor.removeSvgOverlay(o.id)');
		expect(menu).toContain('ui.svgSourceFocus = o.id');
		expect(menu).toContain("settings.shortcut('edit.delete')");
	});
});

describe('the preview’s placement box', () => {
	test('is drawn for the selected animated title only, while paused and inside its span', () => {
		expect(preview).toContain('editor.selectedSvgOverlay');
		expect(preview).toMatch(/o && !ui\.playing && !empty && isVisibleAt\(o, ui\.time\)/);
	});

	test('counts toward the titles shown, which is what routes the preview', () => {
		expect(preview).toContain('titlesShown: titlesHere.length > 0 || svgBox !== null');
	});

	test('one edit on release, only when something moved', () => {
		const up = bodyOf(preview, 'onSvgUp');
		expect(up.match(/updateSvgOverlay\(/g)).toHaveLength(1);
		expect(up).toContain('rectPatch(d.from, roundRect(d.rect))');
		expect(up).toContain('Object.keys(patch).length === 0');
	});

	test('Escape, a lost pointer, a cancel and a window blur abandon it', () => {
		expect(preview).toContain('onpointercancel={cancelSvgDrag}');
		expect(preview).toContain('onlostpointercapture={onSvgLostCapture}');
		expect(preview).toContain('blur: cancelDrags');
		expect(preview).toMatch(/e\.key === 'Escape' && \(\(tdrag && !tdrag\.committing\) \|\| \(sdrag && !sdrag\.committing\)\)/);
		expect(bodyOf(preview, 'cancelDrags')).toContain('cancelSvgDrag()');
	});

	test('the box and the title boxes are never dragged at once', () => {
		expect(bodyOf(preview, 'onSvgDown')).toContain('tdrag || sdrag');
		expect(bodyOf(preview, 'onTitleDown')).toContain('tdrag || sdrag');
	});

	test('draws a box and handles, never the artwork', () => {
		expect(preview).not.toContain('selectedSvgOverlay.svg');
		expect(preview).not.toContain('svgBox.svg');
		expect(preview).not.toMatch(/<img[^>]*svg/i);
	});
});

describe('the Inspector', () => {
	test('shows the animated title’s editor for the selected one, after a text title’s', () => {
		expect(inspector).toMatch(/\{#if overlay\}\s*\{@render overlayEditor\(overlay\)\}\s*\{:else if svgTitle\}\s*\{@render svgEditor\(svgTitle\)\}/);
	});

	test('applies custom source only after the backend has read it', () => {
		const actions = read('title-actions.ts');
		const apply = actions.slice(actions.indexOf('export async function applySvgSource'));
		expect(apply.indexOf('await validateSvg(svg)')).toBeGreaterThan(-1);
		expect(apply.indexOf('await validateSvg(svg)')).toBeLessThan(apply.indexOf('updateSvgOverlay'));
		expect(inspector).toContain('applySvgSource(o.id, draft)');
	});

	test('a preset patch carries every parameter, since the backend fills a missing one with its default', () => {
		expect(inspector).toContain('{ preset_params: { ...o.preset.params, ...change } }');
	});

	test('the source box is a monospace textarea with a name', () => {
		const area = /<textarea[\s\S]*?<\/textarea>/.exec(inspector)![0];
		expect(area).toContain('font-family:var(--font-mono)');
		expect(area).toContain('aria-label="SVG source"');
		expect(area).toContain('spellcheck="false"');
	});
});

describe('the page', () => {
	test('Delete removes a selected animated title, with an Undo', () => {
		const body = bodyOf(page, 'deleteKey');
		expect(body).toMatch(/\.removeSvgOverlay\(editor\.selectedSvgOverlayId\)/);
		expect(body).toContain("'Animated title removed'");
	});
});
