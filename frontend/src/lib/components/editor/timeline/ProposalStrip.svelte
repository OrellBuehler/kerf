<script lang="ts">
	/* The bar over the timeline's toolbar while an agent's proposal is open: one bar, two modes, always the same height.
	   While the cut on screen is still the user's it says a proposal is waiting; once it is previewed the same bar says the
	   cut on screen is the agent's and read-only, and steps through where it changes the cut. It used to be a strip here and,
	   once previewed, a second banner above the whole dock that pushed every panel down by its own height and took the
	   Preview button out from under the pointer — a bar that only changes its words moves nothing. */
	import Icon from '../Icon.svelte';
	import IconBtn from '../IconBtn.svelte';
	import ProposalActions from '../ProposalActions.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { tip } from '#lib/tip';
	import { stepProposal } from '#lib/proposal-actions';
	import { placeStep, placesNote, proposalLine, reviewStepTip } from '#lib/proposal-ui';
	import { changeIndex, changePoints } from '#lib/staged-diff';

	let {
		reviewBusy,
		reviewAct
	}: {
		reviewBusy: boolean;
		reviewAct: (answer: () => Promise<boolean>) => Promise<void>;
	} = $props();

	const staged = $derived(editor.staged);
	const previewing = $derived(editor.previewingStaged);
	const points = $derived(staged && previewing ? changePoints(staged.diff.entries) : []);
	const place = $derived(placeStep(points.length, changeIndex(points, ui.time, editor.fps, editor.duration)));
	const note = $derived(staged ? placesNote(points.length, staged.diff.entries.length) : null);
</script>

{#if staged}
	<div data-proposal-strip data-previewing={previewing ? '' : undefined} role="status" class="bar">
		{#if previewing}
			<span class="live" aria-hidden="true"></span>
			<span
				class="lead"
				use:tip={'The agent’s proposal is on the timeline and the cut is read-only. Nothing is changed until you apply it.'}
				>Previewing the agent’s proposal<span class="rest">{' — read-only, your timeline is unchanged'}</span></span
			>
			<span class="stepper" data-review-nav>
				<IconBtn
					size={24}
					title={settings.tip('Previous change', 'review.prevChange')}
					disabled={points.length === 0}
					onclick={() => void stepProposal(-1)}><Icon n="chevron-left" s={13} /></IconBtn
				>
				<span
					class="place"
					aria-live="polite"
					use:tip={reviewStepTip(points.length, staged.diff.entries.length, settings.keysFor('review.prevChange'), settings.keysFor('review.nextChange'))}
					>{place}</span
				>
				<IconBtn
					size={24}
					title={settings.tip('Next change', 'review.nextChange')}
					disabled={points.length === 0}
					onclick={() => void stepProposal(1)}><Icon n="chevron-right" s={13} /></IconBtn
				>
				{#if note}<span class="note">({note})</span>{/if}
			</span>
			<span class="legend" data-review-legend>
				<span class="key"><i style="border:2px solid var(--diff-add)"></i>added</span>
				<span class="key"><i style="border:2px dashed var(--diff-shift)"></i>changed</span>
				<span class="key"
					><i
						style="border:1.5px dashed var(--diff-remove);background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--diff-remove) 55%,transparent) 0 1px,transparent 1px 4px)"
					></i>removed</span
				>
			</span>
		{:else}
			<Icon n="git-pull-request-arrow" s={13} color="var(--agent-300)" />
			<span class="lead"
				>{proposalLine(staged.diff.entries.length)}{staged.note ? ` — ${staged.note}` : ''}<span class="rest">{' · outlined on the cut'}</span></span
			>
		{/if}
		<div style="flex:1"></div>
		<ProposalActions busy={reviewBusy} act={reviewAct} />
	</div>
{/if}

<style>
	/* The one height of the bar in both modes; the container query below is about the timeline's width, which is the bar's. */
	.bar {
		container-type: inline-size;
		flex: none;
		box-sizing: border-box;
		height: 38px;
		display: flex;
		align-items: center;
		gap: 9px;
		padding: 0 12px;
		overflow: hidden;
		background: var(--agent-surface);
		border-bottom: var(--line-width) solid var(--agent-border);
		color: var(--agent-300);
		font-size: 12px;
	}
	.lead {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.rest {
		color: var(--text-muted);
	}
	.live {
		flex: none;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--agent-400);
		box-shadow: 0 0 8px var(--agent-400);
	}
	.stepper {
		flex: none;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		color: var(--text-secondary);
		font-size: 11px;
		white-space: nowrap;
	}
	.place {
		font-family: var(--font-mono);
		min-width: 12ch;
		text-align: center;
	}
	.note {
		margin-left: 4px;
		color: var(--text-muted);
	}
	.legend {
		flex: none;
		display: inline-flex;
		align-items: center;
		gap: 10px;
		margin-left: 4px;
		font-size: 11px;
		color: var(--text-secondary);
		white-space: nowrap;
	}
	.key {
		display: inline-flex;
		align-items: center;
		gap: 4px;
	}
	.key i {
		display: inline-block;
		width: 12px;
		height: 9px;
		border-radius: 2px;
		box-sizing: border-box;
	}
	/* Narrower, the words give way before anything that acts: the legend, then the explanation, then the stepper. */
	@container (max-width: 1060px) {
		.legend {
			display: none;
		}
	}
	@container (max-width: 860px) {
		.rest,
		.note {
			display: none;
		}
	}
	@container (max-width: 640px) {
		.stepper {
			display: none;
		}
	}
</style>
