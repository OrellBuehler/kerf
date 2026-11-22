import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
	BLOCKS,
	BLOCK_SECTIONS,
	InspectorMemory,
	PROFILES,
	WORKSPACE_KEYS,
	clipBlocks,
	defaultOpen,
	orderBlocks,
	profileKey,
	sectionId,
	type BlockId,
	type ClipFacts
} from './inspector-state';

const none: ClipFacts = {
	picture: false,
	hasAudio: false,
	soundDetached: false,
	titleClip: false,
	soundEffect: false,
	composedMusic: false,
	musicOffer: false,
	has360: false
};
/** Footage with a picture and sound, on a picture track. */
const video: ClipFacts = { ...none, picture: true, hasAudio: true };
/** A picture whose sound was detached. */
const silent: ClipFacts = { ...video, soundDetached: true };
const music: ClipFacts = { ...none, hasAudio: true, musicOffer: true };
const voice: ClipFacts = { ...none, hasAudio: true };

describe('which blocks a clip has', () => {
	test('footage with a picture and sound has every one a flat clip can have, in the order the panel always had', () => {
		expect(clipBlocks(video)).toEqual([
			'timing',
			'volume',
			'fades',
			'speed',
			'transform',
			'behaviours',
			'framing',
			'color',
			'grade',
			'stabilize',
			'mask',
			'transition',
			'audio-effects',
			'video-effects',
			'animation',
			'advanced'
		]);
	});

	test('a sound has no picture blocks, and its fit when it is music with a grid', () => {
		expect(clipBlocks(voice)).toEqual(['timing', 'volume', 'fades', 'speed', 'transition', 'audio-effects']);
		expect(clipBlocks(music)).toEqual(['timing', 'volume', 'music', 'fades', 'speed', 'transition', 'audio-effects']);
	});

	test('a picture whose sound was detached keeps its Volume note but has no audio effects', () => {
		const blocks = clipBlocks(silent);
		expect(blocks).toContain('volume');
		expect(blocks).not.toContain('audio-effects');
	});

	test('a picture with no sound has no Volume, and the sound of a sound effect or a score is first after Timing', () => {
		expect(clipBlocks({ ...video, hasAudio: false })).not.toContain('volume');
		expect(clipBlocks({ ...voice, soundEffect: true }).slice(0, 3)).toEqual(['timing', 'sound-effect', 'volume']);
		expect(clipBlocks({ ...voice, composedMusic: true }).slice(0, 3)).toEqual(['timing', 'composed-music', 'volume']);
	});

	test('a title on a track has its own block where Speed would be, and no Speed', () => {
		const blocks = clipBlocks({ ...video, titleClip: true, hasAudio: false });
		expect(blocks).toContain('title');
		expect(blocks).not.toContain('speed');
		expect(blocks.indexOf('title')).toBeLessThan(blocks.indexOf('transform'));
		expect(clipBlocks(video)).not.toContain('title');
	});

	test('the 360 override is under Advanced, and only a clip with nothing 360 about it gets that section', () => {
		expect(clipBlocks(video)).toContain('advanced');
		expect(clipBlocks(video)).not.toContain('360-reframe');
		const flat = clipBlocks({ ...video, has360: true });
		expect(flat).toContain('360-reframe');
		expect(flat).not.toContain('advanced');
		// The sound half of a linked pair has the picture's footage but nothing to reframe.
		expect(clipBlocks({ ...voice, has360: true })).not.toContain('360-reframe');
	});

	test('every block is placed once, and none is left out of the list', () => {
		const all = clipBlocks({ ...video, titleClip: false, has360: true, soundEffect: true, composedMusic: true, musicOffer: true });
		expect(new Set(all).size).toBe(all.length);
		expect(all.filter((b) => !BLOCKS.includes(b))).toEqual([]);
		// advanced and title are the two the same clip cannot have together with their opposites.
		for (const b of BLOCKS) if (b !== 'advanced' && b !== 'title') expect(all, b).toContain(b);
	});
});

