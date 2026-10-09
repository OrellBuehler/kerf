<script lang="ts">
	// Split an asset's sound into drums, bass, other and vocals. Opened from the library (the asset
	// alone: the four stems join the bin) or from a clip (they are also laid under it on new tracks and
	// the clip's own sound is switched off). The first run downloads the runtime and the model, so the
	// dialog says so up front, then names each stage as it runs — the voiceover dialog's arrangement.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { trapFocus } from '$lib/modal';
	import { editor } from '$lib/state.svelte';
	import { cancelStems, onStemsProgress, stemsStatus } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { formatTime } from '$lib/diff';
	import type { StemsProgress, StemsStatus } from '$lib/types';
	import { STEM_NAMES, capitalized, describeStems, downloadNote, isStemsCancelled, stageLabel } from '$lib/stems';

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
			if (!isStemsCancelled(e)) toast.error(e instanceof Error ? e.message : String(e));
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

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	use:trapFocus
	role="dialog"
	aria-modal="true"
	aria-label="Separate stems"
	tabindex="-1"
	onclick={() => !running && onClose()}
	onkeydown={(e) => {
		if (e.key === 'Escape' && !running) onClose();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:color-mix(in srgb,var(--scrim) 55%,transparent);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:480px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg);overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:var(--line-width) solid var(--border-default)"
		>
			<Icon n="layers" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">Separate stems</span>
			{#if !running}
				<Btn variant="ghost" size="sm" aria-label="Close" onclick={onClose}>✕</Btn>
			{/if}
		</div>

		<div style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
			<div style="display:flex;flex-direction:column;gap:2px">
				<span
					style="font-size:13px;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					title={asset?.name ?? ''}>{asset?.name ?? ''}</span
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

		<div style="flex:none;border-top:var(--line-width) solid var(--border-default);padding:12px 16px">
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
						<div
							role="progressbar"
							aria-label="Separating stems"
							aria-valuemin="0"
							aria-valuemax="100"
							aria-valuenow={pct ?? undefined}
							style="height:6px;border-radius:3px;background:var(--surface-inset);overflow:hidden"
						>
							<div
								style="height:100%;width:{pct ?? 30}%;background:var(--kerf-500);transition:width var(--dur-fast) linear"
							></div>
						</div>
					</div>
					<Btn variant="destructive" size="md" disabled={cancelling} onclick={stop}>
						{cancelling ? 'Stopping…' : 'Cancel'}
					</Btn>
				</div>
			{:else}
				<div style="display:flex;align-items:center;gap:8px">
					<div style="flex:1"></div>
					<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
					<Btn variant="primary" size="md" icon="layers" disabled={!canStart} onclick={start}>Separate</Btn>
				</div>
			{/if}
		</div>
	</div>
</div>
