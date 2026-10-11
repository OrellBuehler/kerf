<script lang="ts">
	/* The transport's scrub bar: the cut on a rail, the playhead on it, the in / out range behind
	 * it. A 16 px hit area around a 4 px rail (a rail you have to hit to the pixel is a bar nobody
	 * drags), and a drag that moves the playhead *as it goes* — through `beginDrag`, so the
	 * pointer is captured, Escape / pointercancel / a lost capture / a window blur put the playhead
	 * back where it was, and the playhead is moved at most once per frame (`scrub.ts`:
	 * the picture under it is an FFmpeg composite, so the pointer's rate must not be the playhead's).
	 * Playback is paused while the bar is held and resumes at the end of the gesture, whichever
	 * end it has: a seek mid-play restarts the audio each time. */
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { beginDrag } from '$lib/drag';
	import { windowOf } from '$lib/realm';
	import { formatTimecode } from '$lib/timecode';
	import { chipLeft, frameLimited, markerTicks, scrubFraction, scrubRange, scrubTime } from '$lib/scrub';

	let { duration, empty }: { duration: number; empty: boolean } = $props();

	let held = $state(false);
	const at = $derived(empty ? 0 : scrubFraction(ui.time, duration));
	const range = $derived(scrubRange(ui.markIn, ui.markOut, duration));
	/** The markers on the cut, as ticks along the rail. */
	const ticks = $derived(empty ? [] : markerTicks(editor.markers, duration));

	/** Where the pointer is along the bar, px — the time under it is on a chip above the bar, so a scrub
	 *  can be aimed (and read) before it is pressed. */
	let hoverX = $state<number | null>(null);
	let barW = $state(0);
	const CHIP_W = 84;
	const hoverTime = $derived(hoverX === null || empty ? null : scrubTime(hoverX, 0, barW, duration));

	function onHover(e: PointerEvent) {
		if (empty) return;
		hoverX = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
	}

	function onDown(e: PointerEvent) {
		if (e.button !== 0) return;
		// No text selection, and no focus — on an empty cut too, where a click still focuses a
		// tabindex=-1 element: a bar left focused after a click would own the arrow keys and Space (the
		// page leaves a slider's keys to it), and the transport shortcuts would go quiet.
		e.preventDefault();
		if (empty) return;
		const el = e.currentTarget as HTMLElement;
		const win = windowOf(el);
		const from = ui.time;
		const resume = ui.playing;
		const rate = ui.rate;
		if (resume) ui.pause();
		const timeAt = (ev: PointerEvent) => scrubTime(ev.clientX, el.getBoundingClientRect().left, el.clientWidth, duration);
		const seek = frameLimited((t: number) => ui.seek(t), {
			request: (cb) => win.requestAnimationFrame(() => cb()),
			cancel: (id) => win.cancelAnimationFrame(id)
		});
		const done = () => {
			held = false;
			// An engine that focuses on a canceled pointerdown anyway: only a keyboard focus stays.
			try {
				if (!el.matches(':focus-visible')) el.blur();
			} catch {
				el.blur();
			}
			// Letting go at the very end must not start playback over again: play() restarts a cut that has played out.
			if (resume && ui.time < editor.duration) ui.play(rate);
		};
		held = true;
		ui.seek(timeAt(e));
		beginDrag(e, {
			move: (ev) => seek.push(timeAt(ev)),
			commit: (ev) => {
				seek.cancel();
				ui.seek(timeAt(ev));
				done();
			},
			abandon: () => {
				seek.cancel();
				ui.seek(from);
				done();
			}
		});
	}

	/** The keys a slider operates are its own (the page stands down for them): arrows step a frame, Shift a
	 *  second — the steps the page's own frame keys take —, Home / End go to the ends, and Space still plays. */
	function onKey(e: KeyboardEvent) {
		if (empty || e.ctrlKey || e.metaKey || e.altKey) return;
		// One press, one action, as the page's own play key (`allowsRepeat`).
		if (e.repeat && e.key === ' ') {
			e.preventDefault();
			return;
		}
		const step = e.shiftKey ? 1 : 1 / editor.fps;
		switch (e.key) {
			case 'ArrowLeft':
			case 'ArrowDown':
				ui.seek(ui.time - step);
				break;
			case 'ArrowRight':
			case 'ArrowUp':
				ui.seek(ui.time + step);
				break;
			case 'Home':
				ui.seek(0);
				break;
			case 'End':
				ui.seek(duration);
				break;
			case ' ':
				ui.togglePlay();
				break;
			default:
				return;
		}
		e.preventDefault();
	}
</script>

<div
	role="slider"
	tabindex={empty ? -1 : 0}
	aria-label="Seek"
	aria-valuemin={0}
	aria-valuemax={Math.max(0, duration)}
	aria-valuenow={empty ? 0 : ui.time}
	aria-valuetext={formatTimecode(ui.time, editor.fps)}
	aria-disabled={empty}
	data-scrub
	data-held={held ? '' : undefined}
	bind:clientWidth={barW}
	onpointerdown={onDown}
	onpointermove={onHover}
	onpointerleave={() => (hoverX = null)}
	onkeydown={onKey}
	style="flex:1;min-width:24px;height:16px;position:relative;display:flex;align-items:center;cursor:{empty ? 'default' : 'pointer'};touch-action:none;user-select:none"
>
	<div
		aria-hidden="true"
		data-scrub-rail
		style="position:absolute;left:0;right:0;top:50%;height:4px;margin-top:-2px;border-radius:999px;background:var(--scrub-rail)"
	>
		{#if range}
			<div
				data-scrub-range
				style="position:absolute;top:-2px;bottom:-2px;left:{range.from * 100}%;width:{(range.to - range.from) * 100}%;border-radius:2px;background:color-mix(in srgb,var(--kerf-400) 38%,transparent)"
			></div>
		{/if}
		<div style="position:absolute;inset:0 auto 0 0;width:{at * 100}%;background:var(--kerf-500);border-radius:999px"></div>
	</div>
	{#each ticks as m (m.at)}
		<div
			aria-hidden="true"
			data-scrub-marker
			style="position:absolute;left:{m.at * 100}%;top:50%;width:2px;height:10px;margin:-5px 0 0 -1px;border-radius:1px;background:{m.color || 'var(--agent-400)'};box-shadow:0 0 0 1px var(--surface-app);pointer-events:none"
		></div>
	{/each}
	{#if hoverTime !== null && hoverX !== null}
		<div
			aria-hidden="true"
			data-scrub-chip
			style="position:absolute;bottom:calc(100% + 2px);left:{chipLeft(hoverX, CHIP_W, barW)}px;width:{CHIP_W}px;box-sizing:border-box;text-align:center;padding:1px 0;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-raised);color:var(--text-primary);font-family:var(--font-mono);font-size:11px;line-height:14px;pointer-events:none;z-index:2"
		>
			{formatTimecode(hoverTime, editor.fps)}
		</div>
	{/if}
	<div
		aria-hidden="true"
		data-scrub-thumb
		style="position:absolute;left:{at * 100}%;top:50%;width:{held ? 13 : 11}px;height:{held ? 13 : 11}px;border-radius:50%;background:var(--kerf-400);transform:translate(-50%,-50%);box-shadow:0 0 0 3px var(--surface-app);pointer-events:none"
	></div>
</div>

<style>
	[data-scrub]:focus-visible {
		outline: var(--focus-outline);
		outline-offset: var(--focus-offset);
		border-radius: 4px;
	}
</style>
