/* A clip's name on the timeline — pure. The name is pinned by the browser (`position: sticky`
 * in `Timeline.svelte`, so a clip that starts off-screen keeps its name at the left edge and a
 * scroll costs no script); what is decided here is when there is room for one at all and how
 * strong the chip behind it is. */

/** A clip narrower than this shows no name — a fragment ("t", "m") reads as noise — and says it in a tooltip. */
export const MIN_LABEL_CLIP_PX = 40;

/** Whether a clip `width` px wide has room for its name. */
export const showsLabel = (width: number): boolean => Number.isFinite(width) && width >= MIN_LABEL_CLIP_PX;

/** How much of `--scrim` backs a name that sits on a waveform or a thumbnail strip, %: enough that
 *  white text holds 4.5:1 on the brightest wave of every theme (`theme-guard.test.ts`). */
export const NAME_CHIP_PCT = 55;
