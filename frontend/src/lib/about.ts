// What the About dialog says, as data so the "Copy version info" text and the lines on screen are
// one thing: a person reporting an issue pastes exactly what they were looking at.

export const LICENSE = 'PolyForm Noncommercial 1.0.0';
export const REPOSITORY = 'github.com/OrellBuehler/kerf';
export const TAGLINE = 'Non-destructive, AI-assisted video editing.';

export interface AboutInfo {
	/** The running build's version (`dev` in the browser harness), or empty while it is being read. */
	version: string;
	/** `platformName()`. */
	platform: string;
	/** A development build (the browser harness, `tauri dev`) rather than an installed release. */
	development: boolean;
}

export interface AboutRow {
	label: string;
	value: string;
}

/** The operating system, read from the webview's own report: macOS, Windows, Linux (Android's webview
 *  says "Linux" too, and is not one), else `unknown`. */
export function platformName(nav: { platform?: string; userAgent?: string } | undefined): string {
	const text = `${nav?.platform ?? ''} ${nav?.userAgent ?? ''}`;
	if (/mac|iphone|ipad|ipod/i.test(text)) return 'macOS';
	if (/win/i.test(text) && !/darwin/i.test(text)) return 'Windows';
	if (/linux/i.test(text) && !/android/i.test(text)) return 'Linux';
	return 'unknown';
}

/** The rows under the name: version, what it is running on, which build, the licence. */
export function aboutRows(info: AboutInfo): AboutRow[] {
	return [
		{ label: 'Version', value: info.version || 'unknown' },
		{ label: 'Platform', value: info.platform },
		{ label: 'Build', value: info.development ? 'development' : 'release' },
		{ label: 'License', value: LICENSE }
	];
}

/** The text "Copy version info" puts on the clipboard. */
export function versionInfo(info: AboutInfo): string {
	return ['Kerf', ...aboutRows(info).map((r) => `${r.label}: ${r.value}`), REPOSITORY].join('\n');
}
