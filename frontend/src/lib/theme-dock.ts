// Where the theme editor sits. It is a strip along the right edge of the window, and the app is made narrower by its width while it is
// open (the shell's `right` offset, `--shell-right`), so the Inspector, the Agent panel and the title bar's right-hand buttons —
// what is worth watching while a theme is edited — stay on screen beside it. In a window too narrow to spare the strip it is an
// overlay over the page instead, as it was before it could dock.

/** The editor's width, px. */
export const EDITOR_WIDTH = 480;

/** Below this window width the page is not made narrower: what is left would be too small to be worth watching (800 px here, a
 *  little under the 960 the window itself is held to; the panels keep their minimums and the status bar clips, not wraps). */
export const DOCK_MIN_VIEWPORT = 1280;

/** How much of the window the editor takes from the app: its width when the window can spare it, else nothing (it overlays). */
export function dockWidth(viewport: number): number {
	return Number.isFinite(viewport) && viewport >= DOCK_MIN_VIEWPORT ? EDITOR_WIDTH : 0;
}
