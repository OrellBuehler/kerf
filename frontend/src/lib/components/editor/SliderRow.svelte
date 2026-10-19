<script lang="ts" generics="S = undefined">
	// One numeric property: a label, a slider, a number field and a slot for the keyframe button,
	// on one grid, so every slider in a panel starts and ends at the same place whether or not it
	// can be animated (the slot is kept even when it is empty). The slider is `MixSlider`'s, so a
	// drag is one edit on release, Escape / pointercancel / blur abandon it, a run of arrow keys is
	// one edit, and a double-click puts the property back to its rest value; the rest value is a tick
	// on the travel, and a property that goes both ways fills from it. The number is a field: click it,
	// type a value (not held to the slider's step), Enter or leave to write it, Escape to put it back.
	// What a write goes to is what the edit *began* on (`scope`, read when a drag, a run of keys or a typed
	// entry starts), not what is selected when it ends: the click that selects another clip also
	// blurs the field, and the entry is written on that blur. The numbers are `slider-row.ts`'s, which has the tests.
	// `onLive` hears the value while a drag or a run of keys goes on, and `null` once its write has landed or it
	// was abandoned: the Inspector draws the frame of the gesture with it (the GPU preview's draft).
	import type { Snippet } from 'svelte';
	import MixSlider from './MixSlider.svelte';
	import {
		editable,
		entryFor,
		fillOrigin,
		fromPos,
		neutralOf,
		NUMBER_COL,
		Pending,
		readout,
		resetTo,
		stepBy,
		tickPos,
		toPos,
		type RowSpec
	} from '#lib/slider-row';
	import { tip } from '#lib/tip';

	let {
		label,
		value,
		spec,
		disabled = false,
		labelWidth = 72,
		reserve = true,
		keyframe = undefined,
		scope = undefined,
		onCommit,
		onLive = undefined
	}: {
		label: string;
		/** The committed value, in the property's own units. */
		value: number;
		spec: RowSpec;
		disabled?: boolean;
		/** px of the label column. */
		labelWidth?: number;
		/** Keep the keyframe slot's 24 px when there is no keyframe button, so the rows of a panel line up. */
		reserve?: boolean;
		keyframe?: Snippet;
		/** What the row is editing — a clip, a playhead time — as it is now. Handed back, as it was when the
		 *  edit began, with the value to write. */
		scope?: S;
		/** The value a gesture ended on, or a typed entry, or the rest value: write it, to `scope`. One call per gesture. */
		onCommit: (v: number, scope: S) => unknown;
		/** The value a gesture is at, for `scope` as it began; `null` when it is over (written or abandoned). */
		onLive?: (v: number | null, scope: S) => void;
	} = $props();

	let preview = $state<number | null>(null);
	const shown = $derived(preview ?? value);
	const pending = new Pending<S>();
	const now = () => ({ scope: scope as S, value, spec });

	function move(v: number) {
		pending.begin(scope as S, value, spec);
		preview = v;
		onLive?.(v, pending.scope as S);
	}
	/** The slider keeps showing where it was let go until the write has landed, so it does not
	 *  jump back to the old value for the length of a round trip. */
	async function write(v: number, at: S) {
		preview = v;
		try {
			await onCommit(v, at);
		} finally {
			preview = null;
			onLive?.(null, at);
		}
	}
	const commit = (v: number) => write(v, pending.end(now()).scope);
	function cancel() {
		const at = pending.scope ?? (scope as S);
		pending.drop();
		preview = null;
		onLive?.(null, at);
	}
	function reset() {
		pending.drop();
		preview = null;
		const to = resetTo(value, spec);
		if (to !== null) void write(to, scope as S);
	}

	let editing = $state(false);
	let text = $state('');
	let dirty = false;

	function enter(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		editing = true;
		dirty = false;
		// A fresh edit: one left over (the row was disabled while it had focus, and never blurred) is not this one's.
		pending.drop();
		pending.begin(scope as S, value, spec);
		text = editable(shown, spec);
		e.currentTarget.value = text;
		e.currentTarget.select();
	}
	function leave(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		editing = false;
		const from = pending.end(now());
		const entry = dirty ? entryFor(e.currentTarget.value, from.value, from.spec) : ({ kind: 'revert' } as const);
		dirty = false;
		if (entry.kind === 'commit') void write(entry.value, from.scope);
	}
	function typed(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
		if (e.key === 'Enter') {
			e.currentTarget.blur();
		} else if (e.key === 'Escape' && dirty) {
			dirty = false;
			text = editable(shown, spec);
			e.currentTarget.value = text;
			e.currentTarget.blur();
			e.stopPropagation();
		}
	}
</script>

<div class="row" class:slotless={!reserve && !keyframe} style="--label-w:{labelWidth}px;--num-w:{NUMBER_COL}px">
	<span class="label" use:tip={label}>{label}</span>
	<MixSlider
		orientation="horizontal"
		solid
		value={shown}
		toPos={(v) => toPos(v, spec)}
		fromPos={(p) => fromPos(p, spec)}
		step={(v, key) => stepBy(v, key, spec)}
		{label}
		valueText={(v) => readout(v, spec)}
		tip="{label} {readout(shown, spec)} — drag the handle, or use the arrow keys{neutralOf(spec) === null ? '' : '; double-click to reset'}"
		detent={tickPos(spec)}
		fillFrom={fillOrigin(spec)}
		accent="var(--kerf-500)"
		{disabled}
		onpreview={move}
		oncommit={commit}
		oncancel={cancel}
		onreset={reset}
	/>
	<input
		class="num"
		type="text"
		inputmode="decimal"
		autocomplete="off"
		spellcheck="false"
		aria-label="{label} value"
		value={editing ? text : readout(shown, spec)}
		readonly={disabled}
		aria-disabled={disabled || undefined}
		onfocus={enter}
		onblur={leave}
		oninput={(e) => {
			dirty = true;
			text = e.currentTarget.value;
		}}
		onkeydown={typed}
	/>
	{#if reserve || keyframe}
		<span class="slot">{@render keyframe?.()}</span>
	{/if}
</div>

<style>
	.row {
		display: grid;
		grid-template-columns: var(--label-w) minmax(0, 1fr) var(--num-w) 24px;
		align-items: center;
		column-gap: 8px;
		padding: 2px 0;
		min-width: 0;
	}
	.row.slotless {
		grid-template-columns: var(--label-w) minmax(0, 1fr) var(--num-w);
	}
	.label {
		font-size: var(--text-sm);
		color: var(--text-secondary);
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.num {
		box-sizing: border-box;
		width: 100%;
		min-width: 0;
		height: 24px;
		padding: 0 3px;
		text-align: right;
		font-family: var(--font-mono);
		font-size: var(--text-sm);
		color: var(--text-secondary);
		background: transparent;
		border: var(--line-width) solid transparent;
		border-radius: var(--radius-sm);
		transition:
			background-color var(--dur-fast) var(--ease-out),
			border-color var(--dur-fast) var(--ease-out),
			color var(--dur-fast) var(--ease-out);
	}
	.num:hover:not([aria-disabled='true']) {
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
	.num:focus {
		background: var(--surface-inset);
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
	.num[aria-disabled='true'] {
		cursor: default;
	}
	.slot {
		width: 24px;
		height: 24px;
		display: grid;
		place-items: center;
	}
</style>
