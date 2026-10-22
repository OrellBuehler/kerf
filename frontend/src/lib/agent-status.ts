// What the shell says about the agent: the one status line at the top of the Agent panel, the count a
// pending proposal puts on the Agent tab and in the title bar, and how much of the composer the panel can
// afford. Pure, so the words and the thresholds are tested and the three places cannot disagree.

import { badgeTip } from './proposal-ui';
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

/** The Agent dock tab's accessible name: its visible label, then what the count on it means. */
export function agentTabLabel(indicator: ProposalIndicator | null): string {
	if (!indicator) return 'Agent';
	return indicator.count === 0 ? 'Agent, proposal to review' : `Agent, ${changes(indicator.count)} to review`;
}

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
		tip: count === 0 ? 'The agent staged a proposal — open the Agent panel to review it' : badgeTip(count),
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

/** The title bar's right cell, px, from which the review chip carries its verb (`Review 4 changes`): with the
 *  project's name and badge, Export, the gear, the bell and the version beside it, the full label leaves the name
 *  almost nothing. */
export const REVIEW_CHIP_FULL_WIDTH = 560;

/** Below `REVIEW_CHIP_FULL_WIDTH` and from here up, the chip says what the number counts (`4 changes`) without the
 *  verb: a bare `4` beside a git icon read as a build number, not as the agent's work waiting for you. The project's
 *  name is what gives way for it (it shrinks to an ellipsis), so it is the chip of every window the editor is
 *  laid out for: 1280×720 has a right cell of about 470, 1366×768 513, 1440×900 about 552. Only a window narrower than
 *  that (the 960 px minimum has ~300) keeps the bare count, with the whole sentence in its tooltip and accessible
 *  name. */
export const REVIEW_CHIP_COUNT_WIDTH = 440;

/** What the title bar's chip says in a cell `cellWidth` wide (0 while it is not measured yet: the full label): the
 *  whole label, then the count with its noun, then — when even that would crowd the project's name — the count
 *  alone, which the chip's tooltip and accessible name still spell out. */
export function reviewChipText(indicator: ProposalIndicator, cellWidth: number): string {
	if (cellWidth <= 0 || cellWidth >= REVIEW_CHIP_FULL_WIDTH) return indicator.label;
	if (cellWidth >= REVIEW_CHIP_COUNT_WIDTH) return indicator.count === 0 ? 'Review' : changes(indicator.count);
	return String(indicator.count || 'Review');
}

/** The strip under the Agent panel's status line while review is off. With a proposal still open the agent's edits keep
 *  going into it (its reads follow it, so they cannot split between it and the cut) and nothing applies it for the
 *  user, so "edits land as it makes them" over an open proposal would be wrong. */
export function reviewOffNote(proposalOpen: boolean): string {
	return proposalOpen
		? 'Review is off, but this proposal is still open — the agent’s edits keep going into it until you apply or discard it.'
		: 'Review is off — an agent’s edits, its task work included, land on your timeline as it makes them.';
}

/** What the empty queue says an agent does with a task you queue. */
export function queueEmptyHint(review: boolean): string {
	return review
		? 'Queue a task below. Your connected agent claims it and proposes edits.'
		: 'Queue a task below. Your connected agent claims it and edits your timeline directly — review is off.';
}

/** The Quick edits legend for the queued chip: where the agent's work for it ends up. */
export function queuedChipLegend(review: boolean): string {
	return review ? 'is queued for your agent, which you review before it lands' : 'is queued for your agent, whose edits land as it makes them';
}
