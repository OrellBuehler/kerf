<script lang="ts">
	// The playhead's timecode, and the place to type one (`playback.goToTimecode` focuses it). It reads
	// the way an NLE's timecode field does (`timecode.ts` `parseTimecode`): `1000` is ten seconds in,
	// `1:00:00` a minute, `+15` fifteen frames on, `-1.00` a second back. Enter goes there; Escape, Tab
	// or a click elsewhere leaves the playhead where it was.
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { settings } from '$lib/settings.svelte';
	import { frameTime } from '$lib/frames';
	import { formatTimecode, parseTimecode, timecodeFrameAt, timecodeRate } from '$lib/timecode';
	import { tip } from '$lib/tip';

	let {
		entry,
		size = 12,
		weight = 600,
		describedby
	}: { entry: 'timeline' | 'preview'; size?: number; weight?: number; describedby?: string } = $props();

	let input = $state<HTMLInputElement | null>(null);
	let editing = $state(false);
	let draft = $state('');
	let invalid = $state(false);
	let fresh = false;

	const shown = $derived(formatTimecode(ui.time, editor.fps));
	const dropFrame = $derived(timecodeRate(editor.fps).drop > 0);

	function begin() {
		editing = true;
		draft = shown;
		invalid = false;
		fresh = true;
		queueMicrotask(() => input?.select());
	}

	function end() {
		editing = false;
		invalid = false;
	}

	function commit() {
		const frame = parseTimecode(draft, timecodeRate(editor.fps), timecodeFrameAt(ui.time, editor.fps));
		if (frame === null) {
			invalid = true;
			return;
		}
		ui.seek(frameTime(frame, editor.fps));
		end();
		input?.blur();
	}

	function onkeydown(e: KeyboardEvent) {
		fresh = false;
		if (e.key === 'Enter') {
			e.preventDefault();
			commit();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			end();
			input?.blur();
		}
	}
</script>

<input
	bind:this={input}
	data-timecode-entry={entry}
	type="text"
	inputmode="numeric"
	spellcheck="false"
	autocomplete="off"
	aria-label={entry === 'timeline' ? 'Timeline playhead timecode' : 'Transport playhead timecode'}
	aria-describedby={describedby}
	aria-invalid={invalid}
	use:tip={settings.tip(
		'Go to timecode',
		'playback.goToTimecode',
		`${Number(editor.fps.toFixed(3))} fps · ${dropFrame ? 'drop-frame' : 'non-drop'} · type 1000 for ten seconds, +15 for fifteen frames on`
	)}
	value={editing ? draft : shown}
	oninput={(e) => {
		draft = e.currentTarget.value;
		invalid = false;
	}}
	onfocus={begin}
	onmouseup={(e) => {
		// The click that focused it would put the caret where it landed and drop the selection.
		if (fresh) e.preventDefault();
		fresh = false;
	}}
	onblur={end}
	{onkeydown}
	class="tc"
	style="width:12ch;font-size:{size}px;font-weight:{weight};color:{invalid ? 'var(--danger)' : 'var(--kerf-300)'}"
/>

<style>
	.tc {
		box-sizing: content-box;
		padding: 1px 3px;
		margin: 0 -3px;
		font-family: var(--font-mono);
		font-variant-numeric: tabular-nums;
		background: transparent;
		border: var(--line-width) solid transparent;
		border-radius: var(--radius-xs);
		cursor: text;
		outline: none;
	}
	.tc:hover {
		border-color: var(--border-subtle);
	}
	.tc:focus {
		background: var(--surface-inset);
		border-color: var(--border-strong);
		outline: var(--focus-outline);
		outline-offset: var(--focus-offset);
	}
</style>
