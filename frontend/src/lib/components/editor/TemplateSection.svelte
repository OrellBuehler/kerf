<script lang="ts">
	// The Inspector's Template section for an animated SVG title, floating or on a track. A title that came from a template shows one field
	// per control — text, color, number, font, image — and each committed change is one `set_template_values` edit: a
	// text, color or font field on change, a number on blur or Enter (every step of its arrows would be an edit), never
	// per keystroke. A change made while another edit runs keeps what was typed and is made when that edit is done.
	// A custom title instead offers to be kept as a template. The
	// artwork stays the engine's: nothing here edits the SVG, and a picture is shown only as an <img> data URL.
	import { untrack } from 'svelte';
	import InspectorSection from './InspectorSection.svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { FieldEdits } from '#lib/field-edit';
	import { cssColorHex } from '#lib/css-color';
	import { holdForPreview, report } from '#lib/ops';
	import { setTemplateImage, setTemplateValue } from '#lib/template-actions';
	import {
		controlCount,
		elementAttributes,
		imageHref,
		normalizeValue,
		numberStep,
		pickColor,
		swatchOf,
		targetsLine,
		valueRefusal
	} from '#lib/template-ui';
	import { swatchHex } from '#lib/title-color';
	import { tip } from '#lib/tip';
	import type { TemplateControl, TemplateInstance } from '#lib/types';

	/** A title, floating or on a track: its id, artwork and template. */
	let { o }: { o: { id: string; svg: string; template?: TemplateInstance | null } } = $props();

	const template = $derived(o.template);
	let choosing = $state<string | null>(null);

	/** What a field is editing: the title when it took focus, and what was typed (see `FieldEdits`: the click that
	 *  blurs a field is often the one that selects another title). */
	const edits = new FieldEdits<string>();
	const noteFocus = (e: { currentTarget: HTMLElement }) => edits.focus(e.currentTarget, o.id);
	const noteTyped = (e: { currentTarget: HTMLInputElement }) => edits.input(e.currentTarget, e.currentTarget.value);

	const off = $derived(editor.locked ? { readonly: true, 'aria-disabled': true } : {});
	const swallow = (e: Event) => {
		if (editor.locked) e.preventDefault();
	};

	const valueOf = (c: TemplateControl): string => template?.values[c.id] ?? '';
	/** The control's element is no longer in the artwork (the source was edited): there is nothing to change. */
	const gone = (c: TemplateControl): boolean =>
		c.kind === 'image'
			? !elementAttributes(o.svg, c.targets[0]?.element ?? '')
			: !!template && template.values[c.id] === undefined;
	const swatch = (value: string): string => swatchOf(value) ?? cssColorHex(value) ?? swatchHex(value);

	/** A change that came in while another edit was running: the field keeps what was typed and the change is made when
	 *  that edit has finished (`editor.busy` ends), one at a time. */
	interface Change {
		el: HTMLInputElement | HTMLSelectElement;
		/** The title the field took focus on. */
		id: string;
		control: string;
		typed: string;
		make: (typed: string, was: string) => string;
	}
	let held = $state.raw<Change[]>([]);

	$effect(() => {
		if (editor.busy || held.length === 0) return;
		const [next, ...rest] = held;
		held = rest;
		untrack(() => void commit(next));
	});

	/** What a field shows for a control now: the title's value (a color picker, its swatch). */
	function shownNow(el: HTMLInputElement | HTMLSelectElement, id: string, control: string): string {
		const now = editor.titleById(id)?.template?.values[control] ?? '';
		return el.type === 'color' ? swatch(now) : now;
	}

	/** A field committed. `make` turns the field's text into the value (a color pick keeps the alpha the value had). */
	function write(
		el: HTMLInputElement | HTMLSelectElement,
		c: TemplateControl,
		make: (typed: string, was: string) => string = (typed) => typed
	) {
		const { scope: id, text } = edits.end(el, { scope: o.id, text: el.value });
		return commit({ el, id, control: c.id, typed: text, make });
	}

	async function commit({ el, id, control: controlId, typed, make }: Change) {
		const back = () => {
			if (el.isConnected) el.value = shownNow(el, id, controlId);
		};
		const title = editor.titleById(id);
		const control = title?.template?.controls.find((x) => x.id === controlId);
		if (!title || !control) return;
		const was = title.template!.values[control.id] ?? '';
		const next = make(typed, was);
		const unchanged =
			valueRefusal(control, next) === null &&
			(control.kind === 'text' ? next === was : normalizeValue(control, next) === normalizeValue(control, was));
		if (unchanged) return back();
		// The proposal on screen: nothing can be written, and the field says so instead of just going back.
		if (holdForPreview()) return back();
		// An edit in flight: what was typed stays in the field until that edit is done.
		if (editor.busy) {
			held = [...held.filter((h) => h.el !== el), { el, id, control: controlId, typed, make }];
			return;
		}
		try {
			await setTemplateValue(id, control, next);
		} catch (e) {
			report(e);
			back();
		}
	}

	async function chooseImage(c: TemplateControl) {
		const id = o.id;
		choosing = c.id;
		try {
			await setTemplateImage(id, c);
		} catch (e) {
			report(e);
		} finally {
			choosing = null;
		}
	}

	const field =
		'box-sizing:border-box;height:var(--control-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm);padding:0 8px;min-width:0';
	const caption = 'font-size:12px;color:var(--text-muted)';
	const fonts = $derived(ui.availableFonts);
