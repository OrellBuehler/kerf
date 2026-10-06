// Running an edit from a click: the backend can refuse (a stale id, a value out
// of range, no clip under the playhead), and a button has nowhere to put that
// but a notice.

import { toast } from './notifications.svelte';

export function errorMessage(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

/** Await `op`, turning a rejection into an error toast. Never throws. */
export async function attempt(op: () => Promise<unknown>): Promise<void> {
	try {
		await op();
	} catch (e) {
		toast.error(errorMessage(e));
	}
}