describe('where a workspace puts the blocks', () => {
	const order = (ws: string | null | undefined, f: ClipFacts) => orderBlocks(ws, clipBlocks(f));

	test('Edit is today’s panel, for every kind of clip', () => {
		for (const f of [video, silent, music, voice, { ...video, titleClip: true }, { ...video, has360: true }])
			expect(order('edit', f)).toEqual(clipBlocks(f));
	});

	test('Color opens on the grade: the numbers, curves, wheels and LUT, then a mask and the effects', () => {
		expect(order('color', video).slice(0, 4)).toEqual(['color', 'grade', 'mask', 'video-effects']);
		// the rest follow in Edit's order
		expect(order('color', video).slice(4)).toEqual(clipBlocks(video).filter((b) => !['color', 'grade', 'mask', 'video-effects'].includes(b)));
	});

	test('Audio opens on the level, the fades and the audio effects, then the music’s fit', () => {
		expect(order('audio', video).slice(0, 3)).toEqual(['volume', 'fades', 'audio-effects']);
		expect(order('audio', music).slice(0, 4)).toEqual(['volume', 'fades', 'audio-effects', 'music']);
		expect(order('audio', { ...voice, soundEffect: true }).slice(0, 2)).toEqual(['sound-effect', 'volume']);
	});

	test('Motion opens on the pose, its keys and the speed', () => {
		expect(order('motion', video).slice(0, 5)).toEqual(['transform', 'animation', 'speed', 'behaviours', 'framing']);
	});

	test('Deliver opens on the framing', () => {
		expect(order('deliver', video).slice(0, 3)).toEqual(['framing', 'transform', 'timing']);
		expect(order('deliver', { ...video, has360: true }).slice(0, 3)).toEqual(['framing', '360-reframe', 'transform']);
	});

	test('a block the clip has not got is not placed, and one it has is placed exactly once', () => {
		for (const ws of WORKSPACE_KEYS)
			for (const f of [video, silent, music, voice, { ...video, titleClip: true }]) {
				const placed = order(ws, f);
				expect([...placed].sort()).toEqual([...clipBlocks(f)].sort());
			}
		expect(order('color', voice)).not.toContain('color');
		expect(order('motion', voice)).not.toContain('transform');
	});

	test('a workspace the user made up reads as Edit, and so does nothing at all', () => {
		for (const id of ['w-1a2b3c4d', 'w-', '', null, undefined, 'Color', 'constructor', '__proto__', 'toString', 'hasOwnProperty']) {
			expect(profileKey(id), String(id)).toBe('edit');
			expect(order(id, video), String(id)).toEqual(clipBlocks(video));
		}
		for (const id of WORKSPACE_KEYS) expect(profileKey(id)).toBe(id);
	});

	test('every block a workspace puts first is a real one, once', () => {
		for (const ws of WORKSPACE_KEYS) {
			const first = PROFILES[ws].first;
			expect(new Set(first).size, ws).toBe(first.length);
			for (const b of first) expect(BLOCKS, `${ws}: ${b}`).toContain(b as BlockId);
		}
	});
});

describe('what is open before the user says', () => {
	test('a workspace that has no say leaves the section as it is on its own', () => {
		expect(defaultOpen('edit', 'timing', true)).toBe(true);
		expect(defaultOpen('edit', 'fades', false)).toBe(false);
		expect(defaultOpen('w-9f8e7d6c', 'timing', true)).toBe(true);
	});

	test('Color opens the numbers and the wheels and folds Timing and the level away', () => {
		expect(defaultOpen('color', 'color', false)).toBe(true);
		expect(defaultOpen('color', 'color-wheels', false)).toBe(true);
		expect(defaultOpen('color', 'curves', false)).toBe(false);
		expect(defaultOpen('color', 'timing', true)).toBe(false);
		expect(defaultOpen('color', 'volume', true)).toBe(false);
	});

	test('Audio opens the level, the fades and the effects; Motion the pose and its keys; Deliver the framing', () => {
		for (const id of ['volume', 'fades', 'audio-effects']) expect(defaultOpen('audio', id, false), id).toBe(true);
		expect(defaultOpen('audio', 'timing', true)).toBe(false);
		for (const id of ['transform', 'animation']) expect(defaultOpen('motion', id, false), id).toBe(true);
		expect(defaultOpen('motion', 'volume', true)).toBe(false);
		expect(defaultOpen('deliver', 'framing', false)).toBe(true);
		expect(defaultOpen('deliver', 'timing', true)).toBe(false);
	});

	test('a section a workspace does not mention keeps its own say (a title’s name field stays open in Color)', () => {
		expect(defaultOpen('color', 'title', true)).toBe(true);
		expect(defaultOpen('audio', 'sound-effect', true)).toBe(true);
	});

	test('what a profile opens or closes is a section some component draws', () => {
		const sections = new Set<string>(BLOCKS.filter((b) => b !== 'grade' && b !== 'title'));
		for (const s of Object.values(BLOCK_SECTIONS)) for (const id of s ?? []) sections.add(id);
		for (const ws of WORKSPACE_KEYS)
			for (const id of [...PROFILES[ws].open, ...PROFILES[ws].close]) expect(sections.has(id), `${ws}: ${id}`).toBe(true);
	});

	test('no workspace both opens and closes a section', () => {
		for (const ws of WORKSPACE_KEYS) expect(PROFILES[ws].open.filter((id) => PROFILES[ws].close.includes(id)), ws).toEqual([]);
	});
});

