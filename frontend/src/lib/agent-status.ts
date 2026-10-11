// What the shell says about the agent: the one status line at the top of the Agent panel, the count a
// pending proposal puts on the Agent tab and in the title bar, and how much of the composer the panel can
// afford. Pure, so the words and the thresholds are tested and the three places cannot disagree.

import type { StagedEdit } from './types';

/** `12s ago`, `4m ago`, `2h ago`: how long since an agent last spoke to the endpoint. */
export function ago(secs: number): string {
	if (secs < 60) return `${Math.max(1, Math.round(secs))}s ago`;
	if (secs < 3600) return `${Math.round(secs / 60)}m ago`;
	return `${Math.round(secs / 3600)}h ago`;
}

export interface AgentLine {
	text: string;
	/** `agent` while one is there, `muted` while none is, `danger` when the endpoint itself is down. */
	tone: 'agent' | 'muted' | 'danger';
	/** Whether something is running now (the line shows a spinner). */
	working: boolean;
}

/**
 * The panel's one status line. It used to be two — an `idle` badge in a header and "No agent connected"
 * below it — that could not both be right.
 */
export function agentLine(o: { serverError: string | null; connected: boolean; lastSeen: number | null; working: boolean }): AgentLine {
	if (o.serverError) return { text: o.serverError, tone: 'danger', working: false };
	if (o.working) return { text: 'Agent working', tone: 'agent', working: true };
	if (o.connected) return { text: 'Agent connected', tone: 'agent', working: false };
	if (o.lastSeen === null) return { text: 'No agent connected', tone: 'muted', working: false };
	return { text: `Last seen ${ago(o.lastSeen)}`, tone: 'muted', working: false };
}

export interface ProposalIndicator {
	/** How many changes the proposal holds. */
	count: number;
	/** The title bar chip: `Review 4 changes`. */
	label: string;
	/** The tab's accessible name and the chip's tooltip. */
	tip: string;
	/** The cut moved on since the agent staged it: applying replaces the newer cut. */
	stale: boolean;
}

const changes = (n: number) => `${n} ${n === 1 ? 'change' : 'changes'}`;

/**
 * What a pending agent proposal looks like away from the Agent panel, or `null` when there is none. In
 * the Audio and Deliver workspaces the agent is only a tab, and a proposal that landed there was
 * invisible: this is the dot on that tab and the chip in the title bar.
 */
export function proposalIndicator(staged: Pick<StagedEdit, 'diff' | 'stale'> | null | undefined): ProposalIndicator | null {
	if (!staged) return null;
	const count = staged.diff.entries.length;
	return {
		count,
		label: count === 0 ? 'Review proposal' : `Review ${changes(count)}`,
		tip:
			count === 0
				? 'The agent staged a proposal — open the Agent panel to apply or discard it'
				: `The agent proposed ${changes(count)} — open the Agent panel to apply or discard them`,
		stale: staged.stale
	};
}

/** A panel shorter than this has no room for the composer's hint line: the review card's buttons and the
 *  list beneath them matter more (at 1366×768 the Agent panel is about 420 px tall). */
export const COMPOSER_HINT_MIN_HEIGHT = 520;

/** Whether the composer shows its one-line hint under the field; below that it is the field's description. */
export function showComposerHint(panelHeight: number): boolean {
	return panelHeight >= COMPOSER_HINT_MIN_HEIGHT;
}

/** The title bar's right cell, px, below which the review chip is its icon and count: with the project's
 *  name and badge, Export, the gear, the bell and the version beside it, the full label leaves the name
 *  nothing (at 960 px the badge itself went). */
export const REVIEW_CHIP_FULL_WIDTH = 510;

/** What the title bar's chip says in a cell `cellWidth` wide (0 while it is not measured yet: the full label). */
export function reviewChipText(indicator: ProposalIndicator, cellWidth: number): string {
	const narrow = cellWidth > 0 && cellWidth < REVIEW_CHIP_FULL_WIDTH;
	return narrow ? String(indicator.count || 'Review') : indicator.label;
}
