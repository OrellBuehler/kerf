<script lang="ts">
	// The Templates section of the Titles controls: the library of motion templates (an animated title kept with the
	// parts that can change — a tagline, a brand color, a logo), a card each, and the way in from a `.kerftemplate`
	// file. It reads the library when it opens and on `templates-changed` (a save, an import or a delete, the app's
	// own — announced by `api.ts` — or an agent's), so every mounted grid hears it. A file in the library that would
	// not read is listed, not hidden.
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import Icon from './Icon.svelte';
	import TemplateCard from './TemplateCard.svelte';
	import { listTemplates, onTemplatesChanged } from '$lib/api';
	import { editor } from '$lib/state.svelte';
	import { errorMessage } from '$lib/ops';
	import { tidyReason } from '$lib/failure-copy';
	import { forgetPoster, importTemplateFile } from '$lib/template-actions';
	import { svgFrame } from '$lib/svg-titles';
	import type { TemplateInfo, TemplateList } from '$lib/types';

	let list = $state<TemplateList | null>(null);
	let failed = $state<string | null>(null);
	let importing = $state(false);
	let asked = 0;

	/** Read the library; an answer that arrives after a newer question is dropped. */
	async function load() {
		const mine = ++asked;
		try {
			const next = await listTemplates();
			if (mine !== asked) return;
			// A template that left the library takes its poster out of the cache.
			for (const t of list?.templates ?? []) if (!next.templates.some((n) => n.id === t.id)) forgetPoster(t.id);
			list = next;
			failed = null;
		} catch (e) {
			if (mine === asked) failed = errorMessage(e);
		}
	}

	$effect(() => {
		let stop = () => {};
		let gone = false;
		void load();
		onTemplatesChanged(() => void load()).then(
			(off) => (gone ? off() : (stop = off)),
			() => {}
		);
		return () => {
			gone = true;
			stop();
		};
	});

	async function importFile() {
		if (importing) return;
		importing = true;
		try {
			await importTemplateFile();
		} finally {
			importing = false;
		}
	}

	const aspect = $derived(svgFrame(editor.timeline.format));
	const templates = $derived<TemplateInfo[]>(list?.templates ?? []);
	const problems = $derived(list?.problems ?? []);
</script>

<div style="display:flex;align-items:center;gap:6px;margin-bottom:6px">
	<span class="k-overline" style="flex:1;min-width:0">Templates</span>
	<Btn
		size="sm"
		variant="ghost"
		icon="download"
		iconSize={12}
		pending={importing}
		title="Add a .kerftemplate file to your templates"
		onclick={() => void importFile()}>Import…</Btn
	>
</div>
<div data-templates style="margin-bottom:14px">
	{#if failed}
		<div role="alert" style="font-size:12px;line-height:1.4;color:var(--text-muted)">
			Couldn't read your templates — {tidyReason(failed)}
			<Btn size="sm" variant="ghost" onclick={() => void load()}>Try again</Btn>
		</div>
	{:else if list && templates.length === 0}
		<div style="border:var(--line-width) dashed var(--border-strong);border-radius:var(--radius-sm)">
			<EmptyState
				icon="layout-template"
				title="No templates yet"
				hint="Add an animated SVG title, select it and choose Save as template… in the Inspector: name the parts that should change each time (a tagline, a brand color, a logo) and it lands here. Or import a .kerftemplate file."
				pad={14}
			/>
		</div>
	{:else if templates.length > 0}
		<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(104px,1fr));gap:6px">
			{#each templates as t (t.id)}
				<TemplateCard info={t} {aspect} />
			{/each}
		</div>
	{/if}
	{#if problems.length > 0}
		<details data-template-problems style="margin-top:8px;font-size:11px;line-height:1.4;color:var(--text-muted)">
			<summary style="cursor:pointer;display:flex;align-items:center;gap:5px;list-style:none">
				<Icon n="info" s={12} />
				{problems.length === 1 ? "1 file in your library couldn't be read" : `${problems.length} files in your library couldn't be read`}
			</summary>
			<ul style="margin:4px 0 0;padding:0 0 0 17px">
				{#each problems as p (p.file)}
					<li style="overflow-wrap:anywhere"><span style="font-family:var(--font-mono)">{p.file}</span> — {tidyReason(p.error)}</li>
				{/each}
			</ul>
		</details>
	{/if}
</div>
