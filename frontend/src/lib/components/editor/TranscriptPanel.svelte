<script lang="ts">
	// The transcript as an editing surface: lines resolve to the clip carrying
	// them, click seeks, the playhead line highlights, and × cuts the sentence
	// from the timeline. Empty, it says which of the five reasons applies.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { toast } from '$lib/notifications.svelte';
	import { activeLineIndex, srcToTimeline, transcriptLines, type TxLine } from '$lib/transcript';

	const txLines = $derived(
		transcriptLines(editor.selectedMetadata?.analysis?.transcript ?? [], editor.selectedAssetId, editor.timeline.tracks)
	);
	const activeTx = $derived(activeLineIndex(txLines, ui.time));

	function seekLine(l: TxLine) {
		if (l.clip) ui.seek(srcToTimeline(l.clip, l.start));
	}

	async function cutLine(l: TxLine) {
		if (!l.clip) return;
		try {
			await editor.cutRange(l.clip.id, l.start, l.end);
			toast('Line cut from timeline', {
				action: { label: 'Undo', onClick: () => void editor.undo() }
			});
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	/** What the empty transcript panel should say and offer. An empty transcript
	 * has several quite different causes — nothing selected, no speech-to-text
	 * backend, a model still to download, analysis not run, or genuinely no
	 * speech — and telling them apart is the difference between a dead end and
	 * one click. */
	const tx = $derived.by<{ title: string; body?: string; action?: 'download' | 'analyze' | 'settings' }>(() => {
		const st = ui.transcription;
		const mb = (b?: number | null) => (b ? `${Math.round(b / 1024 / 1024)} MB` : 'a few hundred MB');
		if (ui.analyzing) {
			return { title: ui.analysisLabel ?? 'analyzing', body: ui.analysisStage?.detail ?? undefined };
		}
		if (!editor.selectedAssetId) {
			return { title: 'No media selected', body: 'Pick a clip in the Media panel to see its transcript.' };
		}
		if (st && !st.available) {
			return { title: 'Transcription unavailable', body: st.reason ?? undefined };
		}
		if (st && !st.enabled) {
			return {
				title: 'Speech-to-text is off',
				body: 'Analysis skips transcription while it is off. Turn it on in Settings, then analyze the clip.',
				action: 'settings'
			};
		}
		if (st && !st.model_ready) {
			return {
				title: 'Speech model not downloaded',
				body: `Transcription needs a speech model (${mb(st.approx_download_bytes)}). It downloads on first use, or fetch it now.`,
				action: 'download'
			};
		}
		if (!editor.selectedMetadata?.analysis) {
			return { title: 'Not analyzed yet', body: 'Analyze this clip to transcribe its speech.', action: 'analyze' };
		}
		return { title: 'No speech found', body: 'This media was analyzed but no speech was detected in it.' };
	});
</script>

<div style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden">
	<div style="flex:1;overflow-y:auto;padding:12px">
	{#if txLines.length === 0}
		<div
			style="display:flex;flex-direction:column;align-items:center;gap:12px;padding:36px 16px;color:var(--text-disabled);text-align:center"
		>
			<Icon n="captions" s={22} />
			<span style="font-size:12px;color:var(--text-secondary)">{tx.title}</span>
			{#if tx.body}
				<span style="font-size:11px;line-height:1.5;max-width:240px">{tx.body}</span>
			{/if}
			{#if ui.analyzing}
				<div style="width:200px;height:3px;border-radius:999px;background:var(--surface-inset);overflow:hidden">
					<div
						style="height:100%;width:{Math.round(
							(ui.analysisStage?.fraction ?? 0) * 100
						)}%;background:var(--kerf-500);transition:width .2s"
					></div>
				</div>
			{:else if tx.action === 'download'}
				<div style="display:flex;flex-direction:column;gap:8px;align-items:center">
					<select
						value={ui.transcription?.model ?? ''}
						onchange={(e) => void ui.chooseSpeechModel((e.currentTarget as HTMLSelectElement).value)}
						disabled={!!ui.downloadingModel}
						style="background:var(--surface-inset);color:var(--text-secondary);border:1px solid var(--border-default);border-radius:var(--radius-sm);font-size:11px;padding:4px 6px"
					>
						{#each ui.transcription?.models ?? [] as m (m.name)}
							<option value={m.name}
								>{m.name} · {Math.round(m.approx_bytes / 1024 / 1024)} MB{m.multilingual
									? ''
									: ' · English'}</option
							>
						{/each}
					</select>
					{#if ui.downloadingModel}
						<div style="width:200px;height:3px;border-radius:999px;background:var(--surface-inset);overflow:hidden">
							<div
								style="height:100%;width:{Math.round(
									ui.modelFraction * 100
								)}%;background:var(--kerf-500);transition:width .2s"
							></div>
						</div>
					{:else}
						<Btn size="sm" onclick={() => void ui.fetchSpeechModel(ui.transcription?.model ?? 'base')}
							>Download model</Btn
						>
					{/if}
				</div>
			{:else if tx.action === 'settings'}
				<Btn size="sm" onclick={() => settings.toggle()}>Open Settings</Btn>
			{:else if tx.action === 'analyze' && editor.selectedAssetId}
				<Btn size="sm" onclick={() =>
						void ui
							.runAnalysis(editor.selectedAssetId!)
							.catch((err) => toast.error(err instanceof Error ? err.message : String(err)))}>Analyze &amp; transcribe</Btn>
			{/if}
		</div>
	{:else}
		<div data-selectable style="display:flex;flex-direction:column;gap:2px">
			{#each txLines as l, i (i)}
				<div
					style="display:flex;gap:8px;padding:7px 8px;border-radius:var(--radius-sm);align-items:flex-start;background:{i ===
					activeTx
						? 'var(--surface-inset)'
						: 'transparent'}"
				>
					<button
						onclick={() => seekLine(l)}
						disabled={!l.clip}
						title={l.clip ? 'Jump to this line on the timeline' : 'Not on the timeline (cut)'}
						style="display:flex;gap:8px;flex:1;background:none;border:none;padding:0;text-align:left;cursor:{l.clip
							? 'pointer'
							: 'default'}"
					>
						<span
							style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled);flex:none;padding-top:1px"
							>{l.t}</span
						>
						<span
							style="font-size:12px;line-height:1.45;color:{l.clip
								? 'var(--text-secondary)'
								: 'var(--text-disabled)'};text-decoration:{l.clip ? 'none' : 'line-through'}">{l.s}</span
						>
					</button>
					{#if l.clip}
						<button
							title="Cut this line out of the timeline"
							aria-label="Cut line"
							onclick={() => void cutLine(l)}
							style="background:none;border:none;cursor:pointer;color:var(--text-disabled);display:grid;place-items:center;padding:1px 0 0"
							><Icon n="x" s={11} /></button
						>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
	</div>
</div>
