<script lang="ts" generics="S = undefined">
	// A number of the grade — a wheel's R, G, B or lightness, a curve point's In or Out — in a small labelled box:
	// the letter on top, the number beside the right edge in mono, like the number of a slider row. Click it and type (not held to
	// the step, but to the range and the digits shown), Enter or leave to write it, Escape to put it back; ArrowUp and ArrowDown
	// step it (a run of them is one edit, written when the keys go quiet, on Enter or on leaving; Shift is five steps, and Alt the
	// last digit shown, which in a box of hundredths stepping by hundredths is a full step: `stepBy`). What a write goes to is what
	// the edit *began* on (`scope`, noted when the box takes focus and again on the first key), not what is selected when it ends:
	// the click that selects another clip also blurs the box, and the entry is written on that blur. The run's keys are
	// `step-keys.ts`'s (undo drops a run, on the user's own key and not on the Control that begins it), the arithmetic `slider-row.ts`'s.
	import { keyEvent } from '#lib/key-route';
	import { settings } from '#lib/settings.svelte';
	import { StepKeys } from '#lib/step-keys';
	import { editable, leaveEntry, parseEntry, Pending, readout, stepBy, type RowSpec } from '#lib/slider-row';
	import { sliderKey } from '#lib/slider-gesture';
	import { tip } from '#lib/tip';

	let {
		label,
		name,
		value,
		spec,
		scope = undefined,
		disabled = false,
		hint = undefined,
		blank = false,
		note = undefined,
		onCommit
	}: {
		/** The letter shown on the box: `R`, `In`. */
		label: string;
		/** What it is, for a screen reader and the tooltip: `Shadows red`. */
		name: string;
		/** The committed value, in the property's own units. */
		value: number;
		spec: RowSpec;
		disabled?: boolean;
		/** What the box is editing — a clip — as it is now. Handed back, as it was when the edit began, with the value to write. */
		scope?: S;
		/** Said under the name in the tooltip: what the number does. */
		hint?: string;
		/** There is nothing to edit (no point is selected): the box shows a dash and holds still. */
		blank?: boolean;
		/** Said once about the number shown (it is not what was typed): marks the box and is the tooltip until the next edit. */
		note?: string;
		/** The value an entry ended on: write it, to `scope`. One call per edit. */
		onCommit: (v: number, scope: S) => unknown;
	} = $props();

	let preview = $state<number | null>(null);
	const shown = $derived(preview ?? value);
	const pending = new Pending<S>();
	const now = () => ({ scope: scope as S, value, spec });

	let editing = $state(false);
	let text = $state('');
	let dirty = false;
	/** Writes let go of and not landed yet: each ends its preview itself once its edit has landed. */
	let writing = 0;

	async function write(v: number, at: S) {
		preview = v;
		writing += 1;
		try {
			await onCommit(v, at);
		} finally {
			writing -= 1;
			if (writing === 0) preview = null;
		}
	}

	let box = $state<HTMLInputElement | null>(null);
	/** What a half-typed number says, or the value: where a run of steps starts from. */
	const typedNow = () => (dirty && box ? (parseEntry(box.value, spec) ?? value) : value);
	/** Put a number in the box, selected. */
	function put(v: number) {
		text = editable(v, spec);
		if (box) {
			box.value = text;
			box.select();
		}
	}
	/** Steps the box's keys have made: written as one edit, to what the first of them was on. */
	const keys = new StepKeys({
		owns: (k) => k === 'ArrowUp' || k === 'ArrowDown',
		step: (from, e) => {
			pending.begin(scope as S, value, spec);
			return stepBy(from, sliderKey(e.key, { shift: e.shiftKey, alt: e.altKey })!, spec);
		},
		value: () => value,
		start: typedNow,
		changed: (a, b) => Math.abs(a - b) > 1e-9,
		preview: (v) => {
			preview = v;
			dirty = false;
			put(v);
		},
		commit: (last) => {
			const began = pending.end(now);
			void write(last, began.scope);
		},
		// dropped (Escape, undo), or ended where it began: the committed number is shown again
		abandon: () => {
			pending.drop();
			pending.begin(scope as S, value, spec);
			preview = null;
			dirty = false;
			put(value);
		},
		held: () => false
	});
	// A box that goes away mid-run writes what it has: the keys were pressed, and the edit began on a clip that is still there.
	$effect(() => () => keys.run.finish());

	function enter(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		if (blank) return;
		editing = true;
		dirty = false;
		// A fresh edit: one left over (the box was off while it had focus, and never blurred) is not this one's.
		keys.run.cancel();
		pending.drop();
		pending.begin(scope as S, value, spec);
		text = editable(shown, spec);
		e.currentTarget.value = text;
		e.currentTarget.select();
	}

	function leave(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		if (blank) return;
		editing = false;
		if (keys.run.active) {
			keys.run.finish();
			dirty = false;
			return;
		}
		const { from, entry } = leaveEntry(pending, now, { dirty, text: e.currentTarget.value });
		dirty = false;
		if (entry.kind === 'commit') void write(entry.value, from.scope);
		else preview = null;
	}

	function typed(e: Event & { currentTarget: HTMLInputElement }) {
		if (blank) return;
		dirty = true;
		text = e.currentTarget.value;
		pending.begin(scope as S, value, spec);
	}

	function onkeydown(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
		if (disabled || blank) return;
		if (keys.key(keyEvent(e, settings.actionFor(e) === 'edit.undo'))) return;
		// Not the run's: Enter writes what is typed, Escape puts the number back (with nothing typed it is the page's, which clears the selection).
		if (e.key === 'Enter') {
			e.preventDefault();
			e.currentTarget.blur();
		} else if (e.key === 'Escape' && dirty) {
			pending.drop();
			pending.begin(scope as S, value, spec);
			preview = null;
			dirty = false;
			put(value);
			e.preventDefault();
			e.stopPropagation();
		}
	}
