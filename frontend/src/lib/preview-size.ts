/* How wide a frame the Preview asks the backend for — pure. The picture is drawn into a box of
 * some CSS width on a screen of some pixel ratio, so the frame it wants is `ceil(css × dpr)`
 * wide: fewer pixels is a soft picture on a sharp screen (a 2× display at 960 is upscaled), more is
 * decode and JPEG time for detail nobody sees. The request is rounded up to one of a few steps, so
 * dragging a panel's edge changes it a handful of times rather than on every pixel (each change is
 * an FFmpeg composite), and capped where the MCP's frame tools cap theirs (1920): the backend takes
 * any width and never scales a picture up past its own, so the cap is only ever a bound on cost. */

/** The widths a frame is asked at, narrowest first. */
export const FRAME_STEPS: readonly number[] = [480, 960, 1440, 1920];

/** The widest frame asked for, px. */
export const MAX_FRAME_PX = FRAME_STEPS[FRAME_STEPS.length - 1];

/** What a request was before the box was ever measured, and what an unusable measurement is: the
 *  size the pane always asked for. */
export const DEFAULT_FRAME_PX = 960;

/** How long the box must hold still before a different width is asked for, ms. */
export const RESIZE_SETTLE_MS = 200;

/** The step a box `cssWidth` px wide, on a screen of pixel ratio `dpr`, is asked at. */
export function requestWidth(cssWidth: number, dpr: number): number {
	if (!Number.isFinite(cssWidth) || cssWidth <= 0) return DEFAULT_FRAME_PX;
	const ratio = Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
	const wanted = Math.ceil(cssWidth * ratio - 1e-6);
	return FRAME_STEPS.find((s) => s >= wanted) ?? MAX_FRAME_PX;
}
