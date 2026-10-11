// The master strip's FX block — the limiter, its ceiling and the duck mode. They sat above the fader and
// left it the shortest in the mixer (about 70 px of travel against 190 px for a track's), so they are a
// disclosure now, folded by default, and one line says what is switched on while it is folded.

import { ceilingLabel } from './levels';
import type { MasterBus } from './types';

export interface FxSummary {
	/** What to say beside "FX": `Limiter −1.5 dBFS`, `Limiter · Gate`, `Off`. */
	text: string;
	/** Whether anything on the master is doing something, so a folded block does not hide a limiter that is on. */
	active: boolean;
}

/** One line for the folded block. The compressor is the duck mode every project starts in, so only the speech gate is news. */
export function fxSummary(master: Pick<MasterBus, 'limiter' | 'ceiling_db' | 'duck_depth_db'>): FxSummary {
	const gate = master.duck_depth_db != null;
	if (master.limiter && gate) return { text: 'Limiter · Gate', active: true };
	if (master.limiter) return { text: `Limiter ${ceilingLabel(master.ceiling_db)}`, active: true };
	if (gate) return { text: 'Speech gate', active: true };
	return { text: 'Off', active: false };
}

/** Whether the block is open: a viewer's choice, remembered (not part of the project). Folded by default. */
export const MASTER_FX_KEY = 'kerf.mixer.masterFx';

export function loadMasterFxOpen(): boolean {
	try {
		return localStorage.getItem(MASTER_FX_KEY) === '1';
	} catch {
		return false;
	}
}

export function saveMasterFxOpen(open: boolean): void {
	try {
		localStorage.setItem(MASTER_FX_KEY, open ? '1' : '0');
	} catch {
		/* a convenience, not state — ignore a blocked store */
	}
}
