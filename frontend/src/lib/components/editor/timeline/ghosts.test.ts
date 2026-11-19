import { describe, expect, test } from 'bun:test';
import { byTrack } from './ghosts';

describe('byTrack', () => {
	test('groups ghosts by track: tracks in the order they first appear, each list in the order given', () => {
		const ghosts = [
			{ trackId: 'a', id: 1 },
			{ trackId: 'b', id: 2 },
			{ trackId: 'a', id: 3 }
		];
		const by = byTrack(ghosts);
		expect([...by.keys()]).toEqual(['a', 'b']);
		expect(by.get('a')).toEqual([ghosts[0], ghosts[2]]);
		expect(by.get('b')).toEqual([ghosts[1]]);
	});

	test('keeps the ghosts themselves, not copies, and says nothing of a track with none', () => {
		const g = { trackId: 'a' };
		expect(byTrack([g]).get('a')![0]).toBe(g);
		expect(byTrack([g]).get('b')).toBeUndefined();
		expect(byTrack([]).size).toBe(0);
	});
});
