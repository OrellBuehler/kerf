/* What the colour wheels' controls do with a pointer, a key and a typed number — pure, so the
 * component only wires events and the tests pin the arithmetic. A wheel is a puck in the unit disc
 * (`x`, `y`: red at 0°, green at 120°, blue at 240°, counter-clockwise, y up) and a lightness; the
 * three numbers `colorbalance` gets for it are `wheelRgb`'s, and `rgbToWheel` is that map run
 * backwards, so a typed R, G or B lands on exactly the wheel that would print it. */

import { normalizeWheel, wheelRgb } from './grade';
import type { RowSpec } from './slider-row';
import type { Wheel } from './types';

/** How far one arrow press moves the puck, as a share of the disc's radius: the digit the R, G and B
 *  fields show, so a press changes the number it shows by one. Shift is the slider rows' coarse step
 *  (five), Alt the fine one (a tenth). */
export const PUCK_STEP: Record<'fine' | 'normal' | 'coarse', number> = { fine: 0.001, normal: 0.01, coarse: 0.05 };

/** What the disc says about its keys, in its tooltip and, always, to a screen reader (`aria-describedby`): what Home and a
 *  double-click do (the puck, not the lightness) and what the reset button next to the name does (the whole wheel). */
export const DISC_NOTE =
	'Arrow keys move the puck: Shift five steps at a time, Alt a tenth of one. Home, Delete, Backspace or a double-click puts the puck back in the middle and leaves the lightness as it is. The reset button by the name resets the whole wheel, lightness included.';

/** What the reset button says: the other half of `DISC_NOTE`. */
export const RESET_NOTE =
	'Puts the puck in the middle and the lightness at zero. Home, Delete, Backspace or a double-click on the color wheel centres the puck only and leaves the lightness.';

/** What a key does to a wheel's puck. */
export type WheelKey = { kind: 'move'; dx: -1 | 0 | 1; dy: -1 | 0 | 1; size: keyof typeof PUCK_STEP } | { kind: 'centre' };

/** The wheel action for a key, or `null` for one that is not the wheel's: arrows move the puck (right and
 *  up are more), Shift is a coarse step and Alt a fine one, Home puts the puck back in the middle — the slider
 *  rows' keys, with Home meaning the wheel's rest — and so do Delete and Backspace (a colourist reaching for "take
 *  this off" must not take the clip off the timeline). */
export function wheelKey(key: string, mods: { shift?: boolean; alt?: boolean } = {}): WheelKey | null {
	const size = mods.alt ? 'fine' : mods.shift ? 'coarse' : 'normal';
	switch (key) {
		case 'ArrowRight':
			return { kind: 'move', dx: 1, dy: 0, size };
		case 'ArrowLeft':
			return { kind: 'move', dx: -1, dy: 0, size };
		case 'ArrowUp':
			return { kind: 'move', dx: 0, dy: 1, size };
		case 'ArrowDown':
			return { kind: 'move', dx: 0, dy: -1, size };
		case 'Home':
		case 'Delete':
		case 'Backspace':
			return { kind: 'centre' };
		default:
			return null;
	}
}

/** Keys that are the wheel's though they do nothing to it: with the wheel focused they must not reach the page, where End moves
 *  the playhead and Page Up and Page Down step through a proposal's changes. */
const SWALLOWED = new Set(['End', 'PageUp', 'PageDown']);

/** Whether a key belongs to a focused wheel, whether or not it can act on it just now: the ones `wheelKey` knows and the three it swallows. */
export const wheelOwnsKey = (key: string): boolean => wheelKey(key) !== null || SWALLOWED.has(key);

/** The wheel after a key: the puck moved (held inside the disc, so at the rim it slides along it) or put back
 *  in the middle. The lightness is the slider's and stays. */
export function nudgeWheel(w: Wheel, key: WheelKey): Wheel {
	if (key.kind === 'centre') return { ...w, x: 0, y: 0 };
	const step = PUCK_STEP[key.size];
	return normalizeWheel({ ...w, x: w.x + key.dx * step, y: w.y + key.dy * step });
}

/** The puck for a pointer at `(clientX, clientY)` over a disc whose box is `rect`, held inside the disc. */
export function puckAt(
	rect: { left: number; top: number; width: number; height: number },
	clientX: number,
	clientY: number
): { x: number; y: number } {
	if (!(rect.width > 0) || !(rect.height > 0)) return { x: 0, y: 0 };
	const w = normalizeWheel({
		x: ((clientX - rect.left) / rect.width) * 2 - 1,
		y: 1 - ((clientY - rect.top) / rect.height) * 2,
		lightness: 0
	});
	return { x: w.x, y: w.y };
}

/** Where the puck is drawn, as percentages of the disc's box from its top left. */
export const puckPercent = (w: Wheel): { left: number; top: number } => ({ left: (w.x + 1) * 50, top: (1 - w.y) * 50 });

