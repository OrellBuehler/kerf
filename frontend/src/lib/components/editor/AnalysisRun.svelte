<script lang="ts">
	// An analysis pass that is running on an asset, where the asset is: the step it is on (`step 2 of 5 · detecting
	// scenes 40%`) and the way out. Analysis is minutes of work (a speech model to download, then inference), and
	// its only controls were in the status bar — the bin card and the Inspector, which started it, said "analyzing…"
	// and nothing else. Stop gives up between steps, and within about a second during transcription.
	import Btn from './Btn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { tip } from '$lib/tip';

	let { compact = false }: { compact?: boolean } = $props();

	const label = $derived(ui.analysisLabel ?? 'analyzing');
	const queued = $derived(ui.analysisQueued > 0 ? ` · ${ui.analysisQueued} queued` : '');
</script>

<span
	data-testid="analysis-run"
	role="status"
	style="display:flex;align-items:center;gap:8px;min-width:0;{compact ? 'margin-top:5px' : ''}"
>
	<span
		class="kerf-spin"
		style="flex:none;width:9px;height:9px;border:1.5px solid var(--agent-400);border-top-color:transparent;border-radius:50%"
	></span>
	<span
		data-analysis-step
		use:tip={`${label}${queued}`}
		style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;color:var(--agent-300)"
		>{label}{queued}</span
	>
	<Btn
		size="sm"
		disabled={ui.stoppingAnalysis}
		title="Stop analyzing — it gives up between steps, and within about a second during transcription"
		onclick={() => ui.stopAnalysis()}>{ui.stoppingAnalysis ? 'Stopping…' : 'Stop'}</Btn
	>
</span>
