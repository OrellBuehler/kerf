<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import Switch from './Switch.svelte';
	import Btn from './Btn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import { settings } from '$lib/settings.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { useFilter } from '$lib/settings-filter';
	import { cancelVoiceover, onVoiceoverProgress, prepareVoiceover, voiceoverStatus } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { failure } from '$lib/failure-copy';
	import { ANALYSIS_KINDS } from '$lib/analysis-steps';
	import { approxMB, isVoiceoverCancelled, loadPrefs, stageLabel } from '$lib/voiceover';
	import type { VoiceoverProgress, VoiceoverStatus } from '$lib/types';

	const filter = useFilter();

	const KIND_HELP: Record<(typeof ANALYSIS_KINDS)[number]['kind'], string> = {
		silence: 'Finds the silent spans, so Remove silences and the timeline markers have something to work from.',
		scenes: 'Finds the shot changes, for markers and rough cuts.',
		loudness: 'Measures EBU R128 loudness, so levels can be matched.',
		rhythm: 'Finds onsets, the tempo and beat grid, and whether the audio is speech or music — for Cut to the beat.',
		transcript:
			'Speech to text, for the Transcript tab and captions. Downloads a speech model on first use and runs for minutes on long clips.'
	};

	// ---- speech-to-text model ---------------------------------------------------------------------
	$effect(() => {
		void ui.loadTranscriptionStatus();
	});
	const tx = $derived(ui.transcription);
	const modelMB = (bytes: number) => `${Math.round(bytes / 1024 / 1024)} MB`;
	const downloading = $derived(ui.downloadingModel !== null);
	const modelPct = $derived(ui.modelProgress?.fraction == null ? null : Math.round(ui.modelProgress.fraction * 100));
	const modelLine = $derived(
		!tx
			? 'Checking…'
			: !tx.available
				? (tx.reason ?? 'Speech-to-text is not available in this build.')
				: tx.model_ready
					? `Ready · ${tx.backend === 'libwhisper' ? 'in-process whisper' : 'FFmpeg whisper filter'}`
					: `Not downloaded yet${tx.approx_download_bytes ? ` (${modelMB(tx.approx_download_bytes)})` : ''} — it downloads on first use, or fetch it now`
	);
	const modelBytes = $derived(
		ui.modelProgress && ui.modelProgress.total_bytes
			? `${modelMB(ui.modelProgress.downloaded_bytes)} of ${modelMB(ui.modelProgress.total_bytes)}`
			: ui.modelProgress
				? modelMB(ui.modelProgress.downloaded_bytes)
				: ''
	);

	// ---- voiceover model --------------------------------------------------------------------------
	let voStatus = $state<VoiceoverStatus | null>(null);
	let voError = $state<string | null>(null);
	let voPreparing = $state(false);
	let voCancelling = $state(false);
	let voProgress = $state<VoiceoverProgress | null>(null);
	$effect(() => {
		voiceoverStatus().then(
			(s) => (voStatus = s),
			(e) => (voError = e instanceof Error ? e.message : String(e))
		);
	});
	const voLine = $derived(
		voPreparing
			? 'Downloading the voice model…'
			: voStatus
				? !voStatus.available
					? (voStatus.reason ?? 'Voiceover is not available.')
					: voStatus.ready
						? 'Voice model ready'
						: `Not downloaded yet (${approxMB(voStatus.approx_download_bytes)})`
				: (voError ?? 'Checking…')
	);
	const voPct = $derived(voProgress?.fraction == null ? null : Math.round(voProgress.fraction * 100));

	async function downloadVoiceover() {
		if (!voStatus) return;
		// The voice last used, so the download is the one that will be wanted.
		const last = loadPrefs().voice;
		const voice = voStatus.voices.some((v) => v.id === last) ? last : voStatus.default_voice;
		voPreparing = true;
		voCancelling = false;
		voProgress = null;
		const unlisten = await onVoiceoverProgress((p) => (voProgress = p));
		try {
			voStatus = await prepareVoiceover(voice);
			toast.success('Voice model ready');
		} catch (e) {
			if (!isVoiceoverCancelled(e)) toast.error(failure('Voice model download failed', e));
		} finally {
			unlisten();
			voPreparing = false;
			voCancelling = false;
			voProgress = null;
		}
	}

	async function stopVoiceover() {
		voCancelling = true;
		await cancelVoiceover();
	}
</script>

