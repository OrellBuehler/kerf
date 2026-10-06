/* Forwards what goes wrong in the webview to the desktop app's logfile
 * (`log_frontend`), so a toast that vanished — or an error nobody caught — can
 * still be found afterwards. A no-op outside Tauri. */

export type LogLevel = 'error' | 'warn' | 'info';

const DEDUPE_MS = 1000;
let last = { key: '', at: 0 };

export function describeError(e: unknown): string {
	if (e instanceof Error) return e.stack && !e.message ? e.stack : e.message;
	if (typeof e === 'string') return e;
	try {
		return JSON.stringify(e) ?? String(e);
	} catch {
		return String(e);
	}
}

export function logFrontend(level: LogLevel, message: string, context?: string) {
	if (typeof window === 'undefined' || !('__TAURI_INTERNALS__' in window)) return;
	const key = `${level}\0${message}`;
	const now = Date.now();
	if (key === last.key && now - last.at < DEDUPE_MS) return;
	last = { key, at: now };
	void import('@tauri-apps/api/core')
		.then(({ invoke }) => invoke('log_frontend', { level, message, context }))
		.catch(() => {});
}

let installed = false;

export function installErrorLogging() {
	if (installed || typeof window === 'undefined') return;
	installed = true;
	window.addEventListener('error', (ev) => {
		const where = ev.filename ? `${ev.filename}:${ev.lineno}:${ev.colno}` : undefined;
		logFrontend('error', describeError(ev.error ?? ev.message), `window.onerror ${where ?? ''}`.trim());
	});
	window.addEventListener('unhandledrejection', (ev) => {
		logFrontend('error', describeError(ev.reason), 'unhandledrejection');
	});
}
