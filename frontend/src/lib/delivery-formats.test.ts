import { describe, expect, test } from 'bun:test';
import {
	checkDeliveryFps,
	DELIVERY_FITS,
	DELIVERY_PRESETS,
	DELIVERY_RATES,
	deliverLabel,
	deliveryLabel,
	deliveryTip,
	fitChoices,
	fitLabel,
	fitName,
	formatsFor,
	frameLine,
	frameShapes,
	fpsLabel,
	hasPicture,
	keepRate,
	presetFor,
	ratioLabel,
	rateChoices,
	rateOptions,
	sameRate,
	VARIANT_PRESETS,
	variantPath,
	withRate
} from './delivery-formats';

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
		expect(fitLabel('blur')).toBe('fit & blurred fill');
	});

	test('every fit a frame can take is offered once', () => {
		expect([...DELIVERY_FITS].sort()).toEqual(['blur', 'contain', 'cover']);
	});
});

describe('the shapes a multi-format export offers', () => {
	test('are every preset that is a frame', () => {
		expect(VARIANT_PRESETS.map((p) => p.id)).toEqual(['landscape', 'vertical', 'square', 'portrait']);
		expect(VARIANT_PRESETS.every((p) => p.format !== null)).toBe(true);
	});

	test('formatsFor gives the frames in picker order, whatever order they were ticked', () => {
		expect(formatsFor(['square', 'vertical']).map((f) => `${f.width}x${f.height}`)).toEqual(['1080x1920', '1080x1080']);
		expect(formatsFor([])).toEqual([]);
	});

	test('formatsFor ignores ids that are not shapes — Source is not a file to write', () => {
		expect(formatsFor(['source', 'nonsense'])).toEqual([]);
		expect(formatsFor(['source', 'portrait'])).toHaveLength(1);
	});
});

describe('hasPicture', () => {
	const clip = { id: 'c', asset_id: 'a', source_in: 0, source_out: 1, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 };
	const track = (kind: 'video' | 'audio', clips: unknown[]) => ({ id: kind, name: kind, kind, clips }) as never;

	test('is a clip on a video track', () => {
		expect(hasPicture({ tracks: [track('video', [clip])] } as never)).toBe(true);
		expect(hasPicture({ tracks: [track('audio', [clip]), track('video', [clip])] } as never)).toBe(true);
	});

	test('an audio-only cut, an empty video track and an empty timeline have none', () => {
		expect(hasPicture({ tracks: [track('audio', [clip])] } as never)).toBe(false);
		expect(hasPicture({ tracks: [track('video', [])] } as never)).toBe(false);
		expect(hasPicture({ tracks: [] } as never)).toBe(false);
	});
});

describe('the Frame button', () => {
	test("says and ticks the project's fit, not the matching preset's", () => {
		// 9:16 is a preset that fills; the project fits over a blurred fill.
		const blurred = { width: 1080, height: 1920, fit: 'blur' as const };
		expect(presetFor(blurred).format?.fit).toBe('cover');
		expect(deliveryTip(blurred)).toContain('Project frame: 1080\u00d71920, fit & blurred fill.');
		expect(fitChoices(blurred).filter((c) => c.checked).map((c) => c.fit)).toEqual(['blur']);
		expect(fitChoices(blurred).map((c) => c.label)).toEqual([
			'Footage: fill & crop',
			'Footage: fit & letterbox',
			'Footage: fit & blurred fill'
		]);
		expect(fitChoices(null)).toEqual([]);
		expect(deliveryTip(null)).toContain('Project frame: follows the footage.');
	});

	test('says what it is for, and where else it is set', () => {
		for (const f of [null, { width: 1080, height: 1920, fit: 'cover' as const }]) {
			const tip = deliveryTip(f);
			expect(tip).toContain('The Preview shows it and an export renders it.');
			expect(tip).toContain('Deliver workspace');
		}
	});
});