describe('the section ids are the titles the components draw', () => {
	test('lower case, words hyphenated', () => {
		expect(sectionId('Color wheels')).toBe('color-wheels');
		expect(sectionId('Video effects')).toBe('video-effects');
		expect(sectionId('360 reframe')).toBe('360-reframe');
		expect(sectionId('  LUT ')).toBe('lut');
		expect(sectionId('Sound effect')).toBe('sound-effect');
	});

	test('every InspectorSection title of the panel reads as a block or one of a block’s sections, or is a title-only section', () => {
		const dir = join(import.meta.dir, 'components/editor');
		const files = ['Inspector', 'GradeSections', 'BehaviorsSection', 'StabilizeSection', 'TemplateSection', 'TextSection'];
		const titles = new Set<string>();
		for (const f of files) {
			const source = readFileSync(join(dir, `${f}.svelte`), 'utf8');
			for (const m of source.matchAll(/<InspectorSection\s+(?:id="[^"]*"\s+)?title="([^"]+)"/g)) titles.add(sectionId(m[1]));
			for (const m of source.matchAll(/\{@render fxBlock\(\s*'([^']+)'/g)) titles.add(sectionId(m[1]));
		}
		const known = new Set<string>(BLOCKS);
		for (const s of Object.values(BLOCK_SECTIONS)) for (const id of s ?? []) known.add(id);
		// What a profile names must be drawn by something.
		for (const ws of WORKSPACE_KEYS)
			for (const id of [...PROFILES[ws].open, ...PROFILES[ws].close]) expect(titles.has(id), `${ws}: ${id}`).toBe(true);
		for (const id of ['curves', 'color-wheels', 'lut', 'color', 'transform', 'framing', 'animation', 'volume', 'fades', 'audio-effects']) expect(titles.has(id), id).toBe(true);
	});
});

describe('what the user opened, closed and scrolled to', () => {
	test('an untouched section is as the workspace would have it', () => {
		const m = new InspectorMemory();
		expect(m.isOpen('color', 'color-wheels', false)).toBe(true);
		expect(m.isOpen('color', 'timing', true)).toBe(false);
		expect(m.isOpen('edit', 'timing', true)).toBe(true);
	});

	test('a choice is kept in the workspace it was made in, and not in the others', () => {
		const m = new InspectorMemory();
		m.set('color', 'curves', true, false);
		m.set('color', 'timing', true, true);
		expect(m.isOpen('color', 'curves', false)).toBe(true);
		expect(m.isOpen('color', 'timing', true)).toBe(true);
		expect(m.isOpen('audio', 'curves', false)).toBe(false);
		expect(m.isOpen('edit', 'curves', false)).toBe(false);
	});

	test('a round trip through other workspaces brings the choices back', () => {
		const m = new InspectorMemory();
		m.set('color', 'color-wheels', false, false);
		m.set('audio', 'timing', true, true);
		for (const ws of ['audio', 'motion', 'edit', 'deliver']) m.isOpen(ws, 'color-wheels', false);
		expect(m.isOpen('color', 'color-wheels', false)).toBe(false);
		expect(m.isOpen('audio', 'timing', true)).toBe(true);
	});

	test('closing a section the workspace opened and opening it again leaves no trace, so a default that moves is followed', () => {
		const m = new InspectorMemory();
		m.set('color', 'color-wheels', false, false);
		expect(m.isOpen('color', 'color-wheels', false)).toBe(false);
		m.set('color', 'color-wheels', true, false);
		expect(m.isOpen('color', 'color-wheels', false)).toBe(true);
		// the state the details element reports on first render is the default: nothing is recorded for it
		m.set('edit', 'timing', true, true);
		m.set('edit', 'fades', false, false);
		const after = new InspectorMemory();
		expect(after.isOpen('edit', 'timing', true)).toBe(m.isOpen('edit', 'timing', true));
	});

	test('a workspace the user made up has its own memory and Edit’s defaults', () => {
		const m = new InspectorMemory();
		m.set('w-1a2b3c4d', 'fades', true, false);
		expect(m.isOpen('w-1a2b3c4d', 'fades', false)).toBe(true);
		expect(m.isOpen('w-1a2b3c4d', 'timing', true)).toBe(true);
		expect(m.isOpen('w-5e6f7a8b', 'fades', false)).toBe(false);
		expect(m.isOpen('edit', 'fades', false)).toBe(false);
	});

	test('no workspace reads as the empty one, and does not throw', () => {
		const m = new InspectorMemory();
		m.set(undefined, 'fades', true, false);
		expect(m.isOpen(undefined, 'fades', false)).toBe(true);
		expect(m.isOpen(null, 'fades', false)).toBe(true);
	});

	test('the scroll is kept by workspace and subject, and a panel scrolled to the top keeps nothing', () => {
		const m = new InspectorMemory();
		expect(m.scroll('color', 'clip')).toBe(0);
		m.setScroll('color', 'clip', 377.4);
		m.setScroll('color', 'asset', 90);
		expect(m.scroll('color', 'clip')).toBe(377);
		expect(m.scroll('color', 'asset')).toBe(90);
		expect(m.scroll('audio', 'clip')).toBe(0);
		m.setScroll('color', 'clip', 0);
		expect(m.scroll('color', 'clip')).toBe(0);
		m.setScroll('color', 'clip', -5);
		expect(m.scroll('color', 'clip')).toBe(0);
	});

	test('a workspace id that looks like another’s key does not collide', () => {
		const m = new InspectorMemory();
		m.setScroll('a', 'clip', 10);
		expect(m.scroll('a\u0000clip', 'asset')).toBe(0);
		expect(m.scroll('a', 'clip')).toBe(10);
	});

	test('clear forgets everything', () => {
		const m = new InspectorMemory();
		m.set('color', 'curves', true, false);
		m.setScroll('color', 'clip', 50);
		m.clear();
		expect(m.isOpen('color', 'curves', false)).toBe(false);
		expect(m.scroll('color', 'clip')).toBe(0);
	});
});

describe('wired into the panel', () => {
	const read = (name: string) => readFileSync(join(import.meta.dir, 'components/editor', `${name}.svelte`), 'utf8');
	const inspector = read('Inspector');
	// a failing `toContain` prints the whole component: say what is missing instead
	const has = (source: string, text: string | RegExp, what: string) =>
		expect(typeof text === 'string' ? source.includes(text) : text.test(source), what).toBe(true);
	const lacks = (source: string, text: RegExp, what: string) => expect(text.test(source), what).toBe(false);

	test('the panel places the blocks as the active workspace says, and draws every one of them', () => {
		has(inspector, /orderBlocks\(\s*workspace\.shown/, 'orderBlocks(workspace.shown, …)');
		has(inspector, '{#each blocks as id (id)}', 'the each over the blocks');
		for (const b of BLOCKS) has(inspector, `id === '${b}'`, `a branch for ${b}`);
	});

	test('a section is kept under its title, and the ones a clip and a title share a word for have ids of their own', () => {
		const section = read('InspectorSection');
		has(section, 'id = sectionId(title)', 'the id is the title’s');
		has(section, 'inspectorMemory.isOpen(workspace.shown, id, open)', 'it reads the memory');
		has(section, 'inspectorMemory.set(workspace.shown, id, el.open, open)', 'it writes the memory');
		// the title editors' Timing / Text / Animation are not the clip's: a workspace that folds the clip's Timing away must not fold theirs
		for (const id of ['animated-title-timing', 'title-text', 'title-animation']) has(inspector, `id="${id}"`, id);
	});

	test('the scroll is kept per workspace and per subject, and the form has a width it stops at', () => {
		has(inspector, 'inspectorMemory.setScroll(workspace.shown, subject, scroller.scrollTop)', 'the scroll is written');
		has(inspector, 'inspectorMemory.scroll(workspace.shown, subject)', 'the scroll is read');
		has(inspector, /max-width:\{FORM_MAX\}px;margin:0 auto/, 'the form’s width');
	});

	test('every number a clip animates from the Transform section has its keyframe toggle', () => {
		for (const [label, prop] of [['Scale', 'scale'], ['Position X', 'pos_x'], ['Position Y', 'pos_y'], ['Rotation', 'rotation'], ['Opacity', 'opacity']]) {
			const at = inspector.indexOf(`rangeRow('${label}', tf.${prop},`);
			expect(at, label).toBeGreaterThan(-1);
			has(inspector.slice(at, inspector.indexOf('{@render', at + 10)), `'${prop}', liveTf(`, `${label}'s toggle`);
		}
	});

	test('a row’s label is the one ink the slider rows’ labels are, not the muted one a hint is', () => {
		lacks(
			inspector,
			/<span style="font-size:12px;color:var\(--text-muted\)[^"]*">(?:\{label\}|Text|Font|Name|Subtitle|Blend|Into clip|Source is|Start|End)<\/span>/,
			'a muted row label in the Inspector'
		);
		lacks(
			read('ThumbnailSection'),
			/<span style="width:64px[^"]*color:var\(--text-muted\)[^"]*">(?:Name|Frame|Size|Graphics|\{label\})<\/span>/,
			'a muted row label in the thumbnail section'
		);
	});

	test('Reset is back to a default, Clear takes keys or a ramp out, and both are a small button, never the ghost that reads as off', () => {
		for (const name of ['Inspector', 'SpeedRamp', 'GradeSections']) {
			const source = read(name);
			for (const m of source.matchAll(/<Btn\b[^>]*?variant="ghost"[\s\S]*?<\/Btn\s*>/g)) lacks(m[0], />\s*(Reset|Clear)\b/, `${name}: a ghost ${m[0].slice(0, 60)}`);
		}
		for (const label of ['Reset color', 'Reset crop', 'Clear keys', 'Clear keyframes']) has(inspector, `>${label}</Btn`, label);
		has(read('SpeedRamp'), 'Clear ramp', 'Clear ramp');
	});
});

describe('what each kind of subject keeps under its own ids', () => {
	const read = (name: string) => readFileSync(join(import.meta.dir, 'components/editor', `${name}.svelte`), 'utf8');
	const inspector = read('Inspector');
	const between = (from: string, to: string) => {
		const at = inspector.indexOf(from);
		expect(at, from).toBeGreaterThan(-1);
		return inspector.slice(at, inspector.indexOf(to, at + from.length));
	};
	/** The ids a source's sections are kept under: the explicit `id`, else the title's. */
	const idsOf = (source: string): string[] => [
		...[...source.matchAll(/<InspectorSection\s+title="([^"]+)"(?:\s+id="([^"]+)")?/g)].map((m) => m[2] ?? sectionId(m[1])),
		...[...source.matchAll(/\{@render fxBlock\(\s*'([^']+)'/g)].map((m) => sectionId(m[1]))
	];

	const kinds: Record<string, string[]> = {
		clip: [
			...idsOf(between('{#snippet clipBlock', '<div\n\trole="presentation"')),
			...['GradeSections', 'BehaviorsSection', 'StabilizeSection'].flatMap((f) => idsOf(read(f)))
		],
		title: idsOf(between('{#snippet overlayEditor', '{#snippet presetColor')),
		'animated title': idsOf(between('{#snippet svgEditor', '{#snippet clipBlock')),
		asset: idsOf(read('AssetInspector')),
		'template card': idsOf(read('TemplateInspector'))
	};
	// A title on a track and an animated title both draw the Template and Text sections of `TemplateSection` / `TextSection`:
	// the same sections, so the same ids.
	const shared = ['template', 'text'];

	test('every kind found its sections', () => {
		for (const [kind, ids] of Object.entries(kinds)) expect(ids.length, kind).toBeGreaterThan(1);
		expect(kinds.clip).toContain('composed-music');
		expect(kinds.asset).toContain('asset-composed-music');
	});

	test('no two kinds of subject keep a section under one id, so an asset’s Composed music is not the clip’s', () => {
		const names = Object.keys(kinds);
		for (const a of names)
			for (const b of names) {
				if (a >= b) continue;
				const both = kinds[a].filter((id) => kinds[b].includes(id) && !shared.includes(id));
				expect(both, `${a} and ${b}`).toEqual([]);
			}
	});

	test('an id is used once within a kind, but for Volume, which is one section whether its sound is the clip’s or detached', () => {
		for (const [kind, ids] of Object.entries(kinds))
			expect(ids.filter((id, i) => ids.indexOf(id) !== i && id !== 'volume'), kind).toEqual([]);
	});

	test('what the clip, the title editors, the asset and the template card draw is prefixed by its kind', () => {
		for (const id of kinds.asset) expect(id.startsWith('asset-'), id).toBe(true);
		for (const id of kinds['template card']) expect(id.startsWith('template-card-'), id).toBe(true);
		for (const id of kinds.title) expect(id.startsWith('title-'), id).toBe(true);
		for (const id of kinds['animated title'].filter((i) => !shared.includes(i))) expect(id.startsWith('animated-title-'), id).toBe(true);
	});

	test('opening a section from code is not the user opening it', () => {
		const section = read('InspectorSection');
		expect(section.includes('export function revealSection')).toBe(true);
		expect(section.includes('el.dataset.revealing !== undefined')).toBe(true);
		expect(inspector.includes('revealSection(section)')).toBe(true);
		// no other way of opening a `<details>` from code
		expect(/\bsection\.open = true/.test(inspector)).toBe(false);
		expect(/\.open = true/.test(inspector)).toBe(false);
	});
});

describe('every colour swatch notes what was picked as it is picked', () => {
	// A native colour box fires `input` as it is dragged and `change` when it is let go; by `change` the selection may have
	// moved and the box been drawn again with another title's colour, so what was picked is noted on `input` and read from the note
	// (`title-field-edit.test.ts` drives the order). Every swatch the Inspector and its sections draw does it.
	for (const file of ['Inspector', 'TemplateSection']) {
		test(`${file}: each type="color" input takes focus and input into the field edits`, () => {
			const source = readFileSync(join(import.meta.dir, 'components/editor', `${file}.svelte`), 'utf8');
			const swatches = source.match(/<input\b[^>]*?type="color"[\s\S]*?\/>/g) ?? [];
			expect(swatches.length, file).toBeGreaterThanOrEqual(file === 'Inspector' ? 2 : 1);
			for (const tag of swatches) {
				expect(tag.includes('onfocus={noteFocus}'), `${file}: focus`).toBe(true);
				expect(tag.includes('oninput={noteTyped}'), `${file}: input`).toBe(true);
			}
		});
	}

	test('the Inspector’s title commits go through the one function that builds a change from the title it began on', () => {
		const inspector = readFileSync(join(import.meta.dir, 'components/editor/Inspector.svelte'), 'utf8');
		expect(inspector.includes('commitTitleField(')).toBe(true);
		expect(inspector.includes('colorHandlers(field)')).toBe(true);
		// and an animated title's fields, which write asynchronously, put a field back to the row it is drawn from now
		const svg = inspector.slice(inspector.indexOf('async function patchSvg('), inspector.indexOf('/** Whether the playhead is inside the title'));
		expect(svg.includes('svgOf(scope)')).toBe(true);
	});
});

describe('the header says which clip it is, and can be reached without a pointer', () => {
	const inspector = readFileSync(join(import.meta.dir, 'components/editor/Inspector.svelte'), 'utf8');
	const has = (text: string | RegExp, what: string) => expect(typeof text === 'string' ? inspector.includes(text) : text.test(inspector), what).toBe(true);

	test('the name keeps a width of its own and the facts wrap under it', () => {
		has(/\.who \{[^}]*min-width: 120px/, 'a minimum for the name');
		has(/\.subject \{[^}]*flex-wrap: wrap/, 'the line wraps');
		has(/\.ell \{[^}]*text-overflow: ellipsis/, 'the track and the link ellipsize');
	});

	test('the link badge has its name in text a screen reader reads, and the pill is a button with one', () => {
		has('<span class="sr-only">Linked to&nbsp;</span>', 'Linked to, for a screen reader');
		has('aria-label="Analysis, {meter.text}. Open the Analyze menu."', 'the pill’s name');
	});

	test('the two one-line notes have a More button that says whether it is open, and the explanation it opens', () => {
		has('aria-expanded={showDetachedWhy}', 'the detached sound’s');
		has('aria-expanded={showSelectionWhy}', 'the selection’s');
		has('id="detached-why"', 'its text');
		has('id="selection-why"', 'its text');
	});

	test('a key’s time is timecode wherever the Inspector lists one', () => {
		expect(/k\.time\.toFixed/.test(inspector)).toBe(false);
		expect((inspector.match(/timeDisplay\.fmt\(k\.time\)/g) ?? []).length).toBeGreaterThanOrEqual(5);
	});
});

describe('a key is only put where the clip is', () => {
	const inspector = readFileSync(join(import.meta.dir, 'components/editor/Inspector.svelte'), 'utf8');

	test('every ◇, the pose’s Keyframe at playhead and the camera key are off, with their reason, while the playhead is not on the clip', () => {
		expect(inspector.includes('pending={editor.locked || (!here && !onClip)}')).toBe(true);
		expect(inspector.includes('pending={editor.locked || !onClip}')).toBe(true);
		expect((inspector.match(/OFF_CLIP/g) ?? []).length).toBeGreaterThanOrEqual(4);
		// and the writers refuse as well, for a key press or a menu that got there another way
		expect(/function addKeyframeHere\(\) \{[^}]*!onClip/.test(inspector)).toBe(true);
		expect(/function toggleKey\(prop: Property\) \{[^}]*!onClip\) return;/.test(inspector)).toBe(true);
	});

	test('a number whose edit is a key is off with its reason while the playhead is off the clip, and its writers refuse as well', () => {
		expect(inspector.includes('data-key-refused')).toBe(true);
		expect(inspector.includes('editWritesKey(clip, animate)')).toBe(true);
		// every writer of a key at the playhead asks first: the numbers' keys, the pose's bundle and the camera's
		for (const fn of ['function keyAtPlayhead(', 'function setTf(', 'function setCam(']) {
			const at = inspector.indexOf(fn);
			expect(at, fn).toBeGreaterThan(-1);
			expect(inspector.slice(at, at + 1500).includes('refuseOffClip('), fn).toBe(true);
		}
		// the camera's rows say so too: they have no ◇ to say it
		expect((inspector.match(/reframeKeys\.length > 0\)\}/g) ?? []).length).toBe(4);
	});

	test('Remove keyframe at the playhead is not offered by a key the playhead only clamps to', () => {
		expect(inspector.includes('keyHere(c, ui.time, propertyKeys(c, prop).map((k) => k.time))')).toBe(true);
		expect(inspector.includes('near.here = keyHere(')).toBe(true);
		expect(/Math\.abs\(k\.time - localTime\) <= 0\.02\)\s*:\s*false/.test(inspector)).toBe(false);
	});

	test('a moment held by both stores is two edits, and the two steps are said where they are made', () => {
		expect(inspector.includes('keyRemoval(c, row, keyRows)')).toBe(true);
		expect(inspector.includes('**two** edits')).toBe(true);
		expect(inspector.includes('Project::batch')).toBe(true);
	});

	test('the Transform summary and the Animation section read both ways of keying', () => {
		expect(inspector.includes('transformSummary(clip, tf)')).toBe(true);
		expect(inspector.includes("count(keyRows.length, 'keyframe')")).toBe(true);
		expect(/\bclip\.keyframes\b/.test(inspector.slice(inspector.indexOf("{:else if id === 'animation'}"), inspector.indexOf("{:else if id === 'advanced'}")))).toBe(false);
	});
});
