import { describe, expect, test } from 'bun:test';
import { normalizeWheel, wheelRgb } from './grade';
import {
	WHEEL_SPEC,
	channelValue,
	hueName,
	lightnessWords,
	nudgeWheel,
	puckAt,
	puckPercent,
	rgbToWheel,
	sameWheel,
	wheelKey,
	wheelPolar,
	wheelWords,
	withChannelValue
} from './grade-wheel';
import { parseEntry, readout } from './slider-row';

const centred = { x: 0, y: 0, lightness: 0 };

describe('the wheel’s keys', () => {
	test('arrows move the puck, right and up are more', () => {
		expect(wheelKey('ArrowRight')).toEqual({ kind: 'move', dx: 1, dy: 0, size: 'normal' });
		expect(wheelKey('ArrowLeft')).toEqual({ kind: 'move', dx: -1, dy: 0, size: 'normal' });
		expect(wheelKey('ArrowUp')).toEqual({ kind: 'move', dx: 0, dy: 1, size: 'normal' });
		expect(wheelKey('ArrowDown')).toEqual({ kind: 'move', dx: 0, dy: -1, size: 'normal' });
	});

	test('Shift is the slider rows’ coarse step and Alt the fine one, Alt first', () => {
		expect(wheelKey('ArrowUp', { shift: true })).toMatchObject({ size: 'coarse' });
		expect(wheelKey('ArrowUp', { alt: true })).toMatchObject({ size: 'fine' });
		expect(wheelKey('ArrowUp', { alt: true, shift: true })).toMatchObject({ size: 'fine' });
	});

	test('Home is the wheel’s rest, and a key that is not the wheel’s is nobody’s', () => {
		expect(wheelKey('Home')).toEqual({ kind: 'centre' });
		for (const k of ['a', 'Tab', 'Escape', 'Delete', ' ', 'PageUp']) expect(wheelKey(k)).toBeNull();
	});

	test('a step moves the puck by a hundredth, five with Shift and a thousandth with Alt', () => {
		const right = (size: 'fine' | 'normal' | 'coarse') => nudgeWheel(centred, { kind: 'move', dx: 1, dy: 0, size });
		expect(right('normal')).toEqual({ x: 0.01, y: 0, lightness: 0 });
		expect(right('coarse')).toEqual({ x: 0.05, y: 0, lightness: 0 });
		expect(right('fine')).toEqual({ x: 0.001, y: 0, lightness: 0 });
	});

	test('a run of steps adds up without float noise', () => {
		let w = centred;
		for (let i = 0; i < 30; i++) w = nudgeWheel(w, { kind: 'move', dx: 0, dy: 1, size: 'normal' });
		expect(w.y).toBe(0.3);
	});

	test('the puck stays in the disc: at the rim it slides along it', () => {
		// (the model rounds x and y to a ten-thousandth after scaling them back, so the radius may overshoot by that)
		const rim = nudgeWheel({ x: 1, y: 0, lightness: 0.2 }, { kind: 'move', dx: 1, dy: 0, size: 'coarse' });
		expect(Math.hypot(rim.x, rim.y)).toBeLessThanOrEqual(1.0001);
		expect(rim.x).toBe(1);
		const up = nudgeWheel({ x: 1, y: 0, lightness: 0 }, { kind: 'move', dx: 0, dy: 1, size: 'coarse' });
		expect(Math.hypot(up.x, up.y)).toBeLessThanOrEqual(1.0001);
		expect(up.y).toBeGreaterThan(0);
	});

	test('Home puts the puck back and keeps the lightness', () => {
		expect(nudgeWheel({ x: 0.4, y: -0.3, lightness: 0.25 }, { kind: 'centre' })).toEqual({ x: 0, y: 0, lightness: 0.25 });
	});
});

