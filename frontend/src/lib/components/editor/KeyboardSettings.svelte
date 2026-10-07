<script lang="ts">
	// Settings › Keyboard: every shortcut the editor has, by group, with the keys
	// it is on now. Click a key to record another; Esc gives up, Backspace
	// removes it. A key another action already has is not taken silently — the
	// row says who has it and offers to swap or to unbind them. Only what is
	// changed from the defaults is stored (`keymap.ts`), so Reset puts a row — or
	// everything — back to following them.
	import { tick } from 'svelte';
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { settings } from '$lib/settings.svelte';
	import {
		FIXED_KEYS,
		GROUPS,
		actionDef,
		chordFromEvent,
		displayChord,
		filterActions,
		sameChord,
		type ActionDef,
		type Chord,
		type Rebind
	} from '$lib/keymap';

	const platform = settings.platform;

	let root = $state<HTMLElement | null>(null);
	let query = $state('');
	/** The key being recorded: which action, and the chord it replaces (null: one is being added). */
	let recording = $state<{ id: string; replacing: Chord | null } | null>(null);
	/** A recorded chord that another action already has, waiting for a decision. */
	let pending = $state<{ rebind: Rebind; conflicts: ActionDef[] } | null>(null);
	let confirmingReset = $state(false);
	/** What a screen reader is told after a change; the list itself is the visual. */
	let announcement = $state('');

	const visible = $derived(filterActions(query, settings.bindings, platform));
	const groups = $derived(
		GROUPS.map((g) => ({ ...g, actions: visible.filter((a) => a.group === g.id) })).filter((g) => g.actions.length > 0)
	);
	const changed = $derived(Object.keys(settings.keyOverrides.bindings).length);

	const show = (c: Chord) => displayChord(c, platform);
	const nameOf = (id: string) => actionDef(id)?.label ?? id;

	/** Put focus on something once the list has settled. A recording, a reset or a
	 *  confirmation removes the button that had it, and focus left on the page
	 *  behind a modal is a keyboard user locked out: Escape would stop closing it. */
	async function focusSelector(...selectors: string[]) {
		await tick();
		for (const selector of selectors) {
			const el = root?.querySelector<HTMLElement>(selector);
			if (el) return el.focus();
		}
	}
	/** A row — or, when the search no longer shows it (its key changed and stopped
	 *  matching), the search box. */
	const focusRow = (id: string) => focusSelector(`[data-key-row="${CSS.escape(id)}"]`, '[data-key-search]');

	function startRecording(id: string, replacing: Chord | null) {
		pending = null;
		recording = { id, replacing };
		announcement = `Recording a shortcut for ${nameOf(id)}. Press the keys. Escape cancels, Backspace removes it.`;
	}

	function stopRecording() {
		const id = recording?.id;
		recording = null;
		if (id) void focusRow(id);
	}

	function commit(chord: Chord) {
		if (!recording) return;
		const { id, replacing } = recording;
		if (replacing && sameChord(replacing, chord)) return stopRecording();
		const rebind: Rebind = { id, chord, replacing };
		const conflicts = settings.conflictsFor(rebind);
		if (conflicts.length === 0) {
			settings.rebind(rebind);
			announcement = `${nameOf(id)} is now ${show(chord)}.`;
			return stopRecording();
		}
		recording = null;
		pending = { rebind, conflicts };
		announcement = `${show(chord)} is already used by ${conflicts.map((c) => c.label).join(' and ')}.`;
	}

	function onRecordKey(e: KeyboardEvent) {
		if (!recording) return;
		// Tab moves on; the recording ends with the focus.
		if (e.key === 'Tab') return;
		// Whatever is pressed is the answer, not something for the dialog or the page.
		e.preventDefault();
		e.stopPropagation();
		if (e.repeat) return;
		const bare = !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey;
		if (bare && e.key === 'Escape') return stopRecording();
		if (bare && e.key === 'Backspace') {
			const { id, replacing } = recording;
			if (replacing) {
				settings.removeChord(id, replacing);
				announcement = `Removed ${show(replacing)} from ${nameOf(id)}.`;
			}
			return stopRecording();
		}
		// Still holding a modifier, or a key that cannot be a shortcut: keep listening.
		const chord = chordFromEvent(e);
		if (chord) commit(chord);
	}

	function settle(how: 'swap' | 'unbind' | null) {
		if (!pending) return;
		const { rebind, conflicts } = pending;
		pending = null;
		if (how) {
			settings.rebind(rebind, how);
			announcement =
				how === 'swap'
					? `Swapped: ${nameOf(rebind.id)} is now ${show(rebind.chord)}.`
					: `${nameOf(rebind.id)} is now ${show(rebind.chord)}; ${conflicts.map((c) => c.label).join(' and ')} unbound.`;
		}
		void focusRow(rebind.id);
	}

	function reset(id: string) {
		settings.resetKey(id);
		announcement = `${nameOf(id)} is back to its default.`;
		void focusRow(id);
	}

	function resetEverything() {
		settings.resetAllKeys();
		confirmingReset = false;
		announcement = 'All shortcuts are back to their defaults.';
		// "Reset all" is disabled now, so it cannot take the focus back.
		void focusSelector('[data-key-search]');
	}

	function keepShortcuts() {
		confirmingReset = false;
		void focusSelector('[data-key-reset-all]');
	}

	function onSearchKey(e: KeyboardEvent) {
		// A first Escape empties the search; the dialog's own Escape is the second.
		if (e.key === 'Escape' && query) {
			query = '';
			e.stopPropagation();
		}
	}

	function onPendingKey(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		e.stopPropagation();
		settle(null);
	}

	/** Take focus when the element appears: the recorder, and the answer to a
	 *  question — the safe button of it (`data-autofocus`) when one is marked, else the first. */
	function takeFocus(node: HTMLElement) {
		(node.matches('button') ? node : (node.querySelector<HTMLElement>('[data-autofocus]') ?? node.querySelector<HTMLElement>('button')))?.focus();
	}

	const chip = (active = false) =>
		`display:inline-flex;align-items:center;gap:4px;height:22px;padding:0 7px;border-radius:var(--radius-xs);font-family:var(--font-mono);font-size:11px;line-height:1;white-space:nowrap;cursor:pointer;border:var(--line-width) solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
			active ? 'var(--text-primary)' : 'var(--text-secondary)'
		}`;
	const iconBtn =
		'display:inline-grid;place-items:center;width:22px;height:22px;padding:0;border-radius:var(--radius-xs);cursor:pointer;background:transparent;border:var(--line-width) solid transparent;color:var(--text-muted)';
