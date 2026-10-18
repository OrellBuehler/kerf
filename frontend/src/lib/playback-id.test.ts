import { describe, expect, test } from 'bun:test';
import { PLAYBACK_ID_SPAN, PLAYBACK_MAX_SESSION, playbackId } from './playback-id';

describe('playbackId', () => {
	test('puts the session in the high bits and the count in the low ones', () => {
		expect(playbackId(1, 1)).toBe(PLAYBACK_ID_SPAN + 1);
		expect(playbackId(3, 0)).toBe(3 * PLAYBACK_ID_SPAN);
	});

	test('a reloaded page outranks everything the page before it used', () => {
		// Page one played 900 times; page two restarts its own count at 1.
		expect(playbackId(2, 1)).toBeGreaterThan(playbackId(1, 900));
		expect(playbackId(2, 2)).toBeGreaterThan(playbackId(2, 1));
	});

	test('the same backend vector is spelled in Rust (page_id in lib.rs tests)', () => {
		// (session << 32) | n, as the Rust test builds it.
		expect(playbackId(7, 5)).toBe(7 * 4294967296 + 5);
	});

	test('every id up to the last session stays an exact integer', () => {
		const largest = playbackId(PLAYBACK_MAX_SESSION, PLAYBACK_ID_SPAN - 1);
		expect(Number.isSafeInteger(largest)).toBe(true);
	});

	test('refuses a session an id cannot carry', () => {
		expect(() => playbackId(-1, 1)).toThrow();
		expect(() => playbackId(PLAYBACK_MAX_SESSION + 1, 1)).toThrow();
		expect(() => playbackId(1.5, 1)).toThrow();
	});

	test('a count past the span wraps rather than reaching the next session', () => {
		expect(playbackId(1, PLAYBACK_ID_SPAN + 4)).toBe(playbackId(1, 4));
	});
});
