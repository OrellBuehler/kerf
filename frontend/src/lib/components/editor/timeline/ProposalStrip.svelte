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
	import { placeStep, placesNote, proposalBarLine, reviewStepTip } from '#lib/proposal-ui';
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
	/** The cut has changed since the proposal was staged: said first, here too, since with the Agent panel away the bar is where
	 *  Apply is, and Apply must not be the thing that invites a press under a bar that does not say so. */
	const stale = $derived(!!staged?.stale);
</script>

{#if staged}
	<div data-proposal-strip data-previewing={previewing ? '' : undefined} data-stale={stale ? '' : undefined} role="status" class="bar">
		{#if previewing}
			<span class="live" aria-hidden="true"></span>
			<span
				class="lead"
				use:tip={'The agent’s proposal is on the timeline and the cut is read-only. Nothing is changed until you apply it.'}
				>Previewing the agent’s proposal{#if stale}<span class="warn">{' — your cut has changed since it was proposed'}</span>{:else}<span class="rest">{' — read-only, your timeline is unchanged'}</span>{/if}</span
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
			<Icon n={stale ? 'alert-triangle' : 'git-pull-request-arrow'} s={13} color={stale ? 'var(--diff-remove)' : 'var(--agent-300)'} />
			<span class="lead" use:tip={stale ? 'The cut has changed since the agent proposed these. Open the Agent panel for what applying does and the way out.' : undefined}
				>{proposalBarLine({ changes: staged.diff.entries.length, note: staged.note, stale })}<span class="rest">{' · outlined on the cut'}</span></span
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
	.bar[data-stale] {
		border-bottom-color: var(--diff-remove);
		color: var(--text-primary);
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
	.warn {
		color: var(--text-primary);
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