describe('the project frame is called what it is', () => {
	test('the shape that follows the footage is Footage, not Source', () => {
		expect(DELIVERY_PRESETS[0]).toMatchObject({ id: 'source', label: 'Footage', format: null });
		expect(DELIVERY_PRESETS.map((p) => p.label)).toEqual(['Footage', '16:9', '9:16', '1:1', '4:5']);
	});

	test('the line says the size, the shape, the fit and the rate, or that it follows the footage', () => {
		expect(frameLine(null)).toBe('Follows the footage');
		expect(frameLine({ width: 1080, height: 1920, fit: 'cover' })).toBe('1080×1920 · 9:16 · fill & crop');
		expect(frameLine({ width: 1080, height: 1920, fit: 'blur', fps: 50 })).toBe('1080×1920 · 9:16 · fit & blurred fill · 50 fps');
		expect(frameLine({ width: 1920, height: 1080, fit: 'contain', fps: checkDeliveryFps(29.97) })).toBe('1920×1080 · 16:9 · fit & letterbox · 29.97 fps');
	});

	test('a frame outside the presets is shown as a Custom shape, chosen and saying its size', () => {
		const agent = { width: 3840, height: 2160, fit: 'contain' as const };
		const shapes = frameShapes(agent);
		expect(shapes.map((p) => p.label)).toEqual(['Footage', '16:9', '9:16', '1:1', '4:5', 'Custom']);
		expect(shapes.at(-1)).toMatchObject({ id: 'custom', hint: '3840×2160' });
		expect(presetFor(agent).id).toBe('custom');
		expect(frameLine(agent)).toBe('3840×2160 · 16:9 · fit & letterbox');
		// a preset size, or none, has no Custom state
		expect(frameShapes(null)).toBe(DELIVERY_PRESETS);
		expect(frameShapes({ width: 1080, height: 1920, fit: 'blur' })).toBe(DELIVERY_PRESETS);
	});

	test('a fit is named for a select', () => {
		expect(DELIVERY_FITS.map(fitName)).toEqual(['Fill & crop', 'Fit & letterbox', 'Fit & blurred fill']);
	});
});

// The twins of kerf-core's `delivery_frame_rates_are_held_to_a_sane_range_and_ntsc_is_made_exact`,
// `a_delivery_with_no_rate_stores_exactly_what_it_always_did` and `the_delivery_frame_rate_is_set_checked_persisted_and_undoable`:
// the browser harness has no backend, so these are what it answers with.
describe('checkDeliveryFps', () => {
	test('keeps a rate in range as it was given', () => {
		for (const ok of [1, 12, 24, 25, 30, 50, 60, 120, 240, 33.5]) expect(checkDeliveryFps(ok)).toBe(ok);
	});

	test('refuses what is not a rate', () => {
		for (const bad of [0, 0.99, -24, 240.5, 1000, 1e9, NaN, Infinity, -Infinity]) {
			expect(() => checkDeliveryFps(bad)).toThrow('between 1 and 240 fps');
		}
	});

	test('makes the short NTSC decimals the exact n * 1000 / 1001', () => {
		const exact = (n: number) => (n * 1000) / 1001;
		const cases: [number, number][] = [
			[23.976, 24],
			[23.98, 24],
			[29.97, 30],
			[47.952, 48],
			[59.94, 60],
			[119.88, 120],
			[239.76, 240],
			[exact(30), 30]
		];
		for (const [typed, base] of cases) expect(checkDeliveryFps(typed)).toBe(exact(base));
	});

	test('does not drag a rate near a whole one onto an NTSC one', () => {
		for (const plain of [24, 29, 29.9, 30, 59.9, 60, 23.9, 25]) expect(checkDeliveryFps(plain)).toBe(plain);
	});
});

