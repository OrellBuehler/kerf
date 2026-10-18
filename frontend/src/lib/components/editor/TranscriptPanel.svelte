<script lang="ts">
	// The transcript as an editing surface: lines resolve to the clip carrying
	// them, click seeks, the playhead line highlights, and × cuts the sentence
	// (its words, when they are timed) from the timeline. Speakers label the
	// lines and can filter them. Empty, it says which of the five reasons applies.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import IconBtn from './IconBtn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { toast } from '$lib/notifications.svelte';
	import { exportTranscriptSrt, report } from '$lib/ops';
	import { activeLineIndex, speakersOf, srcToTimeline, transcriptLines, type TxLine } from '$lib/transcript';
	import { tip } from '$lib/tip';

	const txLines = $derived(
		transcriptLines(editor.selectedMetadata?.analysis?.transcript ?? [], editor.selectedAssetId, editor.timeline.tracks)
	);
	const activeTx = $derived(activeLineIndex(txLines, ui.time));
	const speakers = $derived(speakersOf(editor.selectedMetadata?.analysis?.transcript ?? []));
	let speakerFilter = $state<string | null>(null);
	$effect(() => {
		if (speakerFilter && !speakers.includes(speakerFilter)) speakerFilter = null;
	});
	const shown = $derived(
		txLines.map((l, i) => ({ l, i })).filter(({ l }) => !speakerFilter || l.speaker === speakerFilter)
	);
	let refining = $state(false);

	async function labelSpeakers() {
		const id = editor.selectedAssetId;
		if (!id || ui.analyzing) return;
		refining = true;
		try {
			const n = await ui.refineTranscript(id);
			if (n !== null) toast(n > 1 ? `${n} speakers labelled` : 'One speaker found');
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		} finally {
			refining = false;
		}
	}

	function seekLine(l: TxLine) {
		if (!l.clip) return;
		// The start of a line: the first frame inside it, so it is the line the playhead is on.
		const from = srcToTimeline(l.clip, l.start);
		ui.seek(from, { snap: 'up', end: Math.max(from, srcToTimeline(l.clip, l.end)) });
	}

	async function cutLine(l: TxLine) {
		if (!l.clip) return;
		try {
			await editor.cutRange(l.clip.id, l.cut.start, l.cut.end);
			toast('Line cut from timeline', {
				action: { label: 'Undo', onClick: () => void editor.undo() }
			});
		} catch (e) {
			report(e);
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
			return { title: 'No media selected', body: 'Pick a clip in the Media tab of the Library to see its transcript.' };
		}
		if (st && !st.available) {
			return { title: 'Transcription unavailable', body: st.reason ?? undefined };
		}
		const transcribed = mediaStatus.done(editor.selectedAssetId, 'transcript');
		if (st && !st.enabled && !transcribed) {
			return {
				title: 'Speech-to-text is off for new imports',
				body: 'Turn it on in Settings › Analysis, or transcribe this clip by hand.',
				action: 'analyze'
			};
		}
		if (st && !st.model_ready) {
			return {
				title: 'Speech model not downloaded',
				body: `Transcription needs a speech model (${mb(st.approx_download_bytes)}). It downloads on first use, or fetch it now.`,
				action: 'download'
			};
		}
		if (!transcribed) {
			return { title: 'Not transcribed yet', body: 'Transcribe this clip to read its speech.', action: 'analyze' };
		}
		return { title: 'No speech found', body: 'This media was analyzed but no speech was detected in it.' };
	});
</script>

