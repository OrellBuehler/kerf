<script lang="ts">
	// One control of the Save-as-template dialog: what it is called, what kind of thing it changes, which
	// element(s) of the title it writes to and — for everything but text — which attribute, with the likely
	// one already chosen. A control is a draft (`DraftControl`); every edit hands back a new one and the
	// dialog holds the list. The problem the engine would raise is said under it, before the round trip.
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import {
		KIND_CHOICES,
		MAX_TARGETS,
		addTarget,
		ambiguousFor,
		attributeChoices,
		changeKind,
		editId,
		elementsFor,
		numericAttributes,
		pickElement,
		relabel,
		removeTarget,
		setAttribute,
		takesAttribute,
		type DraftContext,
		type DraftControl
	} from '#lib/template-ui';
	import { tip } from '#lib/tip';
	import type { TemplateKind } from '#lib/types';

	let {
		draft,
		index,
		others,
		ctx,
		problem,
		onchange,
		onremove
	}: {
		draft: DraftControl;
		/** Its place in the dialog's list, from 0: what names it to a screen reader until it has a label. */
		index: number;
		/** Every other control of the dialog: an id is kept clear of theirs. */
		others: readonly DraftControl[];
		ctx: DraftContext;
		problem: string | null;
		onchange: (next: DraftControl) => void;
		onremove: () => void;
	} = $props();

	const uid = $props.id();
	const closed = $derived(attributeChoices(draft.kind));
	const options = $derived(elementsFor(draft.kind, ctx.elements));
	const ambiguous = $derived(ambiguousFor(draft.kind, ctx.elements));
	// Every field of a row says whose it is: ten rows of "Id" and "Remove" are not ten things to a screen reader.
	const who = $derived(draft.label.trim() ? `“${draft.label.trim()}”` : `control ${index + 1}`);

	const field =
		'box-sizing:border-box;height:var(--control-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm);padding:0 8px;min-width:0';
	const label = 'font-size:12px;color:var(--text-muted)';
	const row = 'display:grid;grid-template-columns:62px minmax(0,1fr);align-items:center;column-gap:8px';
</script>

<div
	data-control-key={draft.key}
	data-invalid={problem ? '' : undefined}
	style="display:flex;flex-direction:column;gap:7px;padding:9px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid {problem
		? 'color-mix(in srgb,var(--orange-400) 55%,transparent)'
		: 'var(--border-default)'}"
