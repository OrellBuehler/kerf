<script lang="ts">
	// Keep an animated title in the library as a motion template. The author names it and says which parts
	// of the artwork are controls: a tagline, a brand color, a logo. The engine does the saving (fonts, the
	// poster, the checks against the artwork); this asks for what it needs and says what is wrong before it is
	// asked. A title that is itself from a template starts with that template's controls.
	import { tick } from 'svelte';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import TemplateControlEditor from './TemplateControlEditor.svelte';
	import { editor } from '$lib/state.svelte';
	import { svgElementIds } from '$lib/api';
	import { errorMessage } from '$lib/ops';
	import { tidyReason } from '$lib/failure-copy';
	import { svgDisplayName } from '$lib/svg-titles';
	import { saveTitleAsTemplate } from '$lib/template-actions';
	import {
		MAX_CONTROLS,
		checkDraft,
		draftFrom,
		newDraft,
		toControl,
		type DraftContext,
		type DraftControl
	} from '$lib/template-ui';
	import type { SvgElementId } from '$lib/types';

	let { overlayId, onClose }: { overlayId: string; onClose: () => void } = $props();

	const title = $derived(editor.svgOverlays.find((o) => o.id === overlayId));
	// The fields start from the title as it is when the dialog opens.
	const start = editor.svgOverlays.find((o) => o.id === overlayId);
	let name = $state(start ? svgDisplayName(start) : '');
	let description = $state('');
	let drafts = $state<DraftControl[]>([]);
	let elements = $state<SvgElementId[] | null>(null);
	let elementsError = $state<string | null>(null);
	let saving = $state(false);
	let refused = $state<string | null>(null);
	let form = $state<HTMLFormElement | null>(null);
	let keys = 0;

	// The title going away under the dialog (an undo, an agent's edit) ends it.
	$effect(() => {
		if (!title) onClose();
	});

	// The ids a control can aim at, asked of the engine for the artwork as it is now. A control a template title
	// already has comes along the first time.
	let seeded = false;
	$effect(() => {
		const svg = title?.svg;
		if (svg === undefined) return;
		let live = true;
		svgElementIds(svg).then(
			(found) => {
				if (!live) return;
				elements = found;
				elementsError = null;
				if (!seeded) {
					seeded = true;
					drafts = (start?.template?.controls ?? []).map((c) => draftFrom(c, ++keys));
				}
			},
			(e) => {
				if (!live) return;
				elements = [];
				elementsError = tidyReason(errorMessage(e));
			}
		);
		return () => {
			live = false;
		};
	});

	const ctx = $derived<DraftContext>({ svg: title?.svg ?? '', elements: elements ?? [] });
	const check = $derived(checkDraft(name, description, drafts, ctx));
	const problem = $derived(check.name ?? check.description ?? check.controls.find((c) => c !== null) ?? null);
	const loaded = $derived(elements !== null);
	const nothingToAimAt = $derived(loaded && elements!.length === 0);

	async function add() {
		const d = newDraft(++keys, ctx);
		drafts = [...drafts, d];
		await tick();
		form?.querySelector<HTMLElement>(`[data-control-key="${d.key}"] [data-label]`)?.focus();
	}

	async function save(e?: SubmitEvent) {
		e?.preventDefault();
		if (!title || !check.ok || saving || !loaded) return;
		saving = true;
		refused = null;
		try {
			await saveTitleAsTemplate(title.id, name, description, drafts.map(toControl));
			onClose();
		} catch (err) {
			refused = tidyReason(errorMessage(err));
		} finally {
			saving = false;
		}
	}

	const field =
		'box-sizing:border-box;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm);min-width:0';
	const label = 'font-size:12px;color:var(--text-muted)';
</script>

<Modal title="Save as template" icon="layout-template" width={580} busy={saving} dismissOnScrim={false} rescue initial="[data-name]" {onClose}>
	<form
		bind:this={form}
		id="save-template-form"
		onsubmit={save}
		style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px"
	>
		<p style="margin:0;font-size:12px;line-height:1.5;color:var(--text-muted)">
			Keep this title in your templates so you can add it to any project. Name the parts that should change each time — a tagline, a
			brand color, a logo — and everything else stays as drawn. A project keeps its own copy of the artwork and fonts.
		</p>
		<label style="display:grid;grid-template-columns:78px minmax(0,1fr);align-items:center;column-gap:8px">
			<span style={label}>Name</span>
			<input
				data-name
				type="text"
				bind:value={name}
				placeholder={start ? svgDisplayName(start) : ''}
				autocomplete="off"
				style="{field};height:var(--control-sm);padding:0 8px"
			/>
		</label>
		<label style="display:grid;grid-template-columns:78px minmax(0,1fr);align-items:start;column-gap:8px">
			<span style="{label};padding-top:7px">Description</span>
			<textarea
				bind:value={description}
				rows="2"
				placeholder="Optional: when to use it"
				style="{field};padding:6px 8px;resize:vertical;font-family:inherit"
			></textarea>
		</label>

		<div style="display:flex;align-items:center;gap:8px;margin-top:4px">
			<span class="k-overline" style="flex:1">Controls · {drafts.length}</span>
			<Btn
				size="sm"
				icon="plus"
				iconSize={12}
				disabled={!loaded || nothingToAimAt || drafts.length >= MAX_CONTROLS}
				onclick={() => void add()}>Add control</Btn
			>
		</div>
		{#if elementsError}
			<div role="alert" style="font-size:12px;line-height:1.4;color:var(--orange-400)">
				Couldn't read the elements of this title — {elementsError}
			</div>
		{:else if nothingToAimAt}
			<div style="font-size:12px;line-height:1.5;color:var(--text-muted)">
				None of this title's elements has an id, so there is nothing for a control to change. Give the ones you want to change an id
				in the SVG source (for example <span style="font-family:var(--font-mono)">id="tagline"</span>) and save again — or save it
				now as artwork only.
			</div>
		{:else if loaded && drafts.length === 0}
			<div style="font-size:12px;line-height:1.5;color:var(--text-muted)">
				No controls yet: the template can be added but not changed. Use Add control to name a part of it.
			</div>
		{/if}
		{#each drafts as d, i (d.key)}
			<TemplateControlEditor
				draft={d}
				index={i}
				others={drafts.filter((o) => o.key !== d.key)}
				{ctx}
				problem={check.controls[i] ?? null}
				onchange={(next) => (drafts[i] = next)}
				onremove={() => (drafts = drafts.filter((o) => o.key !== d.key))}
			/>
		{/each}
		{#if refused}
			<div
				role="alert"
				data-save-error
				style="font-size:12px;line-height:1.4;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:6px 8px"
			>
				{refused}
			</div>
		{/if}
	</form>
	{#snippet footer()}
		<div style="padding:12px 16px;display:flex;align-items:center;gap:8px">
			<span style="flex:1;min-width:0;font-size:12px;line-height:1.4;color:var(--text-muted)" data-save-hint>
				{#if problem}{problem}{:else if saving}Saving: fonts, the poster and the checks take a moment…{/if}
			</span>
			<Btn variant="ghost" size="md" disabled={saving} onclick={onClose}>Cancel</Btn>
			<Btn
				variant="primary"
				size="md"
				icon="layout-template"
				data-rescue
				type="submit"
				form="save-template-form"
				disabled={!check.ok || saving || !loaded}>Save template</Btn
			>
		</div>
	{/snippet}
</Modal>
