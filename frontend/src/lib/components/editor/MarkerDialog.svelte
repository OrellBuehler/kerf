<script lang="ts">
	// Edit a marker's name, kind, length and comment as one edit. A `chapter` marker starts a
	// chapter of the exported file and of the YouTube chapter text; a comment is a note on the cut.
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { toast } from '$lib/notifications.svelte';
	import { failure } from '$lib/failure-copy';
	import { markerPatch } from '$lib/marker-edit';
	import type { MarkerKind } from '$lib/types';

	let { markerId, onClose }: { markerId: string; onClose: () => void } = $props();

	const marker = $derived(editor.markers.find((m) => m.id === markerId));

	// The fields start from the marker as it is when the dialog opens.
	const start = editor.markers.find((m) => m.id === markerId);
	let name = $state(start?.name ?? '');
	let kind = $state<MarkerKind>(start?.kind ?? 'comment');
	let durationText = $state(start?.duration ? String(start.duration) : '0');
	let comment = $state(start?.comment ?? '');
	let saving = $state(false);

	// The marker going away under the dialog (an undo, an agent's edit) ends it.
	$effect(() => {
		if (!marker) onClose();
	});

	const result = $derived(marker ? markerPatch(marker, { name, kind, durationText, comment }) : null);

	async function save(e?: SubmitEvent) {
		e?.preventDefault();
		if (!marker || !result || 'error' in result) return;
		if (Object.keys(result.patch).length === 0) return onClose();
		saving = true;
		try {
			await editor.updateMarker(marker.id, result.patch);
			onClose();
		} catch (err) {
			toast.error(failure("Couldn't update the marker", err));
		} finally {
			saving = false;
		}
	}

	const chip = (active: boolean) =>
		`padding:4px 9px;font-size:12px;cursor:pointer;border-radius:var(--radius-sm);border:var(--line-width) solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
			active ? 'var(--text-primary)' : 'var(--text-secondary)'
		}`;
	const labelCss = 'font-size:12px;color:var(--text-muted);width:70px;flex:none';
	const inputCss =
		'flex:1;min-width:0;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:13px;min-height:30px;padding:4px 7px';
</script>

<Modal title="Edit marker" icon="bookmark" width={420} busy={saving} dismissOnScrim={false} rescue initial="[data-name]" {onClose}>
	<form
		id="marker-form"
		onsubmit={save}
		style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px"
	>
		<label style="display:flex;align-items:center;gap:8px">
			<span style={labelCss}>Name</span>
			<input data-name type="text" bind:value={name} style={inputCss} />
		</label>
		<div style="display:flex;align-items:center;gap:5px">
			<span style={labelCss}>Kind</span>
			<button type="button" style={chip(kind === 'comment')} aria-pressed={kind === 'comment'} onclick={() => (kind = 'comment')}>
				Comment
			</button>
			<button type="button" style={chip(kind === 'chapter')} aria-pressed={kind === 'chapter'} onclick={() => (kind = 'chapter')}>
				Chapter
			</button>
		</div>
		<label style="display:flex;align-items:center;gap:8px">
			<span style={labelCss}>Length</span>
			<input type="text" inputmode="decimal" aria-label="Length in seconds" bind:value={durationText} style="{inputCss};font-family:var(--font-mono);text-align:right;max-width:90px" />
			<span style="font-size:12px;color:var(--text-muted)">s</span>
		</label>
		<label style="display:flex;align-items:flex-start;gap:8px">
			<span style="{labelCss};padding-top:6px">Comment</span>
			<textarea bind:value={comment} rows="4" style="{inputCss};resize:vertical;font-family:inherit"></textarea>
		</label>
		{#if result && 'error' in result}
			<span style="font-size:12px;color:var(--warning)">{result.error}</span>
		{/if}
		<p style="margin:0;font-size:11px;line-height:1.5;color:var(--text-muted)">
			A chapter marker starts a chapter of the exported file and of the YouTube chapter list.
		</p>
	</form>
	{#snippet footer()}
		<div style="padding:12px 16px;display:flex;align-items:center;gap:8px">
			<div style="flex:1"></div>
			<Btn variant="ghost" size="md" disabled={saving} onclick={onClose}>Cancel</Btn>
			<Btn variant="primary" size="md" icon="bookmark" data-rescue onclick={() => void save()} disabled={!result || 'error' in result || saving}>
				Save
			</Btn>
		</div>
	{/snippet}
</Modal>
