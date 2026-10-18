import { describe, expect, test } from 'bun:test';
import { Generation, ReadOrder } from './generation';

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

describe('ReadOrder', () => {
	test('of the reads that come back, the one that started last writes', () => {
		const o = new ReadOrder();
		const older = o.begin();
		const newer = o.begin();
		expect(o.land(newer)).toBe(true);
		expect(o.land(older)).toBe(false);
	});

	test('a newer read that failed takes nobody’s place: the older answer still lands', () => {
		const o = new ReadOrder();
		const older = o.begin();
		o.begin(); // started later, never landed — its read failed
		expect(o.land(older)).toBe(true);
	});

	test('an older answer that comes first lands, and the newer one then replaces it', () => {
		const o = new ReadOrder();
		const older = o.begin();
		const newer = o.begin();
		expect(o.land(older)).toBe(true);
		expect(o.isLatest(older)).toBe(true);
		expect(o.land(newer)).toBe(true);
		expect(o.isLatest(older)).toBe(false);
		expect(o.isLatest(newer)).toBe(true);
	});

	test('an answer lands once', () => {
		const o = new ReadOrder();
		const t = o.begin();
		expect(o.land(t)).toBe(true);
		expect(o.land(t)).toBe(false);
	});

	test('invalidate makes every read in flight stale, the newest included, and not the next one', () => {
		const o = new ReadOrder();
		const a = o.begin();
		const b = o.begin();
		o.invalidate();
		expect(o.land(a)).toBe(false);
		expect(o.land(b)).toBe(false);
		expect(o.isLatest(b)).toBe(false);
		const next = o.begin();
		expect(o.land(next)).toBe(true);
	});
});
