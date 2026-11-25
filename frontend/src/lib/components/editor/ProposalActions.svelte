<script lang="ts">
	/* The three answers to an agent's proposal, in one order wherever they are asked: Preview (looks, changes nothing),
	   Apply (the cut changes, Undo takes it back), Discard (the proposal goes, Restore brings it back) — the safe action
	   first and the throwing-away one last, in the red ink of a removal. The Agent panel's pinned row and the bar over the
	   timeline both render this, so a press means the same in either and a pick made in the card is what either applies. */
	import Btn from './Btn.svelte';
	import { agent } from '#lib/agent.svelte';
	import { editor } from '#lib/state.svelte';
	import { report } from '#lib/ops';
	import { applyReview, discardProposal } from '#lib/proposal-actions';
	import { applyTip } from '#lib/review-picks';

	let {
		fill = false,
		busy = false,
		act = undefined
	}: {
		/** Apply takes the room the row has left (the panel's footer); otherwise the three are their own width. */
		fill?: boolean;
		/** One of the answers is under way elsewhere (the timeline's guard): held, so a double click is one answer. */
		busy?: boolean;
		/** Runs an answer under that guard; without one the buttons guard themselves. */
		act?: (answer: () => Promise<boolean>) => Promise<void>;
	} = $props();

	let own = $state(false);
	const held = $derived(busy || own);
	const previewing = $derived(editor.previewingStaged);
	/** The cut has changed since the proposal was staged: applying replaces it (or merges onto it), which is not what a first
	 *  glance at a bright button should invite. The safe action is the one that stands out then, and Apply steps back. */
	const stale = $derived(!!editor.staged?.stale);

	async function run(answer: () => Promise<boolean>) {
		if (act) return act(answer);
		own = true;
		try {
			await answer();
		} finally {
			own = false;
		}
	}

	async function togglePreview() {
		try {
			if (previewing) await editor.exitStagedPreview();
			else await editor.previewStaged();
		} catch (e) {
			report(e);
		}
	}

	// Ticks are kept to the changes the proposal still has, and the changes that come along are read against its cut.
	$effect(() => {
		void editor.staged?.updated_at;
		void agent.picks;
		agent.syncProposal();
	});
</script>

<!-- Apply and Discard are `pending`, not `disabled`, while one answer is under way: a disabled button drops keyboard focus to the
     page, and the key that was about to press the next one is lost (`Btn`). The Preview button changes its label rather than being a
     toggle: one or the other is announced, not "Preview, pressed". -->
<div data-proposal-actions style="display:flex;align-items:center;gap:7px;min-width:0">
	<Btn
		size="sm"
		variant={previewing ? 'agent' : stale ? 'primary' : 'secondary'}
		icon={previewing ? 'eye-off' : 'eye'}
		iconSize={13}
		data-testid="proposal-preview"
		title={previewing ? 'Back to your own cut' : 'Show the proposed cut in the editor — nothing changes until you apply it'}
		onclick={togglePreview}
		style="white-space:nowrap"
	>
		{previewing ? 'Exit preview' : 'Preview'}
	</Btn>
	<Btn
		variant={stale ? 'secondary' : 'primary'}
		size="sm"
		pending={held}
		data-testid="proposal-apply"
		title={applyTip(agent.picks.size, agent.along.size, agent.rows.length)}
		onclick={() => void run(applyReview)}
		style="white-space:nowrap;{fill ? 'flex:1' : ''}">{agent.applyText(held)}</Btn
	>
	<Btn
		variant="destructive"
		size="sm"
		data-quiet
		pending={held}
		data-testid="proposal-discard"
		title="Throw the proposal away. Restore, in the Agent panel, brings it back until the agent proposes something new."
		onclick={() => void run(discardProposal)}
		style="white-space:nowrap">Discard</Btn
	>
</div>
