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
		expect(actions.slice(apply - 250, apply)).toContain("variant={stale ? 'secondary' : 'primary'}");
		expect(actions.slice(discard - 250, discard)).toContain('variant="destructive"');
		expect(actions.slice(discard - 250, discard)).toContain('data-quiet');
		// Exit preview takes Preview’s place, so the toggle stays where the pointer is
		expect(actions).toContain("{previewing ? 'Exit preview' : 'Preview'}");
	});

	test('while the cut has changed since the proposal, the safe action is the bright one and Apply steps back', () => {
		expect(actions).toContain('const stale = $derived(!!editor.staged?.stale);');
		const preview = actions.slice(actions.indexOf('<Btn', actions.indexOf('<div data-proposal-actions')), actions.indexOf('data-testid="proposal-preview"'));
		expect(preview).toContain("variant={previewing ? 'agent' : stale ? 'primary' : 'secondary'}");
	});

	test('Apply and Discard are pending, not disabled, so keyboard focus is not dropped to the page while one is under way', () => {
		const apply = actions.slice(actions.indexOf('data-testid="proposal-apply"') - 250, actions.indexOf('data-testid="proposal-apply"') + 250);
		const discard = actions.slice(actions.indexOf('data-testid="proposal-discard"') - 250, actions.indexOf('data-testid="proposal-discard"') + 250);
		for (const [name, source] of [['Apply', apply], ['Discard', discard]]) {
			expect(source, name).toContain('pending={held}');
			expect(source, name).not.toContain('disabled=');
		}
	});

	test('Preview is a button whose label changes, not also a toggle: one is announced, not "Preview, pressed"', () => {
		expect(actions).not.toContain('aria-pressed');
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
		expect(page).not.toContain('Previewing the agent’s proposed');
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

	test('with the Timeline closed the frame says what it is, on the window’s edge where nothing else is', () => {
		expect(page).toContain('data-previewing-tag');
		expect(page).toContain('Previewing the agent’s proposal — read-only');
		const tag = /\.preview-tag \{([^}]*)\}/.exec(page);
		expect(tag, 'the tag rule').not.toBeNull();
		expect(tag![1]).toContain('pointer-events: none');
		expect(tag![1]).toMatch(/z-index: (3\d|4[0-4]);/);
		expect(page).toMatch(/@media \(max-width: 1100px\) \{\s*\.preview-tag \{\s*display: none;/);
	});

	test('the title-bar chip is the way out while previewing, with its whole sentence as its name', () => {
		const bar = read(`${EDITOR}TitleBar.svelte`);
		expect(bar).toContain('{#if proposal && editor.previewingStaged}');
		expect(bar).toContain('aria-label={PREVIEW_CHIP_NAME}');
		expect(bar).toContain('onclick={() => void editor.exitStagedPreview()}');
		expect(bar).toContain('{previewChipText(rightWidth)}');
	});

	test('a stale proposal is flagged in the bar too, before anything on it is pressed', () => {
		expect(strip).toContain('proposalBarLine({ changes: staged.diff.entries.length, note: staged.note, stale })');
		expect(strip).toContain('data-stale={stale ? \'\' : undefined}');
		expect(strip).toContain('your cut has changed since it was proposed');
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
	const effect = page.slice(page.indexOf("const ARRIVAL_TOAST = 'proposal-arrived';"), page.indexOf('/** `[` / `]`'));

	test('it is a calm toast with the way in, not the Agent panel brought forward', () => {
		expect(effect).toContain('arrivalNotice(staged)');
		expect(effect).toContain("action: { label: notice.action, onClick: () => workspace.show('agent') }");
		expect(effect.split('\n').filter((l) => /^\s*workspace\.show\('agent'\)/.test(l))).toEqual([]);
	});

	test('it is said once per proposal under one id, and not for one the user brought back', () => {
		expect(effect).toContain('if (agent.announce(staged))');
		expect(effect).toContain('id: ARRIVAL_TOAST');
		expect(read('lib/proposal-actions.ts')).toContain('agent.markAnnounced(editor.discarded.created_at)');
	});

	test('and it goes when the proposal is applied or discarded, or while it is previewed', () => {
		expect(effect).toContain('toast.dismiss(ARRIVAL_TOAST)');
		expect(effect).toContain('if (editor.previewingStaged) toast.dismiss(ARRIVAL_TOAST);');
		expect(effect).toMatch(/if \(!staged\) \{\s*toast\.dismiss\(ARRIVAL_TOAST\);/);
	});

	test('the three signposts that stay are still there', () => {
		expect(read(`${EDITOR}TitleBar.svelte`)).toContain('proposalIndicator(editor.staged)');
		expect(read(`${EDITOR}Workspace.svelte`)).toContain('proposalIndicator(editor.staged)');
		expect(strip).toContain('proposalBarLine(');
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

	test('rows tell the user’s edits from the agent’s, mark the current one and keep every row readable', () => {
		expect(panel).toContain('historyRows(editor.history)');
		expect(panel).toContain("aria-current={rev.state === 'current' ? 'step' : undefined}");
		expect(panel).toContain('data-source={rev.source}');
		expect(panel).toContain('{rev.who}');
		// undone rows are struck through at full contrast, not faded to a fraction of it, and Redo is not dimmed with them
		expect(panel).not.toContain('opacity:0.62');
		expect(panel).toContain("rev.state === 'undone' ? 'var(--text-secondary)' : 'var(--text-primary)'");
	});

	test('each Go back / Redo is named with its revision and time, so a list of them is not a list of the same words', () => {
		expect(panel).toContain('aria-label={goName(rev)}');
	});

	test('going to a revision reports a refusal and offers a way back that holds only while the cut is where the move put it', () => {
		expect(panel).toContain('onclick={() => void goTo(rev)}');
		expect(panel).not.toContain('editor.revertTo(rev.seq)');
		const go = panel.slice(panel.indexOf('async function goTo('), panel.indexOf("// ---- the agent's pending proposal"));
		expect(go).toContain('report(');
		expect(go).toContain('revertRefusal(e)');
		expect(go).toContain('action: backToAction(from)');
		expect(go).not.toContain('editor.revertTo(from)');
	});

	test('the quick edits’ Undo is the guarded one too, not `editor.undo()` on whatever is newest', () => {
		expect(panel).not.toContain('editor.undo()');
		expect(panel).toContain('action: undoAction()');
	});

	test('a head the page did not reach is looked for, the clock ticks only while the section is looked at, and the list is windowed', () => {
		expect(panel).toContain('needsOlderForHead(editor.history, editor.historyMore)');
		expect(panel).toContain('loadHeadOfHistory()');
		expect(panel).toContain('if (!(historyInView || atEnd) || !scroller) return;');
		expect(panel).not.toContain('setInterval(() => (now = Date.now()), 30_000);\n\t\treturn () => clearInterval');
		expect(panel).toContain('windowRows(allRows, shown)');
		expect(panel).toContain('{#each view.rows as rev, at (rev.seq)}');
		expect(panel).toContain('relativeTime(rev.iso, now)');
		// the rows are made from the history alone, not from the clock
		expect(panel).toContain('const allRows = $derived(historyRows(editor.history));');
	});

	test('the observer and the media query come from the panel’s own window, not the editor’s', () => {
		expect(panel).toContain('intersectionObserverFor(scroller,');
		expect(panel).not.toContain('new IntersectionObserver');
		expect(panel).toContain('windowOf(scroller)');
		expect(panel).not.toContain('window.matchMedia(');
	});

	test('a focused row is not hidden under the sticky tabs', () => {
		expect(panel).toMatch(/scroll-padding-top:34px/);
	});
});

describe('the review card holds at the smallest panel', () => {
	test('it is compact when the panel is as short as the composer’s hint gives way: one line a change, a two-line header', () => {
		expect(panel).toContain('const compact = $derived(panelHeight > 0 && !showComposerHint(panelHeight));');
		expect(panel).toContain('<span class:one-line={compact}');
		expect(panel).toContain('class:one-line={compact}');
		expect(panel).toContain('pickLine(picks.size, along.size, pickable.length, compact)');
	});

	test('the warning is the first thing under the header, so its buttons are on screen without scrolling', () => {
		const header = panel.indexOf('data-testid="proposal-headline"');
		const warning = panel.indexOf('{#if warning}');
		const pickLine = panel.indexOf('data-testid="pick-line"');
		expect(header).toBeLessThan(warning);
		expect(warning).toBeLessThan(pickLine);
		expect(panel).toContain('data-testid="stale-back"');
		expect(panel).toContain('data-testid="go-forward"');
	});

	test('what the warning says is the pure rule’s, with the pick state it is about', () => {
		expect(panel).toContain('reviewWarning({ stale: staged.stale, gap: agent.editsSince, partial: picks.size > 0 && !agent.coversAll, undone: branch })');
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
