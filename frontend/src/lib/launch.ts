// What the app was launched with: a `.kerf` path on its command line.

import type { LaunchRequest } from './types';

/** `take_launch_project`'s answer as a request, or `null` for nothing (or
 *  anything that is not one). */
export function parseLaunchRequest(raw: unknown): LaunchRequest | null {
	if (typeof raw !== 'object' || raw === null) return null;
	const r = raw as Record<string, unknown>;
	if (typeof r.open === 'string' && r.open !== '') return { open: r.open };
	if (typeof r.missing === 'string' && r.missing !== '') return { missing: r.missing };
	return null;
}

/** What the page says when a launch named a project file that is not there. The
 *  file is never created: opening it would have made an empty project there. */
export function missingProjectMessage(path: string): string {
	return `File not found: ${path}`;
}
