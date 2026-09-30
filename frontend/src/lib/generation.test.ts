import { describe, expect, test } from 'bun:test';
import { Generation } from './generation';

describe('Generation', () => {
	test('starts at a value every snapshot agrees with until something advances', () => {
		const g = new Generation();
		const snap = g.read();
		expect(g.isCurrent(snap)).toBe(true);
	});

	test('sequence guard: only the most recently started call may commit', () => {
		// select(assetA) then select(assetB) — A resolves slow, B fast.
		const g = new Generation();
		const a = g.advance();
		const b = g.advance();
		expect(g.isCurrent(a)).toBe(false); // A's response must be dropped
		expect(g.isCurrent(b)).toBe(true); // B's response wins
	});

	test('sequence guard: a lone call always commits', () => {
		const g = new Generation();
		const a = g.advance();
		expect(g.isCurrent(a)).toBe(true);
	});

	test('snapshot guard: an unrelated write invalidates an in-flight fetch', () => {
		// refreshTimeline() reads a snapshot, then a local edit (#apply) commits
		// before the fetch resolves — the refresh must lose.
		const g = new Generation();
		const refreshSnapshot = g.read();
		g.advance(); // the local edit's own commit
		expect(g.isCurrent(refreshSnapshot)).toBe(false);
	});

	test('snapshot guard: no write in between lets the fetch commit and advance', () => {
		const g = new Generation();
		const refreshSnapshot = g.read();
		expect(g.isCurrent(refreshSnapshot)).toBe(true);
		g.advance(); // the refresh's own commit also counts as a write
		expect(g.isCurrent(refreshSnapshot)).toBe(false);
	});

	test('two overlapping snapshot fetches: only the one unstaled by a write commits', () => {
		const g = new Generation();
		const first = g.read();
		const second = g.read(); // started before either resolved, same snapshot
		expect(first).toBe(second);
		g.advance(); // some fetch (or edit) commits first
		expect(g.isCurrent(first)).toBe(false);
		expect(g.isCurrent(second)).toBe(false);
	});
});
