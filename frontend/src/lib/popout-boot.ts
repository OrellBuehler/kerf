/* A detached panel's window is opened by the editor window and filled from it, so
   it runs none of the app's own script. If one does start — a stale popout URL the
   static fallback answered with `index.html` — it must not boot a second editor
   beside the first: two `editor` singletons, two settings writers, two reveal
   requests. The editor window itself is never opened by another window. */

export function isEmbeddedPopout(win: { opener?: unknown } & object = window): boolean {
	return win.opener != null && win.opener !== win;
}