/** The puck in polar form: a hue in degrees (`0..360`, red at 0°) and an amount (`0..1`, the radius). */
export function wheelPolar(w: Wheel): { hue: number; amount: number } {
	const n = normalizeWheel(w);
	const amount = Math.min(1, Math.hypot(n.x, n.y));
	if (amount === 0) return { hue: 0, amount: 0 };
	const deg = (Math.atan2(n.y, n.x) * 180) / Math.PI;
	return { hue: Math.round(((deg % 360) + 360) % 360) % 360, amount };
}

const HUE_NAMES = ['red', 'orange', 'yellow', 'lime', 'green', 'mint', 'cyan', 'azure', 'blue', 'violet', 'magenta', 'rose'] as const;

/** The name of the nearest of twelve hues, 30° apart, so `120` is green and `200` azure. */
export function hueName(hue: number): string {
	const sector = Math.round((((hue % 360) + 360) % 360) / 30) % 12;
	return HUE_NAMES[sector];
}

/** A wheel's puck in words, for a screen reader (and a tooltip): the hue as an angle and a name, and the amount. */
export function wheelWords(w: Wheel): string {
	const { hue, amount } = wheelPolar(w);
	const percent = Math.round(amount * 100);
	if (percent === 0) return 'Centred, no colour pushed';
	return `Hue ${hue} degrees, ${hueName(hue)}; amount ${percent} percent`;
}

/** Lightness in words: `0` is no change. */
export function lightnessWords(lightness: number): string {
	const v = Math.round(lightness * 100) / 100;
	if (v === 0) return 'No change';
	return `${v > 0 ? 'Lifted' : 'Lowered'} by ${Math.abs(Math.round(v * 100))} percent`;
}

// ---- the R, G, B fields ---------------------------------------------------------

export const WHEEL_CHANNELS = ['r', 'g', 'b'] as const;
export type WheelChannel = (typeof WHEEL_CHANNELS)[number];
export const CHANNEL_LABEL: Record<WheelChannel, string> = { r: 'R', g: 'G', b: 'B' };
export const CHANNEL_NAME: Record<WheelChannel, string> = { r: 'red', g: 'green', b: 'blue' };

/** What the R, G, B and lightness fields are: `-1..1` in hundredths, rest at zero. A typed entry is held to the
 *  same range, and to the digits shown. */
export const WHEEL_SPEC: RowSpec = { min: -1, max: 1, step: 0.01, neutral: 0, digits: 2 };

/** The wheel whose three numbers are `rgb`: the numbers sum to three times the lightness (the puck's own push sums
 *  to zero), and what is left of each is the puck's. Exact for numbers the filter could print; a triple that would
 *  need a puck outside the disc is held to the rim, as the model holds any wheel. */
export function rgbToWheel(rgb: readonly [number, number, number]): Wheel {
	const [r, g, b] = rgb;
	const lightness = (r + g + b) / 3;
	const raw = { r: r - lightness, g: g - lightness, b: b - lightness };
	const h = Math.sqrt(3) / 2;
	return normalizeWheel({ x: raw.r, y: (raw.g - raw.b) / (2 * h), lightness });
}

/** The wheel with one of its three numbers replaced: the other two stay as the filter has them. */
export function withChannelValue(w: Wheel, channel: WheelChannel, value: number): Wheel {
	const rgb = [...wheelRgb(w)] as [number, number, number];
	rgb[WHEEL_CHANNELS.indexOf(channel)] = value;
	return rgbToWheel(rgb);
}

/** The wheel's value of one field: `r`, `g`, `b` as the filter gets them. */
export const channelValue = (w: Wheel, channel: WheelChannel): number => wheelRgb(w)[WHEEL_CHANNELS.indexOf(channel)];

/** What to say when a typed R, G or B is not what came of it: no puck inside the disc makes some triples (1, 1, -1 would need a puck
 *  past the rim), and the wheel holds the whole triple to the nearest it can, so the other two numbers move as well. `applied` is the
 *  three numbers the wheel has now; `null` when the one typed is what it has, give or take a rounding. */
export function heldNote(channel: WheelChannel, typed: number, applied: readonly [number, number, number]): string | null {
	if (Math.abs(typed - applied[WHEEL_CHANNELS.indexOf(channel)]) < 0.005) return null;
	const now = WHEEL_CHANNELS.map((c, i) => `${CHANNEL_LABEL[c]} ${applied[i].toFixed(2)}`).join(' · ');
	return `${CHANNEL_LABEL[channel]} ${typed.toFixed(2)} is past the wheel's rim: held to ${now}.`;
}

/** Whether two wheels are the same once the model has rounded them. */
export function sameWheel(a: Wheel, b: Wheel): boolean {
	const [x, y] = [normalizeWheel(a), normalizeWheel(b)];
	return x.x === y.x && x.y === y.y && x.lightness === y.lightness;
}
