<script lang="ts">
	// Split an asset's sound into drums, bass, other and vocals. Opened from the library (the asset
	// alone: the four stems join the bin) or from a clip (they are also laid under it on new tracks and
	// the clip's own sound is switched off). The first run downloads the runtime and the model, so the
	// dialog says so up front, then names each stage as it runs — the voiceover dialog's arrangement.
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import { editor } from '$lib/state.svelte';
	import { cancelStems, onStemsProgress, stemsStatus } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { failure } from '$lib/failure-copy';
	import { formatTime } from '$lib/diff';
	import type { StemsProgress, StemsStatus } from '$lib/types';
	import { STEM_NAMES, capitalized, describeStems, downloadNote, isStemsCancelled, stageLabel } from '$lib/stems';
	import { tip } from '$lib/tip';

	let { assetId, clipId = null, onClose }: { assetId: string; clipId?: string | null; onClose: () => void } = $props();

	const asset = $derived(editor.assets.find((a) => a.id === assetId));
	const placement = $derived.by(() => {
		if (!clipId) return null;
		for (const track of editor.timeline.tracks) {
			const clip = track.clips.find((c) => c.id === clipId);
			if (clip) return { track, clip };
		}
		return null;
	});

	let status = $state<StemsStatus | null>(null);
	let statusError = $state<string | null>(null);
	let running = $state(false);
	let cancelling = $state(false);
	let progress = $state<StemsProgress | null>(null);

	// The asset or clip going away under the dialog (an undo, an agent's edit) ends it — unless a run
	// is under way, which the backend then answers for itself.
	$effect(() => {
		if (!running && (!asset || (clipId && !placement))) onClose();
	});

	$effect(() => {
		stemsStatus().then(
			(s) => (status = s),
			(e) => (statusError = e instanceof Error ? e.message : String(e))
		);
	});

	const note = $derived(status ? downloadNote(status) : null);
	const pct = $derived(progress?.fraction == null ? null : Math.round(progress.fraction * 100));
	const canStart = $derived(!!asset && (status !== null || statusError !== null) && !running && !editor.busy);

	async function start() {
		if (!canStart || !asset) return;
		running = true;
		cancelling = false;
		progress = null;
		const name = asset.name;
		const unlisten = await onStemsProgress((p) => {
			progress = p;
		});
		try {
			const placed = await editor.separateStems(assetId, clipId ?? undefined);
			toast.success(
				describeStems(name, placed),
				clipId ? { action: { label: 'Undo', onClick: () => void editor.undo() } } : undefined
			);
			onClose();
		} catch (e) {
			// A stop is the user's own doing — back to the form, no notice.
			if (!isStemsCancelled(e)) toast.error(failure('Stem separation failed', e));
		} finally {
			unlisten();
			running = false;
			cancelling = false;
			progress = null;
		}
	}

	async function stop() {
		cancelling = true;
		await cancelStems();
	}

	const labelCss = 'font-size:12px;color:var(--text-muted)';
	const boxCss =
		'font-size:12px;line-height:1.5;color:var(--text-secondary);background:var(--surface-inset);border-radius:var(--radius-sm);padding:8px 10px';
</script>

<Modal title="Separate stems" icon="layers" width={480} busy={running} rescue {onClose}>
	<div style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
		<div style="display:flex;flex-direction:column;gap:2px">
			<span
				style="font-size:13px;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				use:tip={asset?.name ?? ''}>{asset?.name ?? ''}</span
			>
			{#if placement}
				<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)"
					>clip at {formatTime(placement.clip.timeline_start)} on {placement.track.name}</span
				>
			{/if}
		</div>

		<p style="margin:0;{labelCss};line-height:1.5">
			Splits the sound into {STEM_NAMES.slice(0, -1).join(', ')} and {STEM_NAMES[STEM_NAMES.length - 1]} on this machine, with
			the Demucs model. It can take about as long as the audio, or longer. Speech lands in vocals, so it also pulls dialogue
			apart from what is behind it.
		</p>

		<div style={boxCss}>
			{#if placement}
				The four stems go on new tracks ({STEM_NAMES.map(capitalized).join(', ')}) under the clip, with its position and
				length, and the clip's own sound is switched off so nothing is heard twice. Undo puts it back.
			{:else}
				The four stems are added to the library as “{asset?.name ?? ''} · {STEM_NAMES[0]}” and so on. Nothing on the
				timeline changes.
			{/if}
		</div>

		{#if note}
			<div style={boxCss}>{note}</div>
		{:else if statusError}
			<div
				style="font-size:12px;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:8px 10px"
			>
				⚠ Couldn't check what is already downloaded — {statusError}
			</div>
		{/if}
	</div>
	{#snippet footer()}
		<div style="padding:12px 16px">
			{#if running}
				<div style="display:flex;align-items:center;gap:10px">
					<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:6px" aria-live="polite">
						<div style="display:flex;align-items:baseline;gap:8px;font-size:12px">
							<span style="color:var(--text-primary)">{progress ? stageLabel(progress.stage) : 'Starting…'}</span>
							<span
								style="flex:1;min-width:0;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
								>{progress?.detail ?? ''}</span
							>
							{#if pct !== null}
								<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{pct}%</span>
							{/if}
						</div>
						<ProgressBar value={progress?.fraction ?? null} label="Separating stems" />
					</div>
					<Btn variant="destructive" size="md" data-rescue disabled={cancelling} onclick={stop}>
						{cancelling ? 'Stopping…' : 'Cancel'}
					</Btn>
				</div>
			{:else}
				<div style="display:flex;align-items:center;gap:8px">
					<div style="flex:1"></div>
					<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
					<Btn variant="primary" size="md" icon="layers" data-rescue disabled={!canStart} onclick={start}>Separate stems</Btn>
				</div>
			{/if}
		</div>
	{/snippet}
</Modal>