</script>

<div bind:this={root} style="position:relative;flex:1;min-height:0;display:flex;flex-direction:column">
	<div
		style="flex:none;padding:12px 16px 10px;border-bottom:var(--line-width) solid var(--border-subtle);display:flex;flex-direction:column;gap:8px"
	>
		<div style="display:flex;align-items:center;gap:8px">
			<label
				style="flex:1;min-width:0;display:flex;align-items:center;gap:7px;height:28px;padding:0 8px;background:var(--surface-inset);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-sm);color:var(--text-muted)"
			>
				<Icon n="search" s={13} color="currentColor" />
				<input
					type="search"
					data-key-search
					bind:value={query}
					oninput={() => {
						// What was being recorded or asked about may be filtered out of sight.
						recording = null;
						pending = null;
					}}
					onkeydown={onSearchKey}
					placeholder="Search actions or keys"
					aria-label="Search keyboard shortcuts"
					style="flex:1;min-width:0;height:100%;padding:0;background:transparent;border:none;outline:none;color:var(--text-primary);font-size:12px"
				/>
			</label>
			<Btn
				variant="ghost"
				size="sm"
				icon="rotate-ccw"
				data-key-reset-all
				disabled={changed === 0 || confirmingReset}
				title={changed === 0 ? 'Every shortcut is at its default' : `Put all ${changed} changed shortcuts back to their defaults`}
				onclick={() => (confirmingReset = true)}>Reset all</Btn
			>
		</div>
		{#if confirmingReset}
			<div
				role="alertdialog"
				aria-label="Reset all shortcuts"
				use:takeFocus
				style="display:flex;align-items:center;gap:8px;min-height:30px"
			>
				<span style="flex:1;font-size:12px;color:var(--text-secondary)"
					>Put {changed === 1 ? 'the 1 changed shortcut' : `all ${changed} changed shortcuts`} back to the defaults?</span
				>
				<Btn variant="destructive" size="sm" onclick={resetEverything}>Reset</Btn>
				<Btn variant="ghost" size="sm" data-autofocus onclick={keepShortcuts}>Keep</Btn>
			</div>
		{:else}
			<p style="margin:0;font-size:12px;line-height:1.5;min-height:30px;color:var(--text-disabled)">
				Click a shortcut, then press the keys you want. Esc cancels; Backspace removes it.
			</p>
		{/if}
	</div>

	<div style="flex:1;min-height:0;overflow-y:auto;padding:4px 16px 16px">
		{#each groups as g (g.id)}
			<div
				role="heading"
				aria-level="3"
				style="margin-top:14px;font:var(--type-label);color:var(--text-secondary);text-transform:uppercase;letter-spacing:.06em"
			>
				{g.label}
			</div>
			<ul style="list-style:none;margin:4px 0 0;padding:0">
				{#each g.actions as a (a.id)}
					{@const chords = settings.bindings[a.id] ?? []}
					{@const custom = settings.isCustomKey(a.id)}
					{@const adding = recording?.id === a.id && recording.replacing === null}
					<li style="border-bottom:var(--line-width) solid var(--border-subtle)">
						<div style="display:flex;align-items:center;gap:8px;min-height:34px;padding:4px 0">
							<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:2px">
								<span style="display:flex;align-items:center;gap:6px;font-size:12px;color:var(--text-primary)">
									{a.label}
									{#if custom}
										<span
											title="Changed from the default"
											role="img"
											aria-label="Changed from the default"
											style="width:6px;height:6px;border-radius:50%;background:var(--kerf-400);flex:none"
										></span>
									{/if}
								</span>
								{#if a.hint}
									<span style="font-size:11px;line-height:1.4;color:var(--text-disabled)">{a.hint}</span>
								{/if}
							</div>

							<div style="flex:none;display:flex;align-items:center;justify-content:flex-end;flex-wrap:wrap;gap:4px;max-width:60%">
								{#each chords as c (show(c))}
									{#if recording?.id === a.id && recording.replacing && sameChord(recording.replacing, c)}
										<button
											use:takeFocus
											onkeydown={onRecordKey}
											onblur={() => (recording = null)}
											data-key-row={a.id}
											aria-label="Recording a shortcut for {a.label}. Press the keys, Escape to cancel, Backspace to remove."
											style={chip(true)}>Press keys…</button
										>
									{:else}
										<button
											data-key-row={a.id}
											onclick={() => startRecording(a.id, c)}
											title="Change this shortcut"
											aria-label="{a.label}: {show(c)}. Change this shortcut"
											style={chip()}>{show(c)}</button
										>
									{/if}
								{/each}

								{#if adding}
									<button
										use:takeFocus
										onkeydown={onRecordKey}
										onblur={() => (recording = null)}
										data-key-row={a.id}
										aria-label="Recording a shortcut for {a.label}. Press the keys, Escape to cancel."
										style={chip(true)}>Press keys…</button
									>
								{:else if chords.length === 0}
									<button
										data-key-row={a.id}
										onclick={() => startRecording(a.id, null)}
										aria-label="{a.label} has no shortcut. Assign one"
										style="{chip()};border-style:dashed;color:var(--text-disabled)">Unassigned</button
									>
								{:else}
									<button
										data-key-row={a.id}
										onclick={() => startRecording(a.id, null)}
										title="Add another shortcut"
										aria-label="Add another shortcut to {a.label}"
										style={iconBtn}><Icon n="plus" s={12} color="currentColor" /></button
									>
								{/if}

								{#if custom}
									<button
										data-key-row={a.id}
										onclick={() => reset(a.id)}
										title="Back to the default"
										aria-label="Reset {a.label} to its default"
										style={iconBtn}><Icon n="rotate-ccw" s={12} color="currentColor" /></button
									>
								{/if}
							</div>
						</div>

						{#if pending?.rebind.id === a.id}
							{@const p = pending}
							{@const names = p.conflicts.map((c) => c.label)}
							<!-- Escape here answers the question (Cancel); the keydown is a shortcut for the
							     Cancel button, which is the real control. -->
							<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
							<div
								role="alert"
								data-key-conflict
								tabindex="-1"
								use:takeFocus
								onkeydown={onPendingKey}
								style="margin:0 0 6px;padding:8px 10px;display:flex;flex-direction:column;gap:8px;border-radius:var(--radius-sm);background:var(--warning-surface);border:var(--line-width) solid color-mix(in srgb,var(--warning) 40%,transparent);font-size:12px;line-height:1.45;color:var(--text-primary)"
							>
								<div style="display:flex;align-items:flex-start;gap:8px">
									<span style="flex:none;display:flex;margin-top:1px"
										><Icon n="alert-triangle" s={13} color="var(--warning)" /></span
									>
									<span
										><strong style="font-family:var(--font-mono);font-weight:600">{show(p.rebind.chord)}</strong> is already
										used by <strong style="font-weight:600">{names.join(' and ')}</strong>.</span
									>
								</div>
								<div style="display:flex;flex-wrap:wrap;gap:6px;padding-left:21px">
									{#if p.rebind.replacing && p.conflicts.length === 1}
										<Btn
											variant="secondary"
											size="sm"
											title="{names[0]} takes {show(p.rebind.replacing)}, which {a.label} is giving up"
											onclick={() => settle('swap')}>Swap</Btn
										>
									{/if}
									<Btn variant="secondary" size="sm" onclick={() => settle('unbind')}
										>Unbind {names.length === 1 ? names[0] : 'them'}</Btn
									>
									<Btn variant="ghost" size="sm" onclick={() => settle(null)}>Cancel</Btn>
								</div>
							</div>
						{/if}
					</li>
				{/each}
			</ul>
		{/each}

		{#if groups.length === 0}
			<p style="margin:18px 0 0;font-size:12px;color:var(--text-muted)">
				No action or key matches “{query}”.
			</p>
		{:else if !query}
			<div
				role="heading"
				aria-level="3"
				style="margin-top:22px;font:var(--type-label);color:var(--text-secondary);text-transform:uppercase;letter-spacing:.06em"
			>
				Always the same
			</div>
			<p style="margin:4px 0 6px;font-size:12px;line-height:1.5;color:var(--text-disabled)">
				These work the same everywhere and cannot be changed.
			</p>
			<ul style="list-style:none;margin:0;padding:0">
				{#each FIXED_KEYS as f (f.keys)}
					<li
						style="display:flex;align-items:center;gap:10px;min-height:28px;border-bottom:var(--line-width) solid var(--border-subtle);font-size:12px;color:var(--text-secondary)"
					>
						<span style="flex:1;min-width:0">{f.does}</span>
						<span
							style="flex:none;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap"
							>{f.keys}</span
						>
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<div aria-live="polite" role="status" style="position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%);white-space:nowrap">
		{announcement}
	</div>
</div>
