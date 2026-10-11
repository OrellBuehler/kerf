<script lang="ts">
	// About Kerf: what is running, on what, under which licence — the lines a bug report needs, with a
	// button that copies them. (It used to be a four-second toast whose body was its invisible
	// description.)
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import KerfMark from './KerfMark.svelte';
	import { updater } from '$lib/updater.svelte';
	import { openReleases } from '$lib/api';
	import { failure } from '$lib/failure-copy';
	import { toast } from '$lib/notifications.svelte';
	import { aboutRows, platformName, REPOSITORY, TAGLINE, versionInfo, type AboutInfo } from '$lib/about';

	let { onClose }: { onClose: () => void } = $props();

	const info = $derived<AboutInfo>({
		version: updater.version,
		platform: platformName(typeof navigator === 'undefined' ? undefined : navigator),
		development: import.meta.env.DEV
	});

	/** A failure here is this dialog's, said in a toast — not the update dialog's error box. */
	async function openNotes() {
		try {
			await openReleases();
		} catch (e) {
			toast.error(failure("Couldn't open the release notes", e));
		}
	}

	async function copy() {
		try {
			await navigator.clipboard.writeText(versionInfo(info));
			toast.success('Version info copied');
		} catch {
			toast.error("Couldn't copy — select the text and copy it by hand");
		}
	}
</script>

<Modal title="About Kerf" icon="info" width={400} rescue initial="[data-copy]" {onClose}>
	<div style="padding:18px 20px 6px;display:flex;flex-direction:column;align-items:center;gap:6px;text-align:center">
		<KerfMark size={30} />
		<div style="font:var(--type-ui);font-size:var(--text-md);font-weight:600;color:var(--text-primary)">Kerf</div>
		<div style="font-size:12px;color:var(--text-secondary)">{TAGLINE}</div>
	</div>
	<dl
		data-selectable
		style="margin:0;padding:10px 20px 14px;display:grid;grid-template-columns:auto 1fr;gap:6px 16px;font-size:12px"
	>
		{#each aboutRows(info) as row (row.label)}
			<dt style="color:var(--text-muted)">{row.label}</dt>
			<dd style="margin:0;color:var(--text-primary);font-family:var(--font-mono)">{row.value}</dd>
		{/each}
		<dt style="color:var(--text-muted)">Source</dt>
		<dd style="margin:0;color:var(--text-primary);font-family:var(--font-mono);overflow-wrap:anywhere">{REPOSITORY}</dd>
	</dl>
	{#snippet footer()}
		<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
			<Btn variant="ghost" size="sm" icon="external-link" onclick={openNotes}>Release notes</Btn>
			<div style="flex:1"></div>
			<Btn variant="secondary" size="md" icon="copy" data-copy data-rescue onclick={copy}>Copy version info</Btn>
			<Btn variant="primary" size="md" data-rescue onclick={onClose}>Close</Btn>
		</div>
	{/snippet}
</Modal>
