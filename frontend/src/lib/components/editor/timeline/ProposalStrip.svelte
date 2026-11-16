<script lang="ts">
	/* The strip over the toolbar that says a proposal is waiting and offers Preview, Apply and Discard. */
	import Btn from '../Btn.svelte';
	import Icon from '../Icon.svelte';
	import { applyProposal, discardProposal } from '#lib/proposal-actions';
	import { proposalLine } from '#lib/proposal-ui';
	import { editor } from '#lib/state.svelte';

	let {
		reviewBusy,
		reviewAct
	}: {
		reviewBusy: boolean;
		reviewAct: (answer: () => Promise<boolean>) => Promise<void>;
	} = $props();
</script>

{#if editor.staged && !editor.previewingStaged}
	<div
		data-proposal-strip
		role="status"
		style="flex:none;display:flex;align-items:center;gap:8px;min-height:30px;padding:3px 12px;background:var(--agent-surface);border-bottom:var(--line-width) solid var(--agent-border);color:var(--agent-300);font-size:12px"
	>
		<Icon n="git-pull-request-arrow" s={13} color="var(--agent-300)" />
		<span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
			>{proposalLine(editor.staged.diff.entries.length)}{editor.staged.note ? ` — ${editor.staged.note}` : ''}</span
		>
		<span style="color:var(--text-muted);white-space:nowrap">· outlined on the cut</span>
		<div style="flex:1"></div>
		<Btn size="sm" icon="eye" iconSize={12} disabled={reviewBusy} onclick={() => void editor.previewStaged()}>Preview</Btn>
		<Btn variant="primary" size="sm" disabled={reviewBusy} onclick={() => void reviewAct(applyProposal)}>Apply</Btn>
		<Btn size="sm" disabled={reviewBusy} onclick={() => void reviewAct(discardProposal)}>Discard</Btn>
	</div>
{/if}
