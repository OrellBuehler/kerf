<script lang="ts">
	// The field a tag is typed into — in the tag dialog and in the Inspector. `key: value` or a plain word; as it is
	// typed the library's vocabulary answers (`suggestTags`): the keys first (accepting one types `key: ` and goes on),
	// then the values of the key typed, or the plain tags. Enter adds the highlighted suggestion, or else what is typed.
	// The list is drawn in the flow, below the field: a dialog clips whatever hangs out of it.
	import { editor } from '#lib/state.svelte';
	import { MAX_SUGGESTIONS, fillFor, parseTagInput, suggestTags, type Suggestion } from '#lib/tag-ui';
	import type { Tag } from '#lib/types';

	let {
		value = $bindable(''),
		exclude = [],
		placeholder = 'location: Cumbuco, or jump',
		label = 'Add a tag',
		error = '',
		busy = false,
		rows = MAX_SUGGESTIONS,
		reserve = false,
		oncommit,
		...rest
	}: {
		/** What is typed; bindable so a dialog can take the text that was not yet added. */
		value?: string;
		/** Tags not worth offering again (already pending, already there). */
		exclude?: readonly Tag[];
		placeholder?: string;
		label?: string;
		/** The reason the last commit was refused, said under the field. */
		error?: string;
		/** A commit is in flight. */
		busy?: boolean;
		/** The most suggestions the list shows. */
		rows?: number;
		/** Keep the room of a full list free below the field, so what is under it does not jump as the list comes and goes
		 *  (a dialog is centred: a list that opens grows it, and it moves). */
		reserve?: boolean;
		/** A tag was typed or picked. Return `false` to keep the text (the commit did not go through). */
		oncommit: (tag: Tag) => boolean | void | Promise<boolean | void>;
		[key: string]: unknown;
	} = $props();

	/** One suggestion's height (px), for the room `reserve` keeps. */
	const ROW_PX = 25;
	const uid = $props.id();
	const listId = `${uid}-list`;
	let field = $state<HTMLInputElement | null>(null);
	let focused = $state(false);
	/** Escape closed the list; typing opens it again. */
	let dismissed = $state(false);
	let active = $state(-1);
	let parseError = $state('');

	const suggestions = $derived<Suggestion[]>(suggestTags(value, editor.tagVocabulary, exclude, rows));
	const open = $derived(focused && !dismissed && suggestions.length > 0);
	$effect(() => {
		void suggestions;
		active = -1;
	});

	async function commit(tag: Tag) {
		parseError = '';
		const kept = await oncommit(tag);
		if (kept === false) return;
		value = '';
		dismissed = false;
	}

	function accept(s: Suggestion) {
		if (s.kind === 'key') {
			value = s.insert;
			dismissed = false;
			field?.focus();
			return;
		}
		void commit(s.tag);
	}

	function enter() {
		if (open && active >= 0) return accept(suggestions[active]);
		const parsed = parseTagInput(value);
		if ('error' in parsed) {
			// An empty field is not an error to shout about: Enter with nothing typed does nothing.
			if (value.trim() !== '') parseError = parsed.error;
			return;
		}
		void commit(parsed.tag);
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.isComposing) return;
		if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			if (suggestions.length === 0) return;
			e.preventDefault();
			dismissed = false;
			const n = suggestions.length;
			active = e.key === 'ArrowDown' ? (active + 1) % n : (active <= 0 ? n : active) - 1;
		} else if (e.key === 'Enter') {
			e.preventDefault();
			enter();
		} else if (e.key === 'Tab' && open && active >= 0 && !e.shiftKey) {
			// Tab fills the field with the suggestion that is lit — it adds nothing, only Enter does — and a Tab with
			// none lit moves on.
			e.preventDefault();
			e.stopPropagation();
			value = fillFor(suggestions[active]);
			dismissed = false;
		} else if (e.key === 'Escape' && open) {
			e.preventDefault();
			e.stopPropagation();
			dismissed = true;
			active = -1;
		}
	}

	const shown = $derived(error || parseError);
</script>

<div data-tag-input style="display:flex;flex-direction:column;gap:4px;min-width:0">
	<input
		bind:this={field}
		bind:value
		type="text"
		role="combobox"
		aria-label={label}
		aria-autocomplete="list"
		aria-expanded={open}
		aria-controls={open ? listId : undefined}
		aria-activedescendant={open && active >= 0 ? `${uid}-opt-${active}` : undefined}
		aria-invalid={shown ? 'true' : undefined}
		autocomplete="off"
		spellcheck="false"
		{placeholder}
		disabled={busy}
		onfocus={() => (focused = true)}
		onblur={() => (focused = false)}
		oninput={() => {
			dismissed = false;
			parseError = '';
		}}
		{onkeydown}
		{...rest}
		style="width:100%;box-sizing:border-box;min-height:28px;padding:4px 8px;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid {shown
			? 'var(--warning)'
			: 'var(--border-strong)'};border-radius:var(--radius-sm);font-size:12px"
	/>
	{#if shown}
		<div role="alert" data-tag-error style="font-size:11px;line-height:1.4;color:var(--warning)">{shown}</div>
	{/if}
	<div style={reserve ? `min-height:${rows * ROW_PX + 8}px` : undefined}>
		{#if open}
			<ul
				id={listId}
				role="listbox"
				aria-label="Suggestions"
				style="margin:0;padding:3px;list-style:none;display:flex;flex-direction:column;gap:1px;background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm)"
			>
				{#each suggestions as s, i (s.kind === 'key' ? `k:${s.label}` : `t:${s.label}`)}
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<li
						id="{uid}-opt-{i}"
						role="option"
						aria-selected={i === active}
						data-suggestion={s.kind}
						onpointerdown={(e) => e.preventDefault()}
						onclick={() => accept(s)}
						onpointermove={() => (active = i)}
						style="display:flex;align-items:baseline;gap:8px;padding:3px 6px;border-radius:var(--radius-sm);font-size:12px;cursor:pointer;color:var(--text-primary);background:{i === active
							? 'var(--surface-active)'
							: 'transparent'}"
					>
						<span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{s.label}</span>
						{#if s.kind === 'key'}
							<span style="flex:none;font-size:11px;color:var(--text-muted)">key</span>
						{/if}
						<span style="flex:none;font-size:11px;font-variant-numeric:tabular-nums;color:var(--text-muted)">{s.assets}</span>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>