>
	<div style="display:flex;align-items:center;gap:8px">
		<input
			data-label
			type="text"
			value={draft.label}
			placeholder="Label, e.g. Tagline"
			aria-label="Label of control {index + 1}"
			oninput={(e) => onchange(relabel(draft, e.currentTarget.value, others))}
			style="{field};flex:1"
		/>
		<select
			class="k-select"
			aria-label="Kind of {who}"
			use:tip={KIND_CHOICES.find((k) => k.id === draft.kind)?.hint}
			value={draft.kind}
			onchange={(e) => onchange(changeKind(draft, e.currentTarget.value as TemplateKind, ctx))}
			style="width:104px;flex:none"
		>
			{#each KIND_CHOICES as k (k.id)}
				<option value={k.id}>{k.label}</option>
			{/each}
		</select>
		<IconBtn title="Remove {who}" size={28} data-tone="quiet" onclick={onremove}><Icon n="trash" s={14} /></IconBtn>
	</div>

	<label style={row}>
		<span style={label}>Id</span>
		<input
			type="text"
			value={draft.id}
			spellcheck="false"
			autocomplete="off"
			aria-label="Id of {who}, how an agent or a script names it"
			oninput={(e) => onchange(editId(draft, e.currentTarget.value))}
			style="{field};font-family:var(--font-mono)"
		/>
	</label>

	{#each draft.targets as t, i (i)}
		{@const listed = options.some((e) => e.id === t.element)}
		{@const numbers = t.element ? numericAttributes(ctx.svg, t.element) : []}
		<div style={row}>
			<span style={label}>{i === 0 ? 'Changes' : ''}</span>
			<div style="display:flex;align-items:center;gap:6px;min-width:0">
				<select
					class="k-select"
					aria-label={draft.targets.length > 1 ? `Element ${i + 1} that ${who} changes` : `Element that ${who} changes`}
					value={t.element}
					onchange={(e) => onchange(pickElement(draft, i, e.currentTarget.value, ctx))}
					style="flex:1;min-width:0"
				>
					<option value="">Choose an element…</option>
					{#each options as e (e.id)}
						<option value={e.id}>{e.id} · {e.tag}</option>
					{/each}
					{#each ambiguous as a (a.id)}
						<option value={a.id} disabled={a.id !== t.element}>{a.id} · used by {a.count} elements</option>
					{/each}
					{#if t.element && !listed && !ambiguous.some((a) => a.id === t.element)}
						<option value={t.element}>{t.element}</option>
					{/if}
				</select>
				{#if takesAttribute(draft.kind)}
					{#if closed && closed.length === 1}
						<span
							style="{field};display:inline-flex;align-items:center;width:104px;flex:none;font-family:var(--font-mono);color:var(--text-secondary)"
							>{closed[0]}</span
						>
					{:else if closed}
						<select
							class="k-select"
							aria-label="Attribute {who} sets"
							value={t.attribute}
							onchange={(e) => onchange(setAttribute(draft, i, e.currentTarget.value))}
							style="width:112px;flex:none;font-family:var(--font-mono)"
						>
							{#each closed as a (a)}
								<option value={a}>{a}</option>
							{/each}
							{#if t.attribute && !closed.includes(t.attribute)}
								<option value={t.attribute}>{t.attribute}</option>
							{/if}
						</select>
					{:else}
						<input
							type="text"
							list="{uid}-attrs-{i}"
							value={t.attribute}
							placeholder="attribute"
							spellcheck="false"
							autocomplete="off"
							aria-label="Attribute {who} sets, e.g. font-size"
							oninput={(e) => onchange(setAttribute(draft, i, e.currentTarget.value))}
							style="{field};width:112px;flex:none;font-family:var(--font-mono)"
						/>
						<datalist id="{uid}-attrs-{i}">
							{#each numbers as a (a)}
								<option value={a}></option>
							{/each}
						</datalist>
					{/if}
				{/if}
				{#if draft.targets.length > 1}
					<IconBtn title="Stop {who} changing {t.element ? `“${t.element}”` : 'this element'}" size={24} data-tone="quiet" onclick={() => onchange(removeTarget(draft, i))}
						><Icon n="x" s={13} /></IconBtn
					>
				{/if}
			</div>
		</div>
	{/each}
	{#if draft.targets.length < MAX_TARGETS}
		<div style={row}>
			<span></span>
			<div>
				<Btn
					size="sm"
					variant="ghost"
					icon="plus"
					iconSize={12}
					aria-label="Change another element too, for {who}"
					onclick={() => onchange(addTarget(draft))}>Change another element too</Btn
				>
			</div>
		</div>
	{/if}

	{#if draft.kind === 'number'}
		<div style={row}>
			<span style={label}>Range</span>
			<div style="display:flex;align-items:center;gap:6px">
				<input
					type="text"
					inputmode="decimal"
					value={draft.min}
					placeholder="min"
					aria-label="Smallest value of {who}, optional"
					oninput={(e) => onchange({ ...draft, min: e.currentTarget.value })}
					style="{field};width:72px;font-family:var(--font-mono);text-align:right"
				/>
				<span style={label}>to</span>
				<input
					type="text"
					inputmode="decimal"
					value={draft.max}
					placeholder="max"
					aria-label="Largest value of {who}, optional"
					oninput={(e) => onchange({ ...draft, max: e.currentTarget.value })}
					style="{field};width:72px;font-family:var(--font-mono);text-align:right"
				/>
				<span style={label}>optional</span>
			</div>
		</div>
	{/if}

	{#if problem}
		<div role="alert" style="font-size:12px;line-height:1.4;color:var(--orange-400)">{problem}</div>
	{/if}
</div>
