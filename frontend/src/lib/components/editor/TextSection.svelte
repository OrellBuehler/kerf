<script lang="ts">
	// The Inspector's Text section for an animated SVG title, floating or on a track (not a preset, which is drawn from its
	// parameters). Each `<text>` with an id can be given a box: a width its lines wrap to, or that it shrinks to fit on one
	// line (`set_title_text_box`, one edit when a field commits, never per keystroke). A box is drawn only when the text is
	// glyph outlines, which a template title's always is; a custom title's text can be set as outlines or drawn with its
	// fonts again (`convert_title_text`). The artwork stays the engine's: nothing here draws it.
	import InspectorSection from './InspectorSection.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '#lib/state.svelte';
	import { holdForPreview, report } from '#lib/ops';
	import { say } from '#lib/template-actions';
	import { appliedLine } from '#lib/template-ui';
	import { boxWidthRefusal, isOutlined, titleTexts, type TitleText } from '#lib/title-text';
	import type { TemplateInstance, TextBox, TextFit, TitleTextForm } from '#lib/types';

	/** A title, floating or on a track: its id, name, artwork and template. */
	let { o }: { o: { id: string; name: string; svg: string; template?: TemplateInstance | null } } = $props();

	const texts = $derived(titleTexts(o.svg));
	const outlined = $derived(isOutlined(o.svg));
	let converting = $state(false);

	/** What a text is called here: the label of a template control that sets it, else its id. */
	const labelOf = (t: TitleText): string =>
		o.template?.controls.find((c) => c.kind === 'text' && c.targets.some((x) => x.element === t.id))?.label ?? `#${t.id}`;

	/** A box field committed: one edit when the box changed, else the field shows the box again. */
	async function commit(el: HTMLInputElement | HTMLSelectElement, t: TitleText, change: Partial<TextBox>) {
		const id = o.id;
		const name = o.name;
		const back = () => {
			if (!el.isConnected) return;
			el.value = el instanceof HTMLSelectElement ? t.box.fit : t.box.width === null ? '' : String(t.box.width);
		};
		const next: TextBox = { ...t.box, ...change };
		if (next.width === t.box.width && (next.fit === t.box.fit || next.width === null)) return back();
		if (editor.locked || editor.busy || holdForPreview()) return back();
		try {
			const edit = await editor.setTitleTextBox(id, t.id, next);
			const line = appliedLine(name, edit.warnings);
			if (line) say(line);
		} catch (e) {
			report(e);
			back();
		}
	}

	function commitWidth(el: HTMLInputElement, t: TitleText) {
		const why = boxWidthRefusal(el.value);
		if (why) {
			report(new Error(`${labelOf(t)}: ${why}`));
			el.value = t.box.width === null ? '' : String(t.box.width);
			return;
		}
		void commit(el, t, { width: el.value.trim() === '' ? null : Number(el.value) });
	}

	async function convert(form: TitleTextForm) {
		if (editor.locked || converting || holdForPreview()) return;
		converting = true;
		try {
			const edit = await editor.convertTitleText(o.id, form);
			const line = appliedLine(o.name, edit.warnings);
			if (line) say(line);
		} catch (e) {
			report(e);
		} finally {
			converting = false;
		}
	}

	const field =
		'box-sizing:border-box;height:var(--control-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm);padding:0 8px;min-width:0';
	const caption = 'font-size:12px;color:var(--text-muted);line-height:1.4';
	const FITS: { value: TextFit; label: string }[] = [
		{ value: 'wrap', label: 'Wrap lines' },
		{ value: 'shrink', label: 'Shrink to fit' }
	];
</script>

<InspectorSection title="Text" summary={outlined ? 'Outlines' : 'Fonts'}>
	{#each texts as t (t.id)}
		<div data-title-text={t.id} style="display:flex;flex-direction:column;gap:3px;padding:4px 0">
			<span style="{caption};overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{labelOf(t)}</span>
			<div style="display:flex;align-items:center;gap:6px;min-width:0">
				<input
					type="number"
					min="1"
					step="any"
					value={t.box.width ?? ''}
					placeholder="No box"
					readonly={editor.locked}
					aria-label="{labelOf(t)}: box width"
					onchange={(e) => commitWidth(e.currentTarget, t)}
					style="{field};width:96px;font-family:var(--font-mono);text-align:right"
				/>
				<select
					class="k-select"
					value={t.box.fit}
					aria-label="{labelOf(t)}: fit"
					disabled={t.box.width === null}
					aria-disabled={editor.locked || undefined}
					onchange={(e) => void commit(e.currentTarget, t, { fit: e.currentTarget.value as TextFit })}
				>
					{#each FITS as f (f.value)}
						<option value={f.value}>{f.label}</option>
					{/each}
				</select>
			</div>
			{#if t.box.width !== null && !t.outlined}
				<div style={caption}>Drawn with its font, so it does not keep to its box.</div>
			{/if}
		</div>
	{:else}
		<div style={caption}>Give a text an <code>id</code> in the SVG source to give it a box.</div>
	{/each}
	{#if texts.length}
		<div style="{caption};margin-top:4px">
			The width is in the artwork's units, from where the text is anchored. Shrink to fit keeps it on one line, made smaller
			until it fits.
		</div>
	{/if}
	<div style="{caption};margin-top:8px">
		{#if o.template}
			A template title's text is drawn as outlines: the same on every machine, whatever fonts it has.
		{:else if outlined}
			The text is drawn as outlines: the same on every machine, whatever fonts it has.
		{:else}
			The text is drawn with its fonts. As outlines it looks the same on every machine, and a box wraps or shrinks it.
		{/if}
	</div>
	{#if !o.template}
		<div style="margin-top:6px">
			{#if outlined}
				<Btn size="sm" icon="type" pending={editor.locked || converting} onclick={() => void convert('text')}>Draw with fonts</Btn>
			{:else}
				<Btn size="sm" icon="pen-tool" pending={editor.locked || converting} onclick={() => void convert('outlines')}
					>Draw as outlines</Btn
				>
			{/if}
		</div>
	{/if}
</InspectorSection>