</script>

<label class="cell">
	<span class="l" aria-hidden="true">{label}</span>
	<input
		bind:this={box}
		class="n"
		type="text"
		role="spinbutton"
		inputmode="decimal"
		autocomplete="off"
		spellcheck="false"
		aria-label={name}
		aria-valuemin={spec.limit?.min ?? spec.min}
		aria-valuemax={spec.limit?.max ?? spec.max}
		aria-valuenow={blank ? undefined : Number(shown.toFixed(4))}
		aria-valuetext={blank ? 'none selected' : readout(shown, spec)}
		value={blank ? '–' : editing ? text : readout(shown, spec)}
		readonly={disabled || blank}
		aria-disabled={disabled || blank || undefined}
		data-held={note ? '' : undefined}
		use:tip={note ? { text: name, detail: note } : hint ? { text: name, detail: hint } : name}
		onfocus={enter}
		onblur={leave}
		oninput={typed}
		{onkeydown}
	/>
</label>

<style>
	.cell {
		position: relative;
		display: block;
		min-width: 0;
	}
	/* The letter sits on the box's top left, small and muted; the number is on the bottom right, so a long one (-0.12)
	   has the box's whole width and never runs under it. */
	.l {
		position: absolute;
		left: 4px;
		top: 2px;
		font-size: var(--text-2xs);
		line-height: 1;
		color: var(--text-muted);
		pointer-events: none;
		user-select: none;
	}
	.n {
		box-sizing: border-box;
		display: block;
		width: 100%;
		min-width: 0;
		height: 26px;
		padding: 9px 4px 0 4px;
		text-align: right;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		line-height: 15px;
		font-variant-numeric: tabular-nums;
		color: var(--text-secondary);
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-sm);
		transition:
			background-color var(--dur-fast) var(--ease-out),
			border-color var(--dur-fast) var(--ease-out),
			color var(--dur-fast) var(--ease-out);
	}
	.n:hover:not([aria-disabled='true']) {
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
	.n:focus {
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
	/* The number is not what was typed: the wheel held it to its rim. */
	.n[data-held] {
		border-color: var(--warning);
		color: var(--text-primary);
	}
	.n[aria-disabled='true'] {
		cursor: default;
	}
</style>
