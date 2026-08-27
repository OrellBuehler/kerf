import { describe, expect, test } from 'bun:test';
import { check, checkAll, fmtDur, TARGETS } from './platforms';
import type { CutSummary } from './platforms';

const target = (id: string) => TARGETS.find((t) => t.id === id)!;

const vertical = (duration: number): CutSummary => ({
	duration,
	width: 1080,
	height: 1920,
	has_audio: true,
	has_text: true
});

describe('fmtDur', () => {
	test('reads as a length, not a timecode', () => {
		expect(fmtDur(90)).toBe('1:30');
		expect(fmtDur(180)).toBe('3:00');
		expect(fmtDur(9)).toBe('0:09');
	});
});

describe('check', () => {
	test('a well-shaped cut passes clean', () => {
		const c = check(target('reels'), vertical(45));
		expect(c.ok).toBe(true);
		expect(c.issues).toEqual([]);
	});

	test('over the hard limit is an error naming the overshoot', () => {
		const c = check(target('shorts'), vertical(200));
		expect(c.ok).toBe(false);
		expect(c.issues[0].severity).toBe('error');
		expect(c.issues[0].message).toContain('3:00');
		expect(c.issues[0].message).toContain('0:20');
	});

	test('over the reach limit is a warning, not a rejection', () => {
		// The one that matters: this uploads fine and then nobody new sees it.
		const c = check(target('reels'), vertical(4 * 60));
		expect(c.ok).toBe(true);
		const warn = c.issues.find((i) => i.severity === 'warning')!;
		expect(warn.message).toContain('follow you');
		expect(warn.message).toContain('1:00');
	});

	test('a reach warning is dropped once the cut is already rejected', () => {
		const c = check(target('tiktok'), vertical(70 * 60));
		expect(c.issues.filter((i) => i.severity === 'warning')).toHaveLength(0);
		expect(c.issues.filter((i) => i.severity === 'error')).toHaveLength(1);
	});

	test('a landscape cut is flagged for a vertical feed', () => {
		const c = check(target('reels'), { ...vertical(30), width: 1920, height: 1080 });
		expect(c.ok).toBe(true);
		expect(c.issues[0].message).toContain('16:9');
		expect(c.issues[0].message).toContain('1080×1920');
	});

	test('aspect is compared as a ratio, not as pixels', () => {
		const c = check(target('reels'), { ...vertical(30), width: 720, height: 1280 });
		expect(c.issues).toHaveLength(1);
		expect(c.issues[0].message).toContain('upscale');
	});

	test('the muted-feed tip only fires when there is sound to miss', () => {
		const noText = { ...vertical(30), has_text: false };
		expect(check(target('reels'), noText).issues.some((i) => i.severity === 'tip')).toBe(true);
		expect(check(target('reels'), { ...noText, has_audio: false }).issues.some((i) => i.severity === 'tip')).toBe(false);
	});

	test('an empty timeline is rejected everywhere', () => {
		expect(checkAll({ ...vertical(0), duration: 0 }).every((c) => !c.ok)).toBe(true);
	});
});
