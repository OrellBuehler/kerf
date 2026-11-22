import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// How the review of an agent's proposal is wired where only a window shows it: one group of three answers in one order,
// one bar that changes its words and not its height, an arrival that does not take the screen over, a History section the
// Edit menu can find, and a connect row that leads somewhere in every state. Read off the source, as the other wiring tests
// are; the words and rules are tested where they live (`proposal-ui`, `history-view`, `agent-status`, `agent-review`).

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';
const actions = read(`${EDITOR}ProposalActions.svelte`);
const strip = read(`${EDITOR}timeline/ProposalStrip.svelte`);
const panel = read(`${EDITOR}AgentPanel.svelte`);
const page = read('routes/+page.svelte');
const empty = read(`${EDITOR}PreviewEmpty.svelte`);

describe('Preview, Apply, Discard: one group, one order, wherever they are', () => {
	test('the group is Preview (looks, changes nothing), then Apply, then Discard in the red ink of a removal', () => {
		const preview = actions.indexOf('data-testid="proposal-preview"');
		const apply = actions.indexOf('data-testid="proposal-apply"');
		const discard = actions.indexOf('data-testid="proposal-discard"');
		expect(preview).toBeGreaterThan(-1);
		expect(preview).toBeLessThan(apply);
		expect(apply).toBeLessThan(discard);
		expect(actions.slice(apply - 200, apply)).toContain('variant="primary"');
		expect(actions.slice(discard - 250, discard)).toContain('variant="destructive"');
		expect(actions.slice(discard - 250, discard)).toContain('data-quiet');
		// Exit preview takes Preview’s place, so the toggle stays where the pointer is
		expect(actions).toContain("{previewing ? 'Exit preview' : 'Preview'}");
	});

	test('the card’s pinned row and the bar over the timeline both render it, and nobody draws the three by hand', () => {
		expect(panel).toContain('<ProposalActions fill />');
		expect(strip).toContain('<ProposalActions busy={reviewBusy} act={reviewAct} />');
		for (const [name, source] of [['the panel', panel], ['the bar', strip], ['the page', page]] as const) {
			expect(source, name).not.toMatch(/>\s*Discard\s*<\/Btn>/);
			expect(source, name).not.toMatch(/proposal-apply/);
		}
		expect(actions).toContain('applyReview');
	});

	test('what Apply applies is what the card ticked, from either place', () => {
		const run = read('lib/proposal-actions.ts');
		expect(run).toContain('const picked = [...agent.picks]');
		expect(panel).not.toContain('let picks = $state');
		expect(panel).toContain('agent.togglePick(id)');
	});
});

describe('the bar over the timeline is one bar with two modes, so previewing moves nothing', () => {
	test('there is no second banner above the dock: the page only frames the window while previewing', () => {
		expect(page).not.toContain('Previewing the agent');
		expect(page).not.toContain('reviewAct');
		expect(page).not.toContain('reviewBusy');
		expect(page).toContain('data-previewing-frame');
		const frame = /data-previewing-frame[^>]*style="([^"]*)"/.exec(page);
		expect(frame, 'the frame').not.toBeNull();
		expect(frame![1]).toContain('pointer-events:none');
		expect(frame![1]).toContain('position:fixed');
		// the layer of what it is most like: above the timeline (24-29), under the dock’s overlay (45)
		expect(frame![1]).toMatch(/z-index:(3\d|4[0-4])\b/);
	});

	test('both modes are the same element at the same fixed height', () => {
		expect((strip.match(/data-proposal-strip/g) ?? []).length).toBe(1);
		expect(strip).toMatch(/\.bar \{[^}]*\bheight: 38px;/);
		expect(strip).toContain('{#if previewing}');
		expect(strip).toContain('Previewing the agent’s proposal');
		expect(strip).toContain("{' — read-only, your timeline is unchanged'}");
	});

	test('the stepper says what a place is, and the keys go through the one stepping function', () => {
		expect(strip).toContain('placeStep(');
		expect(strip).toContain('placesNote(');
		expect(strip).toContain('stepProposal(-1)');
		expect(strip).toContain('stepProposal(1)');
		expect(page).toContain('return stepProposal(dir)');
	});
});

