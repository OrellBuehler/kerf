import { describe, expect, test } from 'bun:test';
import { clipsUsing, missingLine, relinkLine, removedLine } from './library-copy';
import { overwriteCopy, removeMediaCopy } from './confirm-copy';
import type { Timeline } from './types';

const clip = (id: string, asset: string) => ({ id, asset_id: asset, source_in: 0, source_out: 1, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 });
const timeline: Timeline = {
	tracks: [
		{ id: 'v', kind: 'video', name: 'V1', clips: [clip('1', 'a'), clip('2', 'b'), clip('3', 'a')] },
		{ id: 'a', kind: 'audio', name: 'A1', clips: [clip('4', 'a')] }
	]
};

describe('who uses a piece of media', () => {
	test('counts the clips of every track', () => {
		expect(clipsUsing(timeline, 'a')).toBe(3);
		expect(clipsUsing(timeline, 'b')).toBe(1);
		expect(clipsUsing(timeline, 'zzz')).toBe(0);
		expect(clipsUsing(null, 'a')).toBe(0);
	});
});

describe('the words', () => {
	test('a relink says what it cost when the file is a different recording', () => {
		expect(relinkLine('a.mp4', { analysis_kept: true })).toBe('Relinked a.mp4');
		expect(relinkLine('a.mp4', { analysis_kept: false })).toContain('analysis was dropped');
	});

	test('missing media is counted', () => {
		expect(missingLine([])).toBe('');
		expect(missingLine([{ name: 'broll.mp4' }])).toBe('broll.mp4 can’t be found');
		expect(missingLine([{ name: 'a' }, { name: 'b' }])).toBe('2 media files can’t be found');
	});

	test('a removal names the clips that went with it', () => {
		expect(removedLine('a.mp4', 0)).toBe('Removed a.mp4');
		expect(removedLine('a.mp4', 1)).toBe('Removed a.mp4 and 1 clip');
		expect(removedLine('a.mp4', 3)).toBe('Removed a.mp4 and 3 clips');
	});
});

describe('the questions', () => {
	test('unused media is a plain one, used media says what goes with it', () => {
		const plain = removeMediaCopy('a.mp4', 0);
		expect(plain.title).toBe('Remove a.mp4?');
		expect(plain.message).toContain('file on disk stays');
		expect(plain.ok).toBe('Remove media');

		const used = removeMediaCopy('a.mp4', 3);
		expect(used.message).toContain('3 clips on the timeline');
		expect(used.message).toContain('cannot be undone');
		expect(used.ok).toBe('Remove media and clips');
		expect(removeMediaCopy('a.mp4', 1).ok).toBe('Remove media and clip');

		const both = removeMediaCopy('a.mp4', 2, true);
		expect(both.message).toContain('2 clips on the timeline and in the agent’s proposal');
		const proposalOnly = removeMediaCopy('a.mp4', 0, true);
		expect(proposalOnly.message).toContain('the agent’s proposal');
		expect(proposalOnly.ok).toBe('Remove media');
	});

	test('no button is a bare Yes or OK', () => {
		for (const c of [removeMediaCopy('x', 0), removeMediaCopy('x', 2), overwriteCopy(['/a/b.mp4'])]) {
			expect(c.ok).not.toMatch(/^(yes|ok)$/i);
			expect(c.title.endsWith('?')).toBe(true);
		}
	});

	test('replacing files names them, a few at a time', () => {
		const one = overwriteCopy(['/out/cut.mp4']);
		expect(one.title).toBe('Replace the existing file?');
		expect(one.message).toBe('cut.mp4 is already there. Exporting replaces it.');
		const many = overwriteCopy(['/o/a-9x16.mp4', '/o/a-1x1.mp4', '/o/a-16x9.mp4', '/o/d.mp4', '/o/e.mp4', '/o/f.mp4']);
		expect(many.title).toBe('Replace 6 existing files?');
		expect(many.message).toContain('and 2 more');
		expect(many.ok).toBe('Replace all');
	});
});
