import { describe, expect, test } from 'bun:test';
import { flagBtn } from './flags';

describe('flagBtn', () => {
	test('lit, the flag is filled with its accent and reads on it', () => {
		const style = flagBtn(true, 'var(--kerf-400)');
		expect(style).toContain('border:var(--line-width) solid var(--kerf-400);background:var(--kerf-400);color:var(--text-on-accent)');
	});

	test('off, it is an outline with muted text', () => {
		const style = flagBtn(false, 'var(--kerf-400)');
		expect(style).toContain('border:var(--line-width) solid var(--border-strong);background:transparent;color:var(--text-muted)');
		expect(style).not.toContain('--kerf-400');
	});

	test('the size and the look are the same either way', () => {
		const rest = (s: string) => s.slice(0, s.indexOf('border:var(--line-width)'));
		expect(rest(flagBtn(true, 'var(--x)'))).toBe(rest(flagBtn(false, 'var(--x)')));
		expect(rest(flagBtn(true, 'var(--x)'))).toContain('min-width:26px;min-height:26px');
	});
});
