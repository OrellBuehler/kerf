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
	import { firstRunSteps } from '$lib/first-run';

	const steps = $derived(
		firstRunSteps({
			assets: editor.assets.length,
			clips: editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0),
			importKeys: settings.shortcut('file.import'),
			exportKeys: settings.shortcut('file.export')
		})
	);
</script>

<div
	data-testid="first-run"
	style="width:min(380px,100%);box-sizing:border-box;padding:18px 20px;border-radius:var(--radius-lg);background:var(--surface-raised);border:var(--line-width) solid var(--border-default);color:var(--text-secondary)"
>
	<div style="display:flex;align-items:center;gap:9px;margin-bottom:12px">
		<Icon n="clapperboard" s={18} color="var(--text-muted)" />
		<span style="font-size:13px;font-weight:600;color:var(--text-primary)">Start a cut</span>
	</div>
	<ol style="list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:11px">
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
					<span style="display:block;margin-top:2px;font-size:12px;line-height:1.45;color:var(--text-muted)">{s.body}</span>
				</span>
			</li>
		{/each}
	</ol>
	<div style="display:flex;gap:6px;flex-wrap:wrap;margin-top:14px">
		{#if editor.assets.length === 0}
			<Btn variant="primary" size="sm" icon="plus" onclick={importMedia}>Import media…</Btn>
		{:else}
			<Btn size="sm" icon="film" onclick={() => workspace.show('library')}>Open Library</Btn>
		{/if}
		<Btn size="sm" icon="upload" onclick={() => ui.openExport()}>Export…</Btn>
	</div>
	<div style="margin-top:14px;padding-top:12px;border-top:var(--line-width) solid var(--border-subtle);display:flex;align-items:center;gap:10px">
		<span style="flex:1;font-size:12px;line-height:1.45;color:var(--text-muted)"
			>Or connect an agent over MCP and let it assemble a first cut.</span
		>
		<Btn size="sm" icon="plug" onclick={() => workspace.show('agent')}>Connect an agent</Btn>
	</div>
</div>
