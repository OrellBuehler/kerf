import { expect, test } from 'bun:test';
import { MAX_FRAMES, frameAt, frameTimes } from './template-frames';

test('the preview draws evenly over the template, never more than the engine takes in a call', () => {
	expect(frameTimes(1, 4)).toEqual([0.125, 0.375, 0.625, 0.875]);
	expect(frameTimes(60)).toHaveLength(MAX_FRAMES);
	expect(frameTimes(0)).toEqual([0]);
});

test('a time shows the picture whose step holds it', () => {
	expect(frameAt(0, 2, 4)).toBe(0);
	expect(frameAt(0.6, 2, 4)).toBe(1);
	expect(frameAt(2, 2, 4)).toBe(3);
	expect(frameAt(1, 0, 4)).toBe(0);
});
