// `Number.prototype.toFixed` with Rust's rounding — for the TS mirrors of strings
// kerf-core formats with `{:.N}` (`fmt_time`, `fmt_delta`, the refusals' "0.12s").
//
// Both round the *exact* binary value, so they agree on everything but a **tie**: a
// value exactly halfway between two outputs, which only a dyadic fraction can be
// (4.25 at one decimal, 0.125 at two). `toFixed` takes the larger neighbour there
// (4.25 → "4.3"); Rust takes the even one (4.25 → "4.2"). A mirror that prints the
// other digit shows the user a different number from the one the backend reports.

/** `x.toFixed(digits)` rounding an exact tie to the even digit, as Rust's `{:.N}` does. `digits` ≥ 1. */
export function toFixedEven(x: number, digits: number): string {
	// Rust prints a negative zero with its sign; `toFixed` drops it.
	const rounded = Object.is(x, -0) ? `-${x.toFixed(digits)}` : x.toFixed(digits);
	if (!Number.isFinite(x) || digits < 1 || digits > 20) return rounded;
	// A double ≥ 2^-48 has at most 100 fractional bits, so 100 decimals is its exact
	// expansion; a tie is a 5 followed by nothing but zeros after the last kept digit.
	const exact = Math.abs(x).toFixed(100);
	const dot = exact.indexOf('.');
	if (!/^50*$/.test(exact.slice(dot + 1 + digits))) return rounded;
	const kept = exact.slice(0, dot + 1 + digits);
	// `toFixed` went up, so it already ends in an even digit when the truncation ends in an odd one.
	if (Number(kept.slice(-1)) % 2 !== 0) return rounded;
	return `${x < 0 ? '-' : ''}${kept}`;
}
