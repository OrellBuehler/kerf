<script lang="ts">
	// The Preview with nothing on the timeline: three steps and where the project stands in them, and the
	// agent as the other way in. It was a clapperboard icon and "No media loaded".
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { settings } from '$lib/settings.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { importMedia } from '$lib/import-media';
	import { firstRunAction, firstRunSteps } from '$lib/first-run';
	import { toast } from '$lib/notifications.svelte';
	import { report } from '$lib/ops';
	import { placementOut } from '$lib/editing-prefs';
	import { clipsIn, exportButtonState } from '$lib/export-button';

	const steps = $derived(
		firstRunSteps({
			assets: editor.assets.length,
			clips: editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0),
			importKeys: settings.shortcut('file.import'),
			exportKeys: settings.shortcut('file.export')
		})
	);
	const action = $derived(
		firstRunAction({ assets: editor.assets, selectedId: editor.selectedAssetId, libraryOpen: workspace.open.includes('library') })
	);
	const target = $derived(action.kind === 'add' ? editor.assets.find((a) => a.id === action.assetId) : undefined);

	/** Put the picked media on the timeline at the start, and say what went where. */
	async function addSelected() {
		const a = target;
		if (!a) return;
		try {
			await editor.add(a.id, 0, placementOut(a, settings.editing.still_seconds), undefined, 0);
			toast.success(`Added ${a.name} to the timeline`);
		} catch (e) {
			report(e);
		}
	}
	const exportState = $derived(
		exportButtonState({ clips: clipsIn(editor.liveTimeline), running: editor.exportRun !== null })
	);
</script>

<!-- It fills the pane (the pane is a size container, so the queries below are about *its* height) and the
     card scrolls inside it rather than spilling over the transport bar: at 960×640 the pane is under 300 px. -->
<div class="pane">
<div
	class="card"
	data-testid="first-run"
	style="width:min(380px,100%);box-sizing:border-box;max-height:100%;overflow:auto;border-radius:var(--radius-lg);background:var(--surface-raised);border:var(--line-width) solid var(--border-default);color:var(--text-secondary)"
>
	<div class="head">
		<Icon n="clapperboard" s={18} color="var(--text-muted)" />
		<span style="font-size:13px;font-weight:600;color:var(--text-primary)">Start a cut</span>
	</div>
	<ol class="steps">
		{#each steps as s, i (s.id)}
			<li style="display:flex;gap:10px;align-items:flex-start" aria-current={s.state === 'current' ? 'step' : undefined}>
				<span
					aria-hidden="true"
					style="flex:none;width:20px;height:20px;border-radius:50%;box-sizing:border-box;display:grid;place-items:center;font-family:var(--font-mono);font-size:11px;font-weight:600;border:var(--line-width) solid {s.state ===
					'done'
						? 'var(--green-400)'
						: s.state === 'current'
							? 'var(--kerf-500)'
							: 'var(--border-strong)'};color:{s.state === 'done'
						? 'var(--green-400)'
						: s.state === 'current'
							? 'var(--text-primary)'
							: 'var(--text-muted)'};background:{s.state === 'current'
						? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)'
						: 'transparent'}"
				>
					{#if s.state === 'done'}<Icon n="check" s={11} />{:else}{i + 1}{/if}
				</span>
				<span style="min-width:0">
					<span style="display:block;font-size:12px;font-weight:600;color:{s.state === 'todo' ? 'var(--text-secondary)' : 'var(--text-primary)'}"
						>{s.title}{#if s.state === 'done'}<span class="sr-only"> (done)</span>{/if}</span
					>
					<span class="body" style="display:block;margin-top:2px;font-size:12px;line-height:1.45;color:var(--text-muted)">{s.body}</span>
				</span>
			</li>
		{/each}
	</ol>
	<div class="actions">
		{#if action.kind === 'import'}
			<Btn variant="primary" size="sm" icon="plus" onclick={importMedia}>{action.label}</Btn>
		{:else if action.kind === 'add'}
			<Btn
				variant="primary"
				size="sm"
				icon="plus"
				pending={editor.busy}
				title={target ? `Put ${target.name} at the start of the timeline` : undefined}
				onclick={addSelected}>{action.label}</Btn
			>
		{:else}
			<Btn size="sm" icon="film" onclick={() => workspace.show('library')}>{action.label}</Btn>
		{/if}
		<Btn
			size="sm"
			icon="upload"
			aria-disabled={exportState.disabled ? 'true' : undefined}
			title={exportState.reason}
			onclick={() => {
				if (!exportState.disabled) ui.openExport();
			}}>Export…</Btn
		>
	</div>
	<div class="agent" style="border-top:var(--line-width) solid var(--border-subtle)">
		<span style="flex:1;font-size:12px;line-height:1.45;color:var(--text-muted)"
			>Or connect an agent over MCP and let it assemble a first cut.</span
		>
		<Btn size="sm" icon="plug" onclick={() => workspace.show('agent')}>Connect an agent</Btn>
	</div>
</div>
</div>

<style>
	.pane {
		position: absolute;
		inset: 0;
		box-sizing: border-box;
		padding: 20px;
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		grid-template-rows: minmax(0, 1fr);
		place-items: center;
	}
	.card {
		--pad: 18px 20px;
		--head-gap: 12px;
		--steps-gap: 11px;
		--rows-gap: 14px;
		--agent-pad: 12px;
		padding: var(--pad);
	}
	.head {
		display: flex;
		align-items: center;
		gap: 9px;
		margin-bottom: var(--head-gap);
	}
	.steps {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--steps-gap);
	}
	.actions {
		display: flex;
		gap: 6px;
		flex-wrap: wrap;
		margin-top: var(--rows-gap);
	}
	.agent {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: var(--rows-gap);
		padding-top: var(--agent-pad);
	}
	/* The pane is a size container, and these are about its height (its content box: the pane less its padding).
	   The card is 340 px tall in full. Below 345 its spacing tightens (1366×768 has 326); below 310 the lines under
	   each step go (1280×720 has 296); below 260 the agent row goes too (960×640 has 247). The three steps, their
	   buttons and the title always fit; anything still taller scrolls inside the card instead of over the transport. */
	@container (max-height: 344px) {
		.card {
			--pad: 12px 16px;
			--head-gap: 8px;
			--steps-gap: 8px;
			--rows-gap: 10px;
			--agent-pad: 8px;
		}
	}
	@container (max-height: 309px) {
		.body {
			display: none !important;
		}
	}
	@container (max-height: 259px) {
		.agent {
			display: none !important;
		}
	}
</style>
