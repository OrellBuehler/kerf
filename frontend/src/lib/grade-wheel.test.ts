import { describe, expect, test } from 'bun:test';
import { normalizeWheel, wheelRgb } from './grade';
import {
	DISC_NOTE,
	RESET_NOTE,
	WHEEL_SPEC,
	channelValue,
	heldNote,
	hueName,
	lightnessWords,
	nudgeWheel,
	puckAt,
	puckPercent,
	rgbToWheel,
	sameWheel,
	wheelKey,
	wheelOwnsKey,
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

	test('Home, Delete and Backspace are the wheel’s rest, and a key that is not the wheel’s is nobody’s', () => {
		for (const k of ['Home', 'Delete', 'Backspace']) expect(wheelKey(k), k).toEqual({ kind: 'centre' });
		for (const k of ['a', 'Tab', 'Escape', ' ', 'PageUp', 'End', 'Enter']) expect(wheelKey(k), k).toBeNull();
	});

	test('End and the Page keys belong to a focused wheel too, though they do nothing: they must not move the playhead', () => {
		for (const k of ['End', 'PageUp', 'PageDown', 'Home', 'Delete', 'Backspace', 'ArrowLeft']) expect(wheelOwnsKey(k), k).toBe(true);
		for (const k of [' ', 'j', 'k', 'l', 's', 'Tab', 'F2', 'Enter', 'Escape']) expect(wheelOwnsKey(k), k).toBe(false);
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

describe('what the disc and its reset button say about Home', () => {
	test('Home and a double-click centre the puck and leave the lightness; the reset button resets the whole wheel', () => {
		expect(DISC_NOTE).toContain('Home, Delete, Backspace or a double-click puts the puck back in the middle and leaves the lightness as it is');
		expect(DISC_NOTE).toContain('resets the whole wheel, lightness included');
		expect(RESET_NOTE).toContain('puck in the middle and the lightness at zero');
		expect(RESET_NOTE).toContain('a double-click on the color wheel centres the puck only and leaves the lightness');
	});

	test('what the note says is what Home does', () => {
		expect(nudgeWheel({ x: 0.4, y: -0.3, lightness: 0.25 }, wheelKey('Home')!)).toEqual({ x: 0, y: 0, lightness: 0.25 });
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

	test('a triple that needs a puck outside the disc is held to the rim: what comes back is a wheel on the rim, with other numbers', () => {
		// 1, 1, -1 sum to 1, so the lightness is 1/3 and what is left, (2/3, 2/3, -4/3), is a puck at x 2/3, y 1.155: past the rim
		const w = rgbToWheel([1, 1, -1]);
		expect(Math.hypot(w.x, w.y)).toBeCloseTo(1, 3);
		expect(w.lightness).toBeCloseTo(1 / 3, 4);
		const got = wheelRgb(w);
		expect(got[0]).toBeCloseTo(0.8333, 3);
		expect(got[1]).toBeCloseTo(0.8333, 3);
		expect(got[2]).toBeCloseTo(-0.6667, 3);
		// the number typed and the number applied are up to a third apart
		expect(Math.abs(got[2] - -1)).toBeGreaterThan(0.33);
		expect(normalizeWheel(w)).toEqual(w);
		// typing one box on a wheel whose other two are at their limit is held the same way
		const typed = withChannelValue({ x: 0.7, y: 0.5, lightness: 0 }, 'r', 1);
		expect(channelValue(typed, 'r')).toBeLessThan(1);
	});

	test('a triple a puck inside the disc makes is taken as typed', () => {
		const w = withChannelValue(centred, 'r', 1);
		// 1, 0, 0 is lightness 1/3 and a puck at x 2/3: inside the disc
		expect(Math.hypot(w.x, w.y)).toBeLessThan(1);
		expect(channelValue(w, 'r')).toBe(1);
	});

	test('what is said when the number applied is not the number typed, and nothing when it is', () => {
		expect(heldNote('b', -1, [0.83, 0.83, -0.67])).toBe("B -1.00 is past the wheel's rim: held to R 0.83 · G 0.83 · B -0.67.");
		expect(heldNote('r', 1, [0.83, 0.83, -0.67])).toBe("R 1.00 is past the wheel's rim: held to R 0.83 · G 0.83 · B -0.67.");
		expect(heldNote('g', 0.25, [0.1, 0.25, 0])).toBeNull();
		expect(heldNote('g', 0.254, [0.1, 0.25, 0])).toBeNull();
		// the three typed into a real wheel, through the same functions the component calls: only the last is past the rim
		let w = withChannelValue(centred, 'r', 1);
		expect(heldNote('r', 1, wheelRgb(w))).toBeNull();
		w = withChannelValue(w, 'g', 1);
		expect(heldNote('g', 1, wheelRgb(w))).toBeNull();
		w = withChannelValue(w, 'b', -1);
		const note = heldNote('b', -1, wheelRgb(w));
		expect(note).toContain('B -1.00 is past the wheel');
		expect(note).toContain('R 0.83 · G 0.83 · B -0.67');
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