describe('a proposal arriving does not take the screen over', () => {
	test('it is a calm toast with the way in, not the Agent panel brought forward', () => {
		const effect = page.slice(page.indexOf('let hadStaged = false;'), page.indexOf('/** `[` / `]`'));
		expect(effect).toContain('arrivalNotice(staged)');
		expect(effect).toContain("action: { label: notice.action, onClick: () => workspace.show('agent') }");
		expect(effect).not.toMatch(/if \(has && !hadStaged\) workspace\.show\('agent'\)/);
		expect(effect.split('\n').filter((l) => /^\s*workspace\.show\('agent'\)/.test(l))).toEqual([]);
	});

	test('the three signposts that stay are still there', () => {
		expect(read(`${EDITOR}TitleBar.svelte`)).toContain('proposalIndicator(editor.staged)');
		expect(read(`${EDITOR}Workspace.svelte`)).toContain('proposalIndicator(editor.staged)');
		expect(strip).toContain('proposalLine(');
	});
});

describe('the edit history is a section of its own that the Edit menu can find', () => {
	test('the section’s root is marked, headed History, and sits under a tab that jumps to it', () => {
		expect(panel).toContain('<section bind:this={historyEl} data-history aria-labelledby="agent-history-h"');
		expect(panel).toContain("'agent-history-h')}");
		expect(panel).toContain("{@render secHead('History'");
		expect(panel).toContain('data-testid="history-tab"');
		expect(panel).toContain('historyEl?.scrollIntoView({ block: \'start\'');
	});

	test('rows tell the user’s edits from the agent’s, mark the current one and dim the undone ones', () => {
		expect(panel).toContain('historyRows(editor.history, now)');
		expect(panel).toContain("aria-current={rev.state === 'current' ? 'step' : undefined}");
		expect(panel).toContain('data-source={rev.source}');
		expect(panel).toContain("rev.state === 'undone' ? 'opacity:0.62'");
		expect(panel).toContain('{rev.who}');
	});

	test('going to a revision reports a refusal and offers the way back, instead of dropping the promise', () => {
		expect(panel).toContain('onclick={() => void goTo(rev)}');
		expect(panel).not.toContain('editor.revertTo(rev.seq)');
		const go = panel.slice(panel.indexOf('async function goTo('), panel.indexOf('// ---- the agent\'s pending proposal'));
		expect(go).toContain('report(e)');
		expect(go).toContain("label: 'Undo'");
	});
});

describe('connecting an agent leads somewhere in every state', () => {
	test('the first-run button opens the setup itself, not just the tab it is in', () => {
		expect(empty).toContain("if (agentRow.opens === 'setup') agent.connectOpen = true;");
		expect(empty).toContain('$effect(() => agent.watch())');
		expect(empty).toContain('firstRunAgent(');
		expect(empty).not.toContain('Or connect an agent over MCP');
	});

	test('the setup card says what an agent is for and where it stands, and folds away when one connects', () => {
		expect(panel).toContain('agentPurpose(settings.agentReview)');
		expect(panel).toContain('data-testid="connect-state"');
		expect(panel).toContain('now && !wasConnected');
		expect(panel).toContain('agent.connectOpen = false');
	});

	test('a dead endpoint has a way out in the panel, and an empty queue offers the setup', () => {
		expect(panel).toContain('data-testid="agent-fix"');
		expect(panel).toContain("settings.openSection('agent')");
		expect(panel).toContain('data-testid="queue-connect"');
		expect(panel).toContain('QUEUE_EMPTY_NO_AGENT');
	});

	test('the panel and the first-run card read one connection, not two polls', () => {
		expect(panel).toContain('$effect(() => agent.watch())');
		expect(panel).not.toContain('setInterval(() => void poll()');
		expect(panel).not.toContain('agentStatus()');
	});
});
