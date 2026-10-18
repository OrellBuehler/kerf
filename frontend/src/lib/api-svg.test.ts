import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addSvgOverlay,
	getHistory,
	getTimeline,
	listSvgPresets,
	removeSvgOverlay,
	revertTo,
	svgPresetPreview,
	updateSvgOverlay,
	validateSvg
} from './api';
import { MAX_SVG_OVERLAYS } from './svg-dev';
import type { SvgOverlay, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's animated titles. They have to
// keep the backend's contract: the same refusals, one revision per edit, an absent list when empty,
// and a preset that follows its params. The preset artwork is kerf-core's own, written down in
// fixtures/svg-presets.json.

const CUSTOM = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><rect width="100" height="50"/></svg>';

beforeEach(async () => {
	await revertTo(0);
});

const titles = (t: Timeline): SvgOverlay[] => t.svg_overlays ?? [];

async function addPreset(text = 'Hello', start = 1, end?: number) {
	const t = await addSvgOverlay({ preset: { kind: 'fade_up', params: { text } } }, start, end);
	return titles(t).at(-1)!;
}

describe('the presets (browser harness)', () => {
	test('lists the four kinds, each asking for text', async () => {
		const infos = await listSvgPresets();
		expect(infos.map((i) => i.kind)).toEqual(['fade_up', 'typewriter', 'lower_third', 'word_pop']);
		for (const info of infos) expect(info.params[0]).toEqual({ name: 'text', required: true });
		expect(infos.find((i) => i.kind === 'word_pop')!.params.some((p) => p.name === 'subtitle')).toBe(false);
		expect(infos[0].params.find((p) => p.name === 'color')!.default).toBeTruthy();
	});

	test('every kind draws an SVG that carries the text, escaped', async () => {
		for (const kind of ['fade_up', 'typewriter', 'lower_third', 'word_pop'] as const) {
			const svg = await svgPresetPreview(kind, { text: 'Fish <b>& Chips</b>' });
			expect(svg.startsWith('<svg')).toBe(true);
			expect(svg).not.toContain('<b>');
			expect(svg).not.toContain('Your');
			expect(svg).toContain('&lt;');
		}
		const fade = await svgPresetPreview('fade_up', { text: 'A & B' });
		expect(fade).toContain('>A &amp; B<');
	});

	test('a typewriter has one step per character and a word pop one pop per word', async () => {
		const typed = await svgPresetPreview('typewriter', { text: 'Hi there' });
		expect(typed.match(/<tspan /g)!.length).toBe(8);
		const pops = await svgPresetPreview('word_pop', { text: 'one two' });
		expect(pops.match(/<g /g)!.length).toBe(2);
		expect(pops).toContain('>one<');
		expect(pops).toContain('>two<');
		expect(await svgPresetPreview('word_pop', { text: 'two one' })).toContain('>two<');
	});

	test('a subtitle only shows where the preset has one', async () => {
		const withSub = await svgPresetPreview('lower_third', { text: 'Ada', subtitle: 'Engineer' });
		expect(withSub).toContain('>Ada<');
		expect(withSub).toContain('Engineer');
		expect(await svgPresetPreview('lower_third', { text: 'Ada' })).not.toContain('subtitle');
		expect(await svgPresetPreview('word_pop', { text: 'Ada', subtitle: 'ignored' })).not.toContain('ignored');
	});

	test('colour and font are swapped in, and one that is not a colour is refused', async () => {
		const {
			defaults: { color, accent, font }
		} = (await import('./fixtures/svg-presets.json')).default;
		const svg = await svgPresetPreview('lower_third', { text: 'Ada', color: 'teal', accent: 'rgb(1, 2, 3)', font: 'Georgia' });
		expect(svg).toContain('fill="teal"');
		expect(svg).toContain('fill="rgb(1, 2, 3)"');
		expect(svg).toContain('font-family="Georgia"');
		expect(svg).not.toContain(`fill="${color}"`);
		expect(svg).not.toContain(`fill="${accent}"`);
		expect(svg).not.toContain(`font-family="${font}"`);
		await expect(svgPresetPreview('fade_up', { text: 'x', color: 'a" onload="x' })).rejects.toThrow('colour');
	});

	test('refuses what the engine does: no text, or text and fonts too long', async () => {
		await expect(svgPresetPreview('fade_up', { text: '  \n ' })).rejects.toThrow('some text');
		await expect(svgPresetPreview('fade_up', { text: 'x'.repeat(201) })).rejects.toThrow('200');
		await expect(svgPresetPreview('fade_up', { text: 'x', subtitle: 'y'.repeat(201) })).rejects.toThrow('subtitle');
		await expect(svgPresetPreview('fade_up', { text: 'x', font: 'f'.repeat(101) })).rejects.toThrow('font');
	});
});

describe('validating an SVG (browser harness)', () => {
	test('answers the document and what it knows will be dropped', async () => {
		const ok = await validateSvg(CUSTOM);
		expect(ok).toEqual({ svg: CUSTOM, warnings: [], natural_duration: null });
		const risky = await validateSvg(CUSTOM.replace('<rect', '<script>alert(1)</script><animateMotion/><rect'));
		expect(risky.warnings.length).toBe(2);
		expect(risky.svg).toBe(CUSTOM);
	});

	test('refuses what is not an SVG, or too much of it', async () => {
		await expect(validateSvg('<html></html>')).rejects.toThrow('<svg');
		await expect(validateSvg('')).rejects.toThrow('<svg');
		await expect(validateSvg(CUSTOM + ' '.repeat(128 * 1024))).rejects.toThrow('larger than 128 KiB');
	});

	test('refuses a root without the SVG namespace, or without a size to fit', async () => {
		const bare = '<svg viewBox="0 0 10 10"><rect width="5" height="5"/></svg>';
		await expect(validateSvg(bare)).rejects.toThrow('xmlns="http://www.w3.org/2000/svg"');
		await expect(validateSvg(bare.replace('<svg', '<svg xmlns="http://example.com/other"'))).rejects.toThrow('xmlns');
		const ns = 'xmlns="http://www.w3.org/2000/svg"';
		for (const root of [`<svg ${ns}>`, `<svg ${ns} width="100">`, `<svg ${ns} width="100%" height="50%">`, `<svg ${ns} viewBox="0 0 0 10">`, `<svg ${ns} viewBox="0 0 10">`]) {
			await expect(validateSvg(`${root}<rect/></svg>`)).rejects.toThrow('viewBox');
		}
		for (const root of [`<svg ${ns} viewBox="0 0 10 5">`, `<svg ${ns} viewBox="0,0,10,5">`, `<svg ${ns} width="10" height="5px">`, `<svg ${ns} width="10" height="5" viewBox="x">`]) {
			expect((await validateSvg(`<?xml version="1.0"?><!-- c -->${root}<rect/></svg>`)).warnings).toEqual([]);
		}
	});

	test('asks the browser to read it as XML when there is one, and says what it complained of', async () => {
		const real = globalThis.DOMParser;
		const seen: string[] = [];
		globalThis.DOMParser = class {
			parseFromString(src: string, type: string) {
				seen.push(type);
				const broken = src.includes('&nbsp;');
				return { getElementsByTagName: () => (broken ? [{ textContent: 'Entity \'nbsp\' not defined\nLine 1' }] : []) };
			}
		} as unknown as typeof DOMParser;
		try {
			expect((await validateSvg(CUSTOM)).warnings).toEqual([]);
			await expect(validateSvg(CUSTOM.replace('<rect', '&nbsp;<rect'))).rejects.toThrow("not valid XML: Entity 'nbsp' not defined");
			expect(seen).toEqual(['image/svg+xml', 'image/svg+xml']);
		} finally {
			globalThis.DOMParser = real;
		}
	});

	test('reads colours the way the engine does', async () => {
		const colour = (color: string) => svgPresetPreview('fade_up', { text: 'x', color });
		for (const ok of ['teal', 'RED', ' white ', 'transparent', '#abc', '#abcd', '#aabbcc', '#aabbccdd', 'rgb(1,2,3)', 'rgb(1 2 3)', 'rgba(1, 2, 3, 0.5)', 'rgb(10%, 20%, 30%)', 'rgb(10%, 20, 30)', 'hsl(120, 50%, 50%)', 'hsla(120 50% 50% 0.5)']) {
			expect(await colour(ok)).toContain('<svg');
		}
		for (const bad of ['', 'tealish', '#ab', '#abcde', '#abcdefg', 'rgb(1,2)', 'rgb(1,2,3,4,5)', 'rgb(1,2,3', 'rgb (1,2,3)', 'rgb(1, 2%, 3)', 'rgb(a,b,c)', 'rgb(1,,3)', 'url(#x)', 'a" onload="x', 'red;blue', 'x'.repeat(65)]) {
			await expect(colour(bad)).rejects.toThrow('colour');
		}
	});
});

describe('adding an animated title (browser harness)', () => {
	test('a title is absent from a timeline until one is added', async () => {
		expect((await getTimeline()).svg_overlays).toBeUndefined();
	});

	test('a preset takes the whole frame and runs four seconds by default', async () => {
		const o = await addPreset('Hello', 2.5);
		expect(o).toMatchObject({ name: '', start: 2.5, end: 6.5, x: 0, y: 0, width: 1, height: 1, opacity: 1 });
		expect(o.preset!.kind).toBe('fade_up');
		expect(o.preset!.params.text).toBe('Hello');
		expect(o.preset!.params.color).toBeTruthy();
		expect(o.preset!.params.subtitle).toBeNull();
		expect(o.svg).toContain('>Hello<');
		expect(o.offset).toBeUndefined();
		expect((await getTimeline()).svg_overlays).toHaveLength(1);
	});

	test('a custom title keeps its source and has no preset', async () => {
		const t = await addSvgOverlay({ custom: CUSTOM }, 0, 3, { name: '  Logo ', x: 0.7, y: 0.05, width: 0.25, height: 0.2, opacity: 0.5 });
		const o = titles(t)[0];
		expect(o).toMatchObject({ name: 'Logo', svg: CUSTOM, start: 0, end: 3, x: 0.7, y: 0.05, width: 0.25, height: 0.2, opacity: 0.5 });
		expect(o.preset).toBeUndefined();
	});

	test('a start before zero is held at zero, and titles stay in the order they were added', async () => {
		await addSvgOverlay({ custom: CUSTOM }, -3, 2);
		await addSvgOverlay({ custom: CUSTOM }, 0.5, 1);
		const [a, b] = titles(await getTimeline());
		expect([a.start, a.end]).toEqual([0, 2]);
		expect([b.start, b.end]).toEqual([0.5, 1]);
	});

	test('refuses an end before the start, a placement off the frame and a source it cannot read', async () => {
		await expect(addSvgOverlay({ custom: CUSTOM }, 5, 5)).rejects.toThrow('end must be after start');
		await expect(addSvgOverlay({ custom: CUSTOM }, 5, 2)).rejects.toThrow('end must be after start');
		await expect(addSvgOverlay({ custom: CUSTOM }, Number.NaN)).rejects.toThrow('start must be a number');
		await expect(addSvgOverlay({ custom: CUSTOM }, 0, 1, { width: 0 })).rejects.toThrow('width and height');
		await expect(addSvgOverlay({ custom: CUSTOM }, 0, 1, { x: 9 })).rejects.toThrow('x and y');
		await expect(addSvgOverlay({ custom: CUSTOM }, 0, 1, { opacity: 1.5 })).rejects.toThrow('opacity');
		await expect(addSvgOverlay({ custom: 'nope' }, 0, 1)).rejects.toThrow('<svg');
		await expect(addSvgOverlay({ preset: { kind: 'fade_up', params: { text: '' } } }, 0, 1)).rejects.toThrow('some text');
		expect((await getTimeline()).svg_overlays).toBeUndefined();
	});

	test('holds at most fifty', async () => {
		for (let i = 0; i < MAX_SVG_OVERLAYS; i++) await addSvgOverlay({ custom: CUSTOM }, i, i + 1);
		await expect(addSvgOverlay({ custom: CUSTOM }, 0, 1)).rejects.toThrow(`at most ${MAX_SVG_OVERLAYS}`);
		expect(titles(await getTimeline())).toHaveLength(MAX_SVG_OVERLAYS);
	});

	test('is one revision, labelled as the core labels it, and undo takes it back', async () => {
		const head = (await getHistory()).find((h) => h.current)!.seq;
		await addPreset();
		const history = await getHistory();
		expect(history.find((h) => h.current)!.seq).toBe(head + 1);
		expect(history.at(-1)!.label).toBe('Add animated title');
		expect((await revertTo(head)).svg_overlays).toBeUndefined();
	});

	test('the timeline handed back is a copy', async () => {
		const t = await addSvgOverlay({ custom: CUSTOM }, 0, 2);
		titles(t)[0].start = 99;
		expect(titles(await getTimeline())[0].start).toBe(0);
	});
});

describe('updating an animated title (browser harness)', () => {
	test('moves and resizes it, leaving what the patch does not name', async () => {
		const o = await addPreset('Hello', 1, 5);
		const t = await updateSvgOverlay(o.id, { start: 2, x: 0.1, width: 0.5, opacity: 0.4, name: ' Intro ' });
		expect(titles(t)[0]).toMatchObject({ start: 2, end: 5, x: 0.1, y: 0, width: 0.5, height: 1, opacity: 0.4, name: 'Intro' });
		expect(titles(t)[0].svg).toBe(o.svg);
	});

	test('re-fills a preset, which keeps its kind', async () => {
		const o = await addPreset('Hello');
		const t = await updateSvgOverlay(o.id, { preset_params: { text: 'Goodbye', color: 'teal' } });
		const u = titles(t)[0];
		expect(u.preset).toEqual({ kind: 'fade_up', params: { text: 'Goodbye', subtitle: null, color: 'teal', accent: o.preset!.params.accent, font: null } });
		expect(u.svg).toContain('>Goodbye<');
		expect(u.svg).not.toContain('>Hello<');
	});

	test('switches a preset to another kind in one edit, keeping its place and params', async () => {
		const first = await addPreset('Hello', 1, 6);
		const second = await addPreset('Second', 2);
		const head = (await getHistory()).find((h) => h.current)!.seq;
		const t = await updateSvgOverlay(first.id, { preset_kind: 'lower_third' });
		expect(titles(t).map((o) => o.id)).toEqual([first.id, second.id]);
		const u = titles(t)[0];
		expect(u).toMatchObject({ id: first.id, start: 1, end: 6 });
		expect(u.preset).toEqual({ kind: 'lower_third', params: first.preset!.params });
		expect(u.svg).not.toBe(first.svg);
		expect(u.svg).toContain('>Hello<');
		expect((await getHistory()).find((h) => h.current)!.seq).toBe(head + 1);
		const both = titles(await updateSvgOverlay(first.id, { preset_kind: 'word_pop', preset_params: { text: 'New' } }))[0];
		expect(both.preset).toMatchObject({ kind: 'word_pop', params: { text: 'New' } });
	});

	test('swapping in artwork makes it custom, and a custom title cannot be re-filled', async () => {
		const o = await addPreset();
		const custom = titles(await updateSvgOverlay(o.id, { svg: CUSTOM }))[0];
		expect(custom.svg).toBe(CUSTOM);
		expect(custom.preset).toBeUndefined();
		await expect(updateSvgOverlay(o.id, { preset_params: { text: 'x' } })).rejects.toThrow('custom SVG');
		await expect(updateSvgOverlay(o.id, { preset_kind: 'typewriter' })).rejects.toThrow('custom SVG');
	});

	test('refuses both at once, a bad length or placement, and a title that is not there', async () => {
		const o = await addPreset('Hello', 1, 5);
		await expect(updateSvgOverlay(o.id, { svg: CUSTOM, preset_params: { text: 'x' } })).rejects.toThrow('not both');
		await expect(updateSvgOverlay(o.id, { svg: CUSTOM, preset_kind: 'typewriter' })).rejects.toThrow('not both');
		await expect(updateSvgOverlay(o.id, { end: 1 })).rejects.toThrow('end must be after start');
		await expect(updateSvgOverlay(o.id, { start: 9 })).rejects.toThrow('end must be after start');
		await expect(updateSvgOverlay(o.id, { opacity: 2 })).rejects.toThrow('opacity');
		await expect(updateSvgOverlay(o.id, { start: Number.NaN })).rejects.toThrow('numbers of seconds');
		await expect(updateSvgOverlay(o.id, { preset_params: { text: '' } })).rejects.toThrow('some text');
		await expect(updateSvgOverlay('00000000-0000-0000-0000-000000000000', { name: 'x' })).rejects.toThrow('not found');
		// a refused update leaves the title exactly as it was
		expect(titles(await getTimeline())[0]).toEqual(o);
	});

	test('is one revision labelled as the core labels it', async () => {
		const o = await addPreset();
		const head = (await getHistory()).find((h) => h.current)!.seq;
		await updateSvgOverlay(o.id, { opacity: 0.5 });
		const history = await getHistory();
		expect(history.find((h) => h.current)!.seq).toBe(head + 1);
		expect(history.at(-1)!.label).toBe('Update animated title');
	});
});

describe('removing an animated title (browser harness)', () => {
	test('takes it out, and the list is absent again when it empties', async () => {
		const a = await addPreset('A');
		const b = await addPreset('B', 3);
		const one = await removeSvgOverlay(a.id);
		expect(titles(one).map((o) => o.id)).toEqual([b.id]);
		const none = await removeSvgOverlay(b.id);
		expect(none.svg_overlays).toBeUndefined();
		expect('svg_overlays' in none).toBe(false);
	});

	test('refuses a title that is not there, and says so in the history only when it removed one', async () => {
		await expect(removeSvgOverlay('00000000-0000-0000-0000-000000000000')).rejects.toThrow('not found');
		const o = await addPreset();
		await removeSvgOverlay(o.id);
		expect((await getHistory()).at(-1)!.label).toBe('Remove animated title');
	});
});
