// The width and height typed into the export dialog's "Custom…" size. The backend takes `Option<(u32, u32)>`, so a
// negative or fractional entry is a serde error rather than a size, and the delivery frame (`Delivery::new` in
// kerf-core) is `max(2)` and rounded down to even — 4:2:0 video has no odd side. An entry is brought to that here and
// the field says what it did, instead of exporting at 1920 because `parseInt` of "" is NaN.

export const MIN_SIDE = 2;
export const MAX_SIDE = 8192;

export interface SideEdit {
	/** The side to use: always a whole, even number in `MIN_SIDE..=MAX_SIDE`. */
	value: number;
	/** What was changed or refused, for the line under the fields; null when the entry stood as typed. */
	note: string | null;
}

/** What a typed side comes to. An entry that is no number keeps `current`. Fractions are cut, an odd side goes down to
 *  the even number below it (as the core does), and the range is clamped. */
export function editSide(raw: string, current: number): SideEdit {
	const text = raw.trim();
	const n = text === '' ? NaN : Number(text);
	if (!Number.isFinite(n)) return { value: current, note: 'Enter a size in pixels.' };
	const whole = Math.trunc(n);
	if (whole < MIN_SIDE) return { value: MIN_SIDE, note: `The smallest side is ${MIN_SIDE} px.` };
	if (whole > MAX_SIDE) return { value: MAX_SIDE, note: `The largest side is ${MAX_SIDE} px.` };
	if (whole % 2 === 1) return { value: whole - 1, note: `Sides are even: ${whole} became ${whole - 1}.` };
	return { value: whole, note: whole === n ? null : `Sides are whole pixels: ${text} became ${whole}.` };
}
