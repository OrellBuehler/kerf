<script lang="ts" generics="S = undefined">
	// A number of the grade — a wheel's R, G, B or lightness, a curve point's In or Out — in a small labelled box:
	// the letter on top, the number beside the right edge in mono, like the number of a slider row. Click it and type (not held to
	// the step, but to the range and the digits shown), Enter or leave to write it, Escape to put it back; the arrows
	// step it (a run of them is one edit, written when the keys go quiet, on Enter or on leaving; Shift is five steps,
	// Alt a tenth). What a write goes to is what the edit *began* on (`scope`, noted when the box takes focus and again on the
	// first key), not what is selected when it ends: the click that selects another clip also blurs the box, and the
	// entry is written on that blur. The arithmetic is `slider-row.ts`'s and `key-run.ts`'s, which have the tests.
	import { KeyRun } from '#lib/key-run';
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

	/** Steps the box's keys have made: written as one edit, to what the first of them was on. */
	const run = new KeyRun<number>((from, last) => {
		const began = pending.end(now);
		if (Math.abs(last - from) < 1e-9) {
			preview = null;
			return;
		}
		void write(last, began.scope);
	});
	// A box that goes away mid-run writes what it has: the keys were pressed, and the edit began on a clip that is still there.
	$effect(() => () => run.finish());

	function enter(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		if (blank) return;
		editing = true;
		dirty = false;
		// A fresh edit: one left over (the box was off while it had focus, and never blurred) is not this one's.
		run.cancel();
		pending.drop();
		pending.begin(scope as S, value, spec);
		text = editable(shown, spec);
		e.currentTarget.value = text;
		e.currentTarget.select();
	}

	function leave(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		if (blank) return;
		editing = false;
		if (run.active) {
			run.finish();
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

	function put(el: HTMLInputElement, v: number) {
		text = editable(v, spec);
		el.value = text;
		el.select();
	}

	function onkeydown(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
		if (disabled || blank) return;
		const el = e.currentTarget;
		if (e.key === 'Enter') {
			e.preventDefault();
			if (run.active) run.finish();
			else el.blur();
			return;
		}
		if (e.key === 'Escape') {
			// Only a change in flight is Escape's: with none, it falls through to the page (which clears the selection).
			if (run.active || dirty) {
				run.cancel();
				pending.drop();
				pending.begin(scope as S, value, spec);
				preview = null;
				dirty = false;
				put(el, value);
				e.preventDefault();
				e.stopPropagation();
			}
			return;
		}
		if (e.ctrlKey || e.metaKey) {
			// A run not yet written out is the newest change: undo takes it back rather than the edit before it.
			if (run.active && e.key.toLowerCase() === 'z' && !e.shiftKey) {
				run.cancel();
				preview = null;
				put(el, value);
				e.preventDefault();
				e.stopPropagation();
			} else run.finish();
			return;
		}
		if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
		const key = sliderKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key) return;
		e.preventDefault();
		pending.begin(scope as S, value, spec);
		// From the run's last stop, else from what is typed in the box (a number half-written steps from what it says), else from the value.
		const typedNow = dirty ? parseEntry(el.value, spec) : null;
		const from = run.from ?? value;
		const base = run.last ?? typedNow ?? value;
		const next = stepBy(base, key, spec);
		run.push(from, next);
		dirty = false;
		preview = next;
		put(el, next);
	}
</script>

<label class="cell">
	<span class="l" aria-hidden="true">{label}</span>
	<input
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
		use:tip={hint ? { text: name, detail: hint } : name}
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
	.n[aria-disabled='true'] {
		cursor: default;
	}
</style>