describe('the delivery frame rate in the menus', () => {
	const vertical = { width: 1080, height: 1920, fit: 'cover' as const };

	test('is Source, the whole rates and the NTSC ones, in rate order', () => {
		expect(DELIVERY_RATES.map((r) => r.label)).toEqual(['Footage’s rate', '23.976', '24', '25', '29.97', '30', '50', '59.94', '60']);
		expect(DELIVERY_RATES[0].fps).toBeNull();
		// Every one the menus offer is a rate the backend takes.
		for (const r of DELIVERY_RATES) if (r.fps != null) expect(() => checkDeliveryFps(r.fps as number)).not.toThrow();
	});

	test('ticks the project’s own rate, and Source when it has none; none without a frame', () => {
		const ticked = (f: typeof vertical & { fps?: number }) => rateChoices(f).filter((c) => c.checked).map((c) => c.label);
		expect(ticked(vertical)).toEqual(['Footage’s rate']);
		expect(ticked({ ...vertical, fps: 50 })).toEqual(['50']);
		// The exact NTSC rate the backend stores ticks the 29.97 the menu wrote.
		expect(ticked({ ...vertical, fps: checkDeliveryFps(29.97) })).toEqual(['29.97']);
		expect(rateChoices(null)).toEqual([]);
		expect(rateChoices(undefined)).toEqual([]);
	});

	test('carries a rate the list lacks, so something is always ticked', () => {
		const choices = rateChoices({ ...vertical, fps: 48 });
		expect(choices.filter((c) => c.checked)).toEqual([{ fps: 48, label: '48', checked: true }]);
		expect(choices).toHaveLength(DELIVERY_RATES.length + 1);
	});

	test('sameRate treats the typed and the exact NTSC rate as one, and none as only itself', () => {
		expect(sameRate(29.97, checkDeliveryFps(29.97))).toBe(true);
		expect(sameRate(30, 29.97)).toBe(false);
		expect(sameRate(null, undefined)).toBe(true);
		expect(sameRate(null, 30)).toBe(false);
	});

	test('fpsLabel writes a rate as people do', () => {
		expect(fpsLabel(50)).toBe('50');
		expect(fpsLabel(checkDeliveryFps(29.97))).toBe('29.97');
		expect(fpsLabel(checkDeliveryFps(23.976))).toBe('23.976');
		expect(fpsLabel(checkDeliveryFps(59.94))).toBe('59.94');
	});

	test('withRate sets or drops the rate and nothing else', () => {
		expect(withRate(vertical, 50)).toEqual({ ...vertical, fps: 50 });
		expect(withRate({ ...vertical, fps: 50 }, null)).toEqual(vertical);
		expect('fps' in withRate({ ...vertical, fps: 50 }, null)).toBe(false);
		expect(withRate({ ...vertical, fps: 50 }, 25)).toEqual({ ...vertical, fps: 25 });
	});

	test('a new shape keeps the rate the project is cut for', () => {
		const square = DELIVERY_PRESETS.find((p) => p.id === 'square')!.format!;
		expect(keepRate(square, { ...vertical, fps: 50 })).toEqual({ ...square, fps: 50 });
		expect(keepRate(square, vertical)).toEqual(square);
		expect(keepRate(square, null)).toEqual(square);
		// Source is no frame, so it carries no rate.
		expect(keepRate(null, { ...vertical, fps: 50 })).toBeNull();
	});

	test('the button and its tip say the rate when there is one', () => {
		expect(deliveryLabel(vertical)).toBe('Frame: 9:16');
		expect(deliveryLabel({ ...vertical, fps: 50 })).toBe('Frame: 9:16 · 50 fps');
		expect(deliveryLabel(null)).toBe('Frame: Footage');
		expect(deliveryTip({ ...vertical, fps: 50 })).toContain('Project frame: 1080\u00d71920 at 50 fps, fill & crop.');
		expect(deliveryTip(vertical)).toContain('Project frame: 1080\u00d71920, fill & crop.');
	});

	test('the Deliver panel’s rate select: the footage’s own first, then the rates, the project’s ticked', () => {
		const labels = (f: Parameters<typeof rateOptions>[0], fps = 30) => rateOptions(f, fps).options.map((o) => o.label);
		expect(labels(vertical)).toEqual(['Footage’s rate (30 fps)', '23.976 fps', '24 fps', '25 fps', '29.97 fps', '30 fps', '50 fps', '59.94 fps', '60 fps']);
		expect(rateOptions(vertical, 30).value).toBe('source');
		const fifty = rateOptions({ ...vertical, fps: 50 }, 50);
		expect(fifty.value).toBe('50');
		// With a rate stated, the footage’s is not known: the choice is not given a number it does not have.
		expect(fifty.options[0].label).toBe('Footage’s rate');
		expect(fifty.options.map((o) => o.fps)).toEqual([null, 23.976, 24, 25, 29.97, 30, 50, 59.94, 60]);
	});

	test('a project that follows the footage has no rate to choose, and says the footage’s', () => {
		expect(rateOptions(null, 24)).toEqual({ options: [{ value: 'source', fps: null, label: 'Footage’s rate (24 fps)' }], value: 'source' });
	});

	test('the exact NTSC rate the backend stores is the 29.97 option, and a rate the list lacks is carried', () => {
		expect(rateOptions({ ...vertical, fps: checkDeliveryFps(29.97) }, 30).value).toBe('29.97');
		const odd = rateOptions({ ...vertical, fps: 48 }, 30);
		expect(odd.value).toBe('48');
		expect(odd.options.map((o) => o.value)).toContain('48');
	});

	test('every option’s value reads back as its rate', () => {
		for (const o of rateOptions({ ...vertical, fps: 25 }, 30).options) expect(o.value === 'source' ? null : Number(o.value)).toBe(o.fps);
	});

	test('the history label is the backend’s', () => {
		expect(deliverLabel(vertical)).toBe('Deliver 1080x1920');
		expect(deliverLabel({ ...vertical, fps: 50 })).toBe('Deliver 1080x1920 at 50 fps');
		expect(deliverLabel({ ...vertical, fps: checkDeliveryFps(29.97) })).toBe('Deliver 1080x1920 at 29.97 fps');
		expect(deliverLabel(null)).toBe('Deliver at source shape');
	});
});
