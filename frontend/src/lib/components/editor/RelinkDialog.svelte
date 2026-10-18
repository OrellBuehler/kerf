<script lang="ts">
	// Media whose file is gone — moved, renamed, on a drive that is not mounted. Each row can be pointed at the file
	// it has become (`relink_asset`: same kind of media, about the same length; the clips and the place in the library
	// stay) or taken out of the project. The dialog opens by itself when a project comes up with some missing, and
	// the bin's badge and the Inspector's callout reach it too.
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '#lib/state.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { relinkMediaFlow, removeMediaFlow } from '#lib/library-actions';
	import { relinkIntro } from '#lib/library-copy';
	import { shortPath } from '#lib/media-info';
	import { tip } from '#lib/tip';

	let { onClose }: { onClose: () => void } = $props();

	const list = $derived(mediaStatus.missingList);
	let working = $state<string | null>(null);

	async function relink(id: string) {
		const asset = editor.assets.find((a) => a.id === id);
		if (!asset) return;
		working = id;
		try {
			await relinkMediaFlow(asset);
		} finally {
			working = null;
		}
	}

	async function remove(id: string) {
		const asset = editor.assets.find((a) => a.id === id);
		if (!asset) return;
		working = id;
		try {
			await removeMediaFlow(asset);
		} finally {
			working = null;
		}
	}
</script>

<Modal title="Relink media" icon="link" width={560} initial="[data-rescue]" rescue {onClose}>
	<div style="padding:14px 16px 6px">
		{#if list.length === 0}
			<div data-testid="relink-done" style="display:flex;align-items:center;gap:8px;font-size:13px;color:var(--text-primary)">
				<Icon n="circle-check" s={15} color="var(--green-400)" />
				Every file is where the project expects it.
			</div>
		{:else}
			<p style="margin:0 0 10px;font-size:12px;line-height:1.55;color:var(--text-secondary)">{relinkIntro(list)}</p>
			<ul data-testid="relink-list" style="margin:0;padding:0;list-style:none;display:flex;flex-direction:column;gap:8px">
				{#each list as m (m.asset_id)}
					<li
						style="padding:9px 10px;border-radius:var(--radius-sm);background:var(--surface-raised);border:var(--line-width) solid var(--border-default)"
					>
						<div style="display:flex;align-items:baseline;gap:8px">
							<span style="font-size:13px;font-weight:500;color:var(--text-primary);min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{m.name}</span>
							<span style="font-size:11px;color:var(--text-muted);white-space:nowrap">
								{m.clips === 0 ? 'not on the timeline' : `used by ${m.clips} ${m.clips === 1 ? 'clip' : 'clips'}`}
							</span>
						</div>
						<div
							use:tip={m.path}
							style="margin-top:2px;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
						>
							{shortPath(m.path)}
						</div>
						<div style="margin-top:8px;display:flex;gap:6px">
							<Btn variant="primary" size="sm" icon="folder-open" pending={working !== null} data-rescue={m === list[0] ? '' : undefined} aria-label="Locate {m.name}…" data-testid="relink-locate" onclick={() => relink(m.asset_id)}
								>Locate…</Btn
							>
							<Btn variant="ghost" size="sm" icon="trash" pending={working !== null} aria-label="Remove {m.name} from project…" data-testid="relink-remove" onclick={() => remove(m.asset_id)}>Remove from project…</Btn>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
	{#snippet footer()}
		<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
			<span style="flex:1;font-size:12px;color:var(--text-muted)">
				{list.length === 0 ? '' : 'You can come back from the library: a missing file is marked there.'}
			</span>
			<Btn variant={list.length === 0 ? 'primary' : 'ghost'} size="md" data-rescue={list.length === 0 ? '' : undefined} onclick={onClose}>
				{list.length === 0 ? 'Done' : 'Later'}
			</Btn>
		</div>
	{/snippet}
</Modal>
