// Putting text on the clipboard with the notice every copy in the app gives ("Reason copied").

import { toast } from './notifications.svelte';

/** Copy `text`; `what` names it in the notice. */
export function copyText(text: string, what: string): void {
	void navigator.clipboard
		.writeText(text)
		.then(() => toast.success(`${what} copied`))
		.catch(() => toast.error(`Couldn't copy the ${what.toLowerCase()}`));
}