<SettingRow id="analyze-on-import" for="sw-auto-analysis" label="Analyze new media when it is imported">
	{#snippet hint()}
		Each pass reads the whole file, one clip at a time, behind any preview proxy that is being built. Off, nothing runs
		by itself — right-click a clip in the bin and pick what to analyze.
	{/snippet}
	<Switch
		id="sw-auto-analysis"
		label="Analyze new media when it is imported"
		testid="auto-analysis-master"
		checked={settings.autoAnalysis.enabled}
		onchange={(on) => void settings.setAutoAnalysis({ enabled: on })}
	/>
</SettingRow>

{#each ANALYSIS_KINDS as k (k.kind)}
	<SettingRow id="analyze-{k.kind}" for="sw-analyze-{k.kind}" label="{k.label} on import" hint={KIND_HELP[k.kind]}>
		<Switch
			id="sw-analyze-{k.kind}"
			label="{k.label} on import"
			testid="auto-analysis-{k.kind}"
			checked={settings.autoAnalysis[k.kind]}
			onchange={(on) => void settings.setAutoKind(k.kind, on)}
		/>
	</SettingRow>
{/each}
{#if filter.shows('analyze-on-import') || ANALYSIS_KINDS.some((k) => filter.shows(`analyze-${k.kind}`))}
	<p style="margin:10px 0 0;font-size:12px;line-height:1.55;color:var(--text-muted)">
		With the master switch off, nothing runs on import. These choices still decide what a connected agent analyzes when it
		doesn't ask for specific steps. You can always analyze a clip by hand.
	</p>
{/if}

<SettingRow id="speech-model" label="Speech-to-text model" stacked>
	{#snippet hint()}
		Transcription downloads a speech model on first use and then runs for a good fraction of each clip's length.
		{settings.autoAnalysis.transcript
			? 'Transcription is on for new imports.'
			: 'Transcription is off for new imports: the Transcript tab and captions have nothing to work from until you transcribe a clip by hand (right-click it in the bin, Analyze).'}
	{/snippet}
	<div style="display:flex;align-items:center;gap:10px;flex-wrap:wrap">
		{#if tx && tx.models.length > 0}
			<select
				value={tx.model ?? ''}
				onchange={(e) => void ui.chooseSpeechModel(e.currentTarget.value)}
				disabled={downloading}
				aria-label="Speech model"
				class="k-select"
				data-testid="speech-model-select"
				style="color:var(--text-secondary)"
			>
				{#each tx.models as m (m.name)}
					<option value={m.name}>{m.name} · {modelMB(m.approx_bytes)}{m.multilingual ? '' : ' · English'}</option>
				{/each}
			</select>
		{/if}
		<span style="flex:1;min-width:140px;font-size:12px;color:{tx && !tx.available ? 'var(--red-400)' : 'var(--text-primary)'}">{modelLine}</span>
		{#if downloading}
			<Btn variant="destructive" size="sm" pending={ui.cancellingModel} data-testid="speech-model-stop" onclick={() => void ui.stopModelDownload()}>
				{ui.cancellingModel ? 'Stopping…' : 'Stop'}
			</Btn>
		{:else if tx?.available && !tx.model_ready}
			<Btn variant="secondary" size="sm" icon="download" data-testid="speech-model-download" onclick={() => void ui.fetchSpeechModel(tx.model ?? 'base')}
				>Download now</Btn
			>
		{/if}
	</div>
	{#if downloading}
		<div style="margin-top:8px;display:flex;flex-direction:column;gap:6px">
			<div style="display:flex;align-items:baseline;gap:8px;font-size:12px">
				<span style="color:var(--text-secondary)">Downloading {ui.downloadingModel}</span>
				<span style="flex:1;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);text-align:right"
					>{modelBytes}{modelPct !== null ? ` · ${modelPct}%` : ''}</span
				>
			</div>
			<ProgressBar value={ui.modelProgress?.fraction ?? null} label="Downloading the speech model" />
		</div>
	{/if}
</SettingRow>

<SettingRow id="voiceover-model" label="Voiceover model" stacked>
	{#snippet hint()}
		Voiceover turns a script into speech on this machine, in English, with a voice model fetched once.{#if !voStatus?.ready}
			Without this it downloads the first time you generate one.{/if}
	{/snippet}
	<div style="display:flex;align-items:center;gap:10px">
		<span style="flex:1;min-width:0;font-size:12px;color:{voStatus && !voStatus.available ? 'var(--red-400)' : 'var(--text-primary)'}">{voLine}</span>
		{#if voPreparing}
			<Btn variant="destructive" size="sm" pending={voCancelling} onclick={stopVoiceover}>
				{voCancelling ? 'Stopping…' : 'Cancel'}
			</Btn>
		{:else if voStatus?.available && !voStatus.ready}
			<Btn variant="secondary" size="sm" icon="download" onclick={downloadVoiceover}>Download now</Btn>
		{/if}
	</div>
	{#if voPreparing}
		<div style="margin-top:8px;display:flex;flex-direction:column;gap:6px">
			<div style="display:flex;align-items:baseline;gap:8px;font-size:12px">
				<span style="color:var(--text-secondary)">{voProgress ? stageLabel(voProgress.stage) : 'Starting…'}</span>
				<span style="flex:1;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);text-align:right"
					>{voProgress?.detail ?? ''}{voPct !== null ? ` · ${voPct}%` : ''}</span
				>
			</div>
			<ProgressBar value={voProgress?.fraction ?? null} label="Downloading the voice model" />
		</div>
	{/if}
</SettingRow>
