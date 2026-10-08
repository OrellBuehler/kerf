/* `<svelte:window on…>` for a panel that may be in a detached window. */

import type { Attachment } from 'svelte/attachments';
import { parseEventKey, windowOf, type WindowHandlers } from './realm';
import { windows } from './windows.svelte';

/** Listen to the window the element is in — the attachment form of
 *  `<svelte:window on…>`:
 *
 *      <div {@attach onWindow({ pointermove, pointerup, pointerdowncapture })}>
 *
 *  It follows the element when dockview moves its panel into a detached window and
 *  back (it runs again when `windows.version` moves), and stops listening when the
 *  element goes. Put it on the panel's root. */
export function onWindow(handlers: WindowHandlers): Attachment<Element> {
	return (node) => {
		void windows.version;
		const win = windowOf(node);
		const stops = Object.entries(handlers).map(([key, handler]) => {
			const { type, capture } = parseEventKey(key);
			win.addEventListener(type, handler as EventListener, capture);
			return () => win.removeEventListener(type, handler as EventListener, capture);
		});
		return () => {
			for (const stop of stops) stop();
		};
	};
}
