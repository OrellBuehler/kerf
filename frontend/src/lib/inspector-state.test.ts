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