describe('the pointer on the disc', () => {
	const box = { left: 100, top: 50, width: 80, height: 80 };
	test('the middle is the centre and the rim is a unit away; up is more', () => {
		expect(puckAt(box, 140, 90)).toEqual({ x: 0, y: 0 });
		expect(puckAt(box, 180, 90)).toEqual({ x: 1, y: 0 });
		expect(puckAt(box, 140, 50)).toEqual({ x: 0, y: 1 });
	});

	test('outside the disc it is held to the rim, along the direction of the pointer', () => {
		const p = puckAt(box, 300, 90);
		expect(p).toEqual({ x: 1, y: 0 });
		const corner = puckAt(box, 180, 50);
		expect(Math.hypot(corner.x, corner.y)).toBeLessThanOrEqual(1.0001);
		expect(corner.x).toBeCloseTo(0.7071, 4);
		expect(corner.y).toBeCloseTo(0.7071, 4);
	});

	test('a box with no size gives the centre, not NaN', () => {
		expect(puckAt({ left: 0, top: 0, width: 0, height: 0 }, 5, 5)).toEqual({ x: 0, y: 0 });
	});

	test('the puck is drawn where the pointer put it', () => {
		expect(puckPercent(centred)).toEqual({ left: 50, top: 50 });
		expect(puckPercent({ x: 1, y: 0, lightness: 0 })).toEqual({ left: 100, top: 50 });
		expect(puckPercent({ x: 0, y: 1, lightness: 0 })).toEqual({ left: 50, top: 0 });
	});
});

describe('a wheel in words', () => {
	test('the engine’s hues: red at 0°, green at 120°, blue at 240°, counter-clockwise', () => {
		expect(wheelPolar({ x: 1, y: 0, lightness: 0 })).toEqual({ hue: 0, amount: 1 });
		expect(wheelPolar({ x: -0.5, y: Math.sqrt(3) / 2, lightness: 0 })).toMatchObject({ hue: 120 });
		expect(wheelPolar({ x: -0.5, y: -Math.sqrt(3) / 2, lightness: 0 })).toMatchObject({ hue: 240 });
		expect(wheelPolar({ x: 0, y: 1, lightness: 0 })).toMatchObject({ hue: 90 });
		expect(wheelPolar(centred)).toEqual({ hue: 0, amount: 0 });
	});

	test('the hue the words name is the one the filter pushes', () => {
		// the filter's red, green and blue for the puck at a hue: the named one is the largest
		for (const [hue, name, index] of [
			[0, 'red', 0],
			[120, 'green', 1],
			[240, 'blue', 2]
		] as const) {
			const r = (hue * Math.PI) / 180;
			const w = { x: Math.cos(r), y: Math.sin(r), lightness: 0 };
			expect(hueName(wheelPolar(w).hue)).toBe(name);
			const rgb = wheelRgb(w);
			expect(rgb[index]).toBe(Math.max(...rgb));
		}
	});

	test('the twelve names, round the wheel and past 360', () => {
		const names = [0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330].map(hueName);
		expect(names).toEqual(['red', 'orange', 'yellow', 'lime', 'green', 'mint', 'cyan', 'azure', 'blue', 'violet', 'magenta', 'rose']);
		expect(hueName(359)).toBe('red');
		expect(hueName(375)).toBe('orange');
		expect(hueName(-30)).toBe('rose');
	});

	test('the sentence says hue and amount, and a centred puck says it pushes nothing', () => {
		expect(wheelWords({ x: -0.2, y: 0.346, lightness: 0 })).toBe('Hue 120 degrees, green; amount 40 percent');
		expect(wheelWords(centred)).toBe('Centred, no colour pushed');
		expect(wheelWords({ x: 0.001, y: 0, lightness: 0 })).toBe('Centred, no colour pushed');
		expect(wheelWords({ x: 0, y: 0, lightness: 0.5 })).toBe('Centred, no colour pushed');
	});

	test('lightness in words', () => {
		expect(lightnessWords(0)).toBe('No change');
		expect(lightnessWords(0.25)).toBe('Lifted by 25 percent');
		expect(lightnessWords(-0.1)).toBe('Lowered by 10 percent');
	});
});

