// The one string the page and the backend agree on to tell "the preview is waiting for a proxy"
// (Proxy only) from a failure. A module of its own, with no imports, because the frame pump —
// which must not treat that refusal as the GPU path breaking — is compiled alone in its test.

/** What the backend's refusal to decode an original under Proxy only starts with
 *  (`PROXY_WAIT_PREFIX` in kerf-core's `proxy.rs`). */
export const PROXY_WAIT_PREFIX = 'waiting for the preview proxy';

/** Whether an error from a preview command is that refusal — a state to show, not a failure to
 *  toast, and nothing that says the GPU path is broken. */
export function isProxyWaitMessage(e: unknown): boolean {
	const text = e instanceof Error ? e.message : typeof e === 'string' ? e : '';
	return text.startsWith(PROXY_WAIT_PREFIX);
}