</script>

<InspectorSection title="Template" summary={template ? controlCount(template.controls.length) : 'Not a template'} open>
	{#if template}
		<div data-template-name style="{caption};line-height:1.4;margin-bottom:6px">
			From the template <strong style="color:var(--text-secondary);font-weight:600">“{template.name}”</strong>. A change here
			edits this title only; your library keeps the template as it was.
		</div>
		{#each template.controls as c (c.id)}
			{@const missing = gone(c)}
			{@const value = valueOf(c)}
			<div data-control={c.id} data-kind={c.kind} style="display:flex;flex-direction:column;gap:3px;padding:4px 0">
				<div style="display:flex;align-items:baseline;gap:8px;min-width:0">
					<span use:tip={targetsLine(c)} style="{caption};flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
						>{c.label}</span
					>
					{#if c.kind === 'number' && (c.min !== undefined || c.max !== undefined)}
						<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);font-variant-numeric:tabular-nums"
							>{c.min ?? ''}–{c.max ?? ''}</span
						>
					{/if}
				</div>
				{#if missing}
					<div style="{caption};line-height:1.4">Not in this title's artwork any more, so there is nothing to change.</div>
				{:else if c.kind === 'text'}
					<input
						type="text"
						{value}
						{...off}
						aria-label={c.label}
						onfocus={noteFocus}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(e) => write(e.currentTarget, c)}
						style="{field};width:100%"
					/>
				{:else if c.kind === 'color'}
					<div style="display:flex;align-items:center;gap:6px;min-width:0">
						<input
							type="color"
							class="k-swatch"
							value={swatch(value)}
							data-empty={swatchOf(value) || cssColorHex(value) ? undefined : ''}
							aria-disabled={editor.locked || undefined}
							aria-label="{c.label}: pick a color"
							onclick={swallow}
							onfocus={noteFocus}
							onchange={(e) => write(e.currentTarget, c, (picked, was) => pickColor(was, picked))}
						/>
						<input
							type="text"
							{value}
							{...off}
							spellcheck="false"
							aria-label={c.label}
							onfocus={noteFocus}
							oninput={noteTyped}
							oncompositionend={noteTyped}
							onchange={(e) => write(e.currentTarget, c)}
							style="{field};flex:1;font-family:var(--font-mono)"
						/>
					</div>
				{:else if c.kind === 'number'}
					<input
						type="number"
						{value}
						min={c.min}
						max={c.max}
						step={numberStep(c)}
						{...off}
						aria-label={c.label}
						onfocus={noteFocus}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onblur={(e) => write(e.currentTarget, c)}
						onkeydown={(e) => e.key === 'Enter' && write(e.currentTarget, c)}
						style="{field};width:110px;font-family:var(--font-mono);text-align:right"
					/>
				{:else if c.kind === 'font'}
					<select
						class="k-select"
						{value}
						aria-label={c.label}
						aria-disabled={editor.locked || undefined}
						onmousedown={swallow}
						onfocus={noteFocus}
						onchange={(e) => write(e.currentTarget, c)}
					>
						{#each fonts as f (f)}
							<option value={f}>{f}</option>
						{/each}
						{#if value && !fonts.includes(value)}
							<option {value}>{value}</option>
						{/if}
					</select>
				{:else}
					{@const href = imageHref(o.svg, c)}
					<div style="display:flex;align-items:center;gap:8px">
						<span
							style="width:56px;height:34px;flex:none;display:grid;place-items:center;overflow:hidden;border-radius:var(--radius-sm);background:var(--surface-void);border:var(--line-width) solid var(--border-default);color:var(--text-muted)"
						>
							{#if href}
								<img src={href} alt="" draggable="false" style="width:100%;height:100%;object-fit:contain;display:block" />
							{:else}
								<Icon n="image" s={14} />
							{/if}
						</span>
						<Btn
							size="sm"
							icon="image"
							pending={editor.locked || choosing !== null}
							title="A PNG, JPEG, GIF or WebP picture, a little under 96 KB"
							onclick={() => void chooseImage(c)}>Choose image…</Btn
						>
					</div>
				{/if}
			</div>
		{/each}
		{#if template.controls.length === 0}
			<div style="{caption};line-height:1.4">This template has no controls: it can be added but not changed.</div>
		{/if}
		<div style="margin-top:8px">
			<Btn
				size="sm"
				icon="layout-template"
				pending={editor.locked}
				title="Keep this title in your templates, with the controls this one has or new ones"
				onclick={() => ui.openSaveTemplate(o.id)}>Save as template…</Btn
			>
		</div>
		<div style="{caption};line-height:1.4;margin-top:6px">
			Applying changes to the SVG source below ends the link to this template's controls.
		</div>
	{:else}
		<div style="{caption};line-height:1.4;margin-bottom:8px">
			Keep this title in your templates to add it to any project. You choose the parts that can change each time — a tagline, a
			brand color, a logo — and everything else stays as drawn.
		</div>
		<Btn size="sm" icon="layout-template" pending={editor.locked} onclick={() => ui.openSaveTemplate(o.id)}>Save as template…</Btn>
	{/if}
</InspectorSection>
