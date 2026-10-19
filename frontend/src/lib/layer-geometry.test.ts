import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/layer-geometry-corpus.json';
import { ffmpegAlpha, pixFmtSubsampling, resolveLayer, resolveLayerAnyGrid, type Keyed, type Subsampling } from './layer-geometry';
import type { Fit, Transform } from './types';

// The differential test for the port of kerf-core's `layer_geometry.rs`: kerf-core writes what it
// answered for each picture, canvas, fit, transform, grid and placement (`layer_geometry/corpus.rs`
// → `fixtures/layer-geometry-corpus.json`) and every case is replayed here. Regenerate with
// `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- layer_geometry_corpus`
// after an intended change; this test then says what the port has to follow.

interface Case {
	src: [number, number];
	canvas: [number, number];
	fit: Fit;
	tf: Transform;
	sub: Subsampling | null;
	keyed: Keyed | null;
	offset: [number, number];
	expect: unknown;
}

const cases = corpus.cases as unknown as Case[];

describe('layer geometry corpus', () => {
	test('has cases', () => {
		expect(cases.length).toBeGreaterThan(400);
	});
	for (const [i, c] of cases.entries()) {
		test(`case ${i}: ${c.src.join('x')} on ${c.canvas.join('x')} ${c.fit}`, () => {
			const placement = { keyframed: c.keyed, offset: c.offset };
			const got = c.sub ? resolveLayer(c.src, c.canvas, c.fit, c.tf, c.sub, placement) : resolveLayerAnyGrid(c.src, c.canvas, c.fit, c.tf, placement);
			expect(got).toEqual(c.expect as never);
		});
	}
	test('pixel formats', () => {
		for (const p of corpus.pix_fmts) expect([p.name, pixFmtSubsampling(p.name)]).toEqual([p.name, p.sub as Subsampling | null]);
	});
});

test('the alpha FFmpeg writes: 50% is 129, not 128', () => {
	expect(ffmpegAlpha(0.5)).toBe(129);
	expect(ffmpegAlpha(0)).toBe(0);
	expect(ffmpegAlpha(1)).toBe(255);
});