describe('the R, G and B fields', () => {
	test('a wheel’s numbers read back to the wheel that printed them', () => {
		const wheels = [
			{ x: 0.3, y: -0.2, lightness: 0.1 },
			{ x: -0.5, y: 0.5, lightness: -0.25 },
			{ x: 0, y: 0, lightness: 0.4 },
			{ x: 0.6, y: 0.7, lightness: 0 },
			{ x: -0.1, y: -0.05, lightness: -0.05 }
		];
		for (const w of wheels) {
			const back = rgbToWheel(wheelRgb(w));
			expect(Math.abs(back.x - w.x)).toBeLessThan(2e-4);
			expect(Math.abs(back.y - w.y)).toBeLessThan(2e-4);
			expect(Math.abs(back.lightness - w.lightness)).toBeLessThan(2e-4);
			// what the filter would get is the same either way
			expect(wheelRgb(back)).toEqual(wheelRgb(w));
		}
	});

	test('typing one number changes that one and leaves the other two as they were', () => {
		const w = { x: 0.2, y: 0.1, lightness: 0.05 };
		const before = wheelRgb(w);
		for (const [channel, index] of [
			['r', 0],
			['g', 1],
			['b', 2]
		] as const) {
			const after = wheelRgb(withChannelValue(w, channel, 0.3));
			expect(after[index]).toBe(0.3);
			for (let i = 0; i < 3; i++) if (i !== index) expect(Math.abs(after[i] - before[i])).toBeLessThan(2e-4);
		}
	});

	test('red alone is a pushed puck and a lifted lightness, as the filter would print it', () => {
		const w = withChannelValue(centred, 'r', 0.3);
		expect(channelValue(w, 'r')).toBe(0.3);
		expect(channelValue(w, 'g')).toBe(0);
		expect(channelValue(w, 'b')).toBe(0);
		expect(w.lightness).toBeCloseTo(0.1, 4);
	});

	test('all three the same is lightness alone, and zeros are the neutral wheel', () => {
		expect(rgbToWheel([0.2, 0.2, 0.2])).toEqual({ x: 0, y: 0, lightness: 0.2 });
		expect(rgbToWheel([0, 0, 0])).toEqual(centred);
		expect(Object.is(rgbToWheel([0, 0, 0]).x, 0)).toBe(true);
	});

	test('a triple that needs a puck outside the disc is held to the rim, and what the model kept is a wheel', () => {
		const w = withChannelValue(centred, 'r', 1);
		expect(Math.hypot(w.x, w.y)).toBeLessThanOrEqual(1.0001);
		expect(normalizeWheel(w)).toEqual(w);
	});

	test('the fields are hundredths from -1 to 1 and take what is typed to the digit shown', () => {
		expect(readout(0.1234, WHEEL_SPEC)).toBe('0.12');
		expect(readout(-0.5, WHEEL_SPEC)).toBe('-0.50');
		expect(parseEntry('0.256', WHEEL_SPEC)).toBe(0.26);
		expect(parseEntry('-3', WHEEL_SPEC)).toBe(-1);
		expect(parseEntry('red', WHEEL_SPEC)).toBeNull();
	});

	test('two wheels are the same once the model has rounded them', () => {
		expect(sameWheel({ x: 0.10001, y: 0, lightness: 0 }, { x: 0.1, y: 0, lightness: 0 })).toBe(true);
		expect(sameWheel({ x: 0.1, y: 0, lightness: 0 }, { x: 0.11, y: 0, lightness: 0 })).toBe(false);
		expect(sameWheel({ x: 3, y: 4, lightness: 0 }, { x: 0.6, y: 0.8, lightness: 0 })).toBe(true);
	});
});