<div style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden">
	<div style="flex:1;overflow-y:auto;padding:12px">
	{#if txLines.length === 0}
		<EmptyState icon="captions" title={tx.title} hint={tx.body}>
			{#if ui.analyzing}
				<div style="width:200px">
					<ProgressBar value={ui.analysisStage?.fraction ?? null} label="Transcribing" height={3} />
				</div>
			{:else if tx.action === 'download'}
				<div style="display:flex;flex-direction:column;gap:8px;align-items:center">
					<select
						value={ui.transcription?.model ?? ''}
						onchange={(e) => void ui.chooseSpeechModel((e.currentTarget as HTMLSelectElement).value)}
						disabled={!!ui.downloadingModel}
						aria-label="Speech model"
						class="k-select"
						style="color:var(--text-secondary)"
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
						<div style="width:200px">
							<ProgressBar value={ui.modelFraction} label="Downloading the speech model" height={3} />
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
				<Btn
					size="sm"
					onclick={() =>
						void ui
							.runAnalysis(editor.selectedAssetId!, ['transcript'])
							.catch((err) => toast.error(err instanceof Error ? err.message : String(err)))}>Transcribe</Btn
				>
			{/if}
		</EmptyState>
	{:else}
		<div style="display:flex;align-items:center;gap:8px;padding:0 6px 8px">
			<span class="k-overline" style="flex:1">{txLines.length} line{txLines.length === 1 ? '' : 's'}</span>
			{#if speakers.length > 1}
				<select
					value={speakerFilter ?? ''}
					onchange={(e) => (speakerFilter = (e.currentTarget as HTMLSelectElement).value || null)}
					aria-label="Show speaker"
					class="k-select"
					style="color:var(--text-secondary)"
				>
					<option value="">All speakers</option>
					{#each speakers as sp (sp)}
						<option value={sp}>{sp}</option>
					{/each}
				</select>
			{:else if speakers.length === 0}
				<Btn size="sm" onclick={() => void labelSpeakers()} disabled={ui.analyzing}
					>{refining ? 'Labelling speakers…' : 'Label speakers'}</Btn
				>
			{/if}
			<Btn
				size="sm"
				icon="file-text"
				iconSize={12}
				title="Write the transcript as a subtitle file, in this media's own time"
				onclick={() => editor.selectedAssetId && void exportTranscriptSrt(editor.selectedAssetId)}>Export .srt</Btn
			>
		</div>
		<div data-selectable style="display:flex;flex-direction:column;gap:2px">
			{#each shown as { l, i }, k (i)}
				{#if speakers.length > 1 && l.speaker && (k === 0 || shown[k - 1].l.speaker !== l.speaker)}
					<div class="speaker">{l.speaker}</div>
				{/if}
				<div class="line" data-active={i === activeTx ? '' : undefined}>
					<button
						onclick={() => seekLine(l)}
						disabled={!l.clip}
						use:tip={l.clip ? 'Jump to this line on the timeline' : 'Not on the timeline (cut)'}
						style="display:flex;gap:8px;flex:1;background:none;border:none;padding:0;text-align:left;opacity:1;cursor:{l.clip
							? 'pointer'
							: 'default'}"
					>
						<span
							style="font-family:var(--font-mono);font-size:var(--text-2xs);color:var(--text-muted);flex:none;padding-top:1px"
							>{l.t}</span
						>
						<span
							class="words"
							style="text-decoration:{l.clip ? 'none' : 'line-through'};color:{l.clip ? '' : 'var(--text-muted)'}">{l.s}</span
						>
					</button>
					{#if l.clip}
						<IconBtn
							size={24}
							data-tone="quiet"
							title="Cut this line out of the timeline"
							aria-label="Cut line"
							onclick={() => void cutLine(l)}
							style="margin:-4px 0"><Icon n="x" s={12} /></IconBtn
						>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
	</div>
</div>

<style>
	/* A line is lit by the pointer, and the one the playhead is in carries the accent bar: the old
	   darker block alone was easy to miss. Text on `surface-hover` is held to 4.5:1 by the contrast guard. */
	.speaker {
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		padding: 8px 8px 2px;
	}
	.line {
		display: flex;
		gap: 8px;
		align-items: flex-start;
		padding: 7px 8px 7px 6px;
		border-left: 2px solid transparent;
		border-radius: var(--radius-sm);
	}
	.line:hover {
		background: color-mix(in srgb, var(--surface-hover) 60%, transparent);
	}
	.line[data-active] {
		background: var(--surface-hover);
		border-left-color: var(--kerf-500);
	}
	.words {
		font-size: var(--text-sm);
		line-height: 1.45;
		color: var(--text-secondary);
	}
	.line[data-active] .words {
		color: var(--text-primary);
	}
</style>
