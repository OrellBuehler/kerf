<script lang="ts">
	// A progress bar that says what it knows. With a `value` (0–1) it fills to it; without one — a
	// download of unknown size, a stage that reports nothing, a render waiting on its proxy — it is
	// *indeterminate*: a segment sweeps along the track instead of sitting at a made-up 30 %, which
	// read as "a third done" for as long as the wait lasted. Under `prefers-reduced-motion` nothing
	// moves; the whole track is tinted instead. Every bar is a `progressbar` for assistive tech, with
	// `aria-valuenow` only when there is a value (`styles/controls.css`, `.k-progress`).
	let {
		value = null,
		label,
		height = 6
	}: {
		/** The fraction done, 0–1; null when it is not known. */
		value?: number | null;
		/** What is progressing, for the accessible name (`Separating stems`). */
		label: string;
		/** px */
		height?: number;
	} = $props();

	const pct = $derived(value === null ? null : Math.round(Math.min(1, Math.max(0, value)) * 100));
</script>

<div
	class="k-progress"
	role="progressbar"
	aria-label={label}
	aria-valuemin="0"
	aria-valuemax="100"
	aria-valuenow={pct ?? undefined}
	data-indeterminate={pct === null ? '' : undefined}
	style="--k-progress-h:{height}px"
>
	<i style={pct === null ? undefined : `width:${pct}%`}></i>
</div>
