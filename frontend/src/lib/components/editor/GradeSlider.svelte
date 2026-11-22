<script lang="ts" generics="S = undefined">
	// A wheel's lightness: `MixSlider`'s travel on a row of its own with no label or number (the number is a
	// `GradeField` beside it), so it fits under a disc a hundred pixels wide. What `SliderRow` does around the slider is
	// done here the same way: a drag, a run of keys or a double-click is one edit; Escape, pointercancel and blur drop it; the write
	// goes to what the edit began on (`scope`), and the slider keeps showing where it was let go until the write has landed.
	import MixSlider from './MixSlider.svelte';
	import { fillOrigin, fromPos, neutralOf, Pending, readout, resetTo, stepBy, tickPos, toPos, type RowSpec } from '#lib/slider-row';

	let {
		name,
		value,
		spec,
		scope = undefined,
		disabled = false,
		words = undefined,
		onCommit
	}: {
		/** What it is, for a screen reader: `Shadows lightness`. */
		name: string;
		value: number;
		spec: RowSpec;
		disabled?: boolean;
		scope?: S;
		/** A value in words, for a screen reader; the number as shown when none is given. */
		words?: (v: number) => string;
		/** The value a gesture ended on, or the rest value: write it, to `scope`. One call per gesture. */
		onCommit: (v: number, scope: S) => unknown;
	} = $props();

	let preview = $state<number | null>(null);
	const shown = $derived(preview ?? value);
	const pending = new Pending<S>();
	const now = () => ({ scope: scope as S, value, spec });
	let writing = 0;

	function move(v: number) {
		pending.begin(scope as S, value, spec);
		preview = v;
	}
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
	const commit = (v: number) => write(v, pending.end(now).scope);
	function cancel() {
		pending.drop();
		preview = null;
	}
	function reset() {
		pending.drop();
		preview = null;
		const to = resetTo(value, spec);
		if (to !== null) void write(to, scope as S);
	}
</script>

<MixSlider
	orientation="horizontal"
	solid
	value={shown}
	toPos={(v) => toPos(v, spec)}
	fromPos={(p) => fromPos(p, spec)}
	step={(v, key) => stepBy(v, key, spec)}
	label={name}
	valueText={(v) => words?.(v) ?? readout(v, spec)}
	tip="{name} {readout(shown, spec)} — drag the handle, or use the arrow keys{neutralOf(spec) === null ? '' : '; double-click to reset'}"
	detent={tickPos(spec)}
	fillFrom={fillOrigin(spec)}
	accent="var(--kerf-500)"
	{disabled}
	onpreview={move}
	oncommit={commit}
	oncancel={cancel}
	onreset={reset}
/>
