// A color name as the swatch beside a field shows it. The page has no table of the 148 names: the browser
// already knows them, so it is asked through a canvas, which answers `#rrggbb` for an opaque color and keeps its
// old value when it is handed a name it does not know. Only the picture beside a field uses this; what is written
// into a title is the name the person typed.

interface Paint {
	fillStyle: string | CanvasGradient | CanvasPattern;
}

let paint: Paint | null | undefined;
const answers = new Map<string, string | null>();

function canvasPaint(): Paint | null {
	if (paint === undefined) paint = typeof document === 'undefined' ? null : document.createElement('canvas').getContext('2d');
	return paint;
}

/** `#rrggbb` for a CSS color name, or `null` for one the browser does not know (or that is not opaque, like `transparent`).
 *  `ctx` stands in for the canvas in a test. */
export function cssColorHex(name: string, ctx: Paint | null = canvasPaint()): string | null {
	const key = name.trim().toLowerCase();
	if (!ctx || !/^[a-z]+$/.test(key)) return null;
	if (ctx === paint && answers.has(key)) return answers.get(key) ?? null;
	// An assignment the canvas cannot read is ignored: the same name set over two different colors only ends
	// up the same if it was read.
	ctx.fillStyle = 'black';
	ctx.fillStyle = key;
	const over = String(ctx.fillStyle);
	ctx.fillStyle = 'white';
	ctx.fillStyle = key;
	const hex = over === String(ctx.fillStyle) && /^#[0-9a-f]{6}$/.test(over) ? over : null;
	if (ctx === paint) answers.set(key, hex);
	return hex;
}
