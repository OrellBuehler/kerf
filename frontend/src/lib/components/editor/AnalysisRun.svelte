<script lang="ts">
	// An analysis pass that is running on an asset, where the asset is: the step it is on (`step 2 of 5 · detecting
	// scenes 40%`) and, in the Inspector, the way out. Analysis is minutes of work (a speech model to download, then
	// inference), so the bin card, the Inspector and the status bar all say where it is. Stop gives up between steps,
	// and within about a second during transcription. There is one Stop on the page per place you act from: the status
	// bar's (always on screen) and the Inspector's (where Analyze was pressed); a bin card only reports — a list of media
	// is no place for a button that ends minutes of work, one drag away from a click. Amber, Kerf's own work: cyan is the agent's.
	import Btn from './Btn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { tip } from '#lib/tip';

	let { compact = false, stoppable = true }: { compact?: boolean; stoppable?: boolean } = $props();

	/** The run that is going, whoever started it: an agent's has no Stop here. */
	const run = $derived(ui.activeRun);
	const label = $derived(run ? (run.own ? run.label : `${run.label} · started by an agent`) : 'analyzing');
	const queued = $derived(run && run.queued > 0 ? ` · ${run.queued} more queued` : '');
	/** This step's own progress, where the backend can say one. */
	const fraction = $derived(ui.analysisStage?.fraction ?? null);
</script>

<span
	data-testid="analysis-run"
	role="status"
	style="display:flex;flex-direction:column;gap:4px;min-width:0;{compact ? 'margin-top:5px' : ''}"
>
	<span style="display:flex;align-items:center;gap:8px;min-width:0">
		<span
			class="kerf-spin"
			style="flex:none;width:9px;height:9px;border:1.5px solid var(--kerf-400);border-top-color:transparent;border-radius:50%"
		></span>
		<span
			data-analysis-step
			use:tip={`${label}${queued}`}
			style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;color:var(--kerf-300)"
			>{label}{queued}</span
		>
		{#if stoppable && run?.own !== false}
			<Btn
				size="sm"
				disabled={ui.stoppingAnalysis}
				title="Stop analyzing — it gives up between steps, and within about a second during transcription"
				onclick={() => ui.stopAnalysis()}>{ui.stoppingAnalysis ? 'Stopping…' : 'Stop'}</Btn
			>
		{/if}
	</span>
	{#if !compact}
		<ProgressBar value={fraction} label="Analyzing this file" height={4} />
	{/if}
</span>
