/* Ghosts of a gesture grouped by the track each is drawn on. */

/** Ghosts by the track each is on. */
export function byTrack<G extends { trackId: string }>(ghosts: readonly G[]): Map<string, G[]> {
	const m = new Map<string, G[]>();
	for (const g of ghosts) {
		const list = m.get(g.trackId);
		if (list) list.push(g);
		else m.set(g.trackId, [g]);
	}
	return m;
}
