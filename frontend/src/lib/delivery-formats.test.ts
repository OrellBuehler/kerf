import { describe, expect, test } from 'bun:test';
import { DELIVERY_PRESETS, fitLabel, presetFor, ratioLabel, variantPath } from './delivery-formats';

describe('variantPath', () => {
	test('splices the shape into the file name beside the base', () => {
		expect(variantPath('/renders/cut.mp4', { width: 1080, height: 1920, fit: 'cover' })).toBe('/renders/cut-9x16.mp4');
		expect(variantPath('C:\\out\\my.cut.mov', { width: 1080, height: 1080, fit: 'cover' })).toBe('C:\\out\\my.cut-1x1.mov');
		expect(variantPath('cut', { width: 1920, height: 1080, fit: 'contain' })).toBe('cut-16x9');
	});
});

describe('ratioLabel', () => {
	test('reduces a frame to its aspect', () => {
		expect(ratioLabel(1920, 1080)).toBe('16:9');
		expect(ratioLabel(1080, 1920)).toBe('9:16');
		expect(ratioLabel(1080, 1080)).toBe('1:1');
		expect(ratioLabel(1080, 1350)).toBe('4:5');
	});
});

describe('presetFor', () => {
	test('an unset format is the source-shape preset', () => {
		expect(presetFor(null).id).toBe('source');
		expect(presetFor(undefined).format).toBeNull();
	});

	test('matches a known frame by its dimensions, ignoring fit', () => {
		expect(presetFor({ width: 1080, height: 1920, fit: 'contain' }).id).toBe('vertical');
		expect(presetFor({ width: 1920, height: 1080, fit: 'cover' }).id).toBe('landscape');
	});

	test('an unknown frame becomes a labelled custom entry', () => {
		const p = presetFor({ width: 2000, height: 1000, fit: 'cover' });
		expect(p.id).toBe('custom');
		expect(p.label).toBe('2:1');
		expect(p.hint).toBe('2000×1000');
	});
});

describe('DELIVERY_PRESETS', () => {
	test('social frames fill and crop; only the landscape one letterboxes', () => {
		// Cover is what makes a vertical delivery a usable shot rather than a
		// strip of picture in a black field.
		for (const p of DELIVERY_PRESETS) {
			if (!p.format) continue;
			const vertical = p.format.width < p.format.height;
			expect(p.format.fit).toBe(vertical ? 'cover' : p.id === 'square' ? 'cover' : 'contain');
		}
	});

	test('every preset round-trips through presetFor', () => {
		for (const p of DELIVERY_PRESETS) expect(presetFor(p.format).id).toBe(p.id);
	});
});

describe('fitLabel', () => {
	test('says what the fit does, not what it is called', () => {
		expect(fitLabel('cover')).toBe('fill & crop');
		expect(fitLabel('contain')).toBe('fit & letterbox');
	});
});
