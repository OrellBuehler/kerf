<script lang="ts">
	/* What is drawn and grabbed *on* a timeline clip, beyond its body: the volume
	 * line, the fade ramps and their handles, the transform keyframes, and the trim
	 * edges with their halos. A child of the clip button, so the clip's own drag /
	 * trim / razor handling in `Timeline.svelte` keeps the body and this component
	 * keeps everything else.
	 *
	 * Where the hit areas go, so they never fight the clip:
	 *  - the top `HANDLE_ZONE` px of a clip is for the fade handles only, and the
	 *    volume line's travel stays below it (`lineTravel`);
	 *  - the trim edges are the 6 px strips down both sides, below that zone; the
	 *    volume line's grab band stops short of them;
	 *  - none of it is hit-testable until the clip is hovered or selected (an
	 *    invisible 10 px band across every clip would change where a plain drag
	 *    starts), and none of it exists under any tool but Select: the razor cuts
	 *    anywhere, and roll / slip / slide own the whole clip body;
	 *  - a locked track keeps its keyframes clickable (that only seeks) and loses
	 *    the rest.
	 *
	 * Every gesture is `beginDrag` — pointer capture, Escape / cancel / blur abandon
	 * — shows its value live without writing anything, and writes ONE edit on
	 * release, through the callbacks. The live value is held until that edit
	 * settles, so the line does not flick back to the old value for the round trip.
	 *
	 * Fades apply to picture and sound alike (the export fades both), so every clip
	 * gets fade handles; only a clip whose asset has an audio stream gets a volume
	 * line. */
	import type { Keyframe, Clip } from '$lib/types';
	import { clipDuration } from '$lib/types';
	import { beginDrag } from '$lib/drag';
	import { quantizeFade } from '$lib/frames';
	import { gainLabel } from '$lib/mixer';
	import { dragGain, lineTravel, lineY } from '$lib/volume-line';

	let {
		clip,
		width,
		pxPerSec,
		fps,
		sound,
		selected,
		locked,
		tooled,
		trimEdge = null,
		onlive,
		onselect,
		onvolume,
		onfade,
		onseek,
		onedge
	}: {
		clip: Clip;
		/** The clip's width on the timeline, css px. */
		width: number;
		pxPerSec: number;
		/** The timeline's frame rate: fades land on frames. */
		fps: number;
		/** Whether the clip's asset has an audio stream — what a volume line is for. */
		sound: boolean;
		selected: boolean;
		locked: boolean;
		/** A tool other than Select is active (razor, roll, slip, slide): it owns the
		 *  whole clip, so nothing here is grabbable. */
		tooled: boolean;
		/** The edge being trimmed right now, so its halo stays lit under the pointer. */
		trimEdge?: 'l' | 'r' | null;
		/** Told the clip's volume while the line is being dragged, `null` when it is not —
		 *  so the waveform can follow the line before the edit is written. */
		onlive: (volume: number | null) => void;
		onselect: () => void;
		onvolume: (volume: number) => unknown;
		onfade: (which: 'in' | 'out', seconds: number) => unknown;
		onseek: (time: number) => void;
		onedge: (e: PointerEvent, edge: 'l' | 'r') => void;
	} = $props();

	/** Top strip of a clip reserved for the fade handles, px. */
	const HANDLE_ZONE = 14;
	/** Width of the trim strips down each side, px — as the clip's own were. */
	const EDGE = 6;
	/** A handle's hit area, px (the square drawn inside it is smaller). */
	const HANDLE = 14;
	/** Pointer travel before a press becomes a drag, px. */
	const SLOP = 2;

	let root = $state<HTMLElement | null>(null);
	let height = $state(0);
	let busy = $state(false);
	/** A fade length while it is being dragged (and until its edit lands). */
	let liveFade = $state<{ in?: number; out?: number }>({});
	/** The volume while its line is being dragged (and until its edit lands). */
	let liveVolume = $state<number | null>(null);
	let announced: number | null = null;
	$effect(() => {
		const v = liveVolume;
		if (v === announced) return;
		announced = v;
		onlive(v);
	});
	// A clip removed mid-drag must not leave its live value behind.
	$effect(() => () => {
		if (announced !== null) onlive(null);
	});
	let readout = $state<{ x: number; y: number; text: string } | null>(null);
	let cancel: (() => void) | null = null;

	// A clip removed mid-gesture gives the gesture up rather than leaving its listeners.
	$effect(() => () => cancel?.());

	const dur = $derived(clipDuration(clip));
	const roomy = $derived(width >= 44 && height >= 28);
	const interactive = $derived(!locked && !tooled);
	const fadeIn = $derived(liveFade.in ?? clip.fade_in ?? 0);
	const fadeOut = $derived(liveFade.out ?? clip.fade_out ?? 0);
	const fadeInPx = $derived(Math.min(fadeIn * pxPerSec, width));
	const fadeOutPx = $derived(Math.min(fadeOut * pxPerSec, width));
	const volume = $derived(liveVolume ?? clip.volume ?? 1);
	const travel = $derived(lineTravel(height));
	const lineAt = $derived(lineY(volume, height));
	const keyframes = $derived((clip.keyframes ?? []).filter((k) => k.time >= 0 && k.time <= dur));

	const fmtSeconds = (s: number) => `${s.toFixed(2)} s`;

	function show(e: PointerEvent, text: string) {
		const box = root?.getBoundingClientRect();
		if (!box) return;
		readout = { x: Math.min(Math.max(e.clientX - box.left + 12, 4), Math.max(4, width - 76)), y: e.clientY - box.top, text };
	}

	/** Hold a live value until the edit that makes it real has settled. */
	const hold = (p: unknown, release: () => void) => void Promise.resolve(p).finally(release);

	// ---- volume line ----------------------------------------------------------

	function onVolumeDown(e: PointerEvent) {
		if (e.button !== 0 || !interactive) return;
		e.stopPropagation();
		e.preventDefault();
		onselect();
		const startY = e.clientY;
		const start = clip.volume ?? 1;
		const range = travel.range;
		let moved = false;
		let value = start;
		busy = true;
		cancel = beginDrag(e, {
			move(ev) {
				const dy = ev.clientY - startY;
				if (!moved && Math.abs(dy) < SLOP) return;
				moved = true;
				value = dragGain(start, dy, range);
				liveVolume = value;
				show(ev, gainLabel(value));
			},
			commit() {
				busy = false;
				readout = null;
				if (moved && value !== start) hold(onvolume(value), () => (liveVolume = null));
				else liveVolume = null;
			},
			abandon() {
				busy = false;
				readout = null;
				liveVolume = null;
			}
		});
	}

	function onVolumeReset(e: MouseEvent) {
		e.stopPropagation();
		if (!interactive || (clip.volume ?? 1) === 1) return;
		liveVolume = 1;
		hold(onvolume(1), () => (liveVolume = null));
	}

	// ---- fades ---------------------------------------------------------------

	function onFadeDown(e: PointerEvent, which: 'in' | 'out') {
		if (e.button !== 0 || !interactive) return;
		e.stopPropagation();
		e.preventDefault();
		onselect();
		const startX = e.clientX;
		const start = (which === 'in' ? clip.fade_in : clip.fade_out) ?? 0;
		// Each fade may take whatever the other leaves of the clip.
		const room = dur - ((which === 'in' ? clip.fade_out : clip.fade_in) ?? 0);
		let moved = false;
		let value = start;
		busy = true;
		cancel = beginDrag(e, {
			move(ev) {
				const dx = ev.clientX - startX;
				if (!moved && Math.abs(dx) < SLOP) return;
				moved = true;
				// One rounding, from where the pointer is — never from the last frame's.
				value = quantizeFade(start + (which === 'in' ? dx : -dx) / pxPerSec, room, fps);
				liveFade = { ...liveFade, [which]: value };
				show(ev, fmtSeconds(value));
			},
			commit() {
				busy = false;
				readout = null;
				if (moved && value !== start) hold(onfade(which, value), () => (liveFade = {}));
				else liveFade = {};
			},
			abandon() {
				busy = false;
				readout = null;
				liveFade = {};
			}
		});
	}

	function onFadeClear(e: MouseEvent, which: 'in' | 'out') {
		e.stopPropagation();
		if (!interactive || ((which === 'in' ? clip.fade_in : clip.fade_out) ?? 0) === 0) return;
		liveFade = { ...liveFade, [which]: 0 };
		hold(onfade(which, 0), () => (liveFade = {}));
	}

	// ---- keyframes -----------------------------------------------------------

	function onKeyframeClick(e: MouseEvent, k: Keyframe) {
		e.stopPropagation();
		onselect();
		onseek(clip.timeline_start + k.time);
	}

	const stop = (e: Event) => e.stopPropagation();

	const stamp = (s: number) => `${Math.floor(s / 60)}:${(s % 60).toFixed(2).padStart(5, '0')}`;
</script>

<div
	bind:this={root}
	bind:clientHeight={height}
	class="ov"
	class:active={selected || busy}
	class:interactive
	style="width:{width}px"
>
	{#if (fadeInPx > 0 || fadeOutPx > 0) && height > 0}
		<!-- The gain ramps, drawn as what they take away: the corner above the ramp. -->
		<svg {width} {height} viewBox="0 0 {width} {height}" class="ramps" aria-hidden="true">
			{#if fadeInPx > 0}
				<polygon points="0,0 {fadeInPx},0 0,{height}" class="shade" />
				<line x1="0" y1={height} x2={fadeInPx} y2="0" class="ramp" />
			{/if}
			{#if fadeOutPx > 0}
				<polygon points="{width},0 {width - fadeOutPx},0 {width},{height}" class="shade" />
				<line x1={width} y1={height} x2={width - fadeOutPx} y2="0" class="ramp" />
			{/if}
		</svg>
	{/if}

	{#if sound && roomy}
		<div class="vline" class:bent={volume !== 1} style="top:{lineAt}px"></div>
		<div
			role="presentation"
			class="grab vgrab"
			title="Volume {gainLabel(volume)} — drag to change, double-click for 0 dB"
			onpointerdown={onVolumeDown}
			ondblclick={onVolumeReset}
			onclick={stop}
			style="top:{lineAt - 5}px;left:{EDGE}px;right:{EDGE}px"
		></div>
	{/if}

	{#if roomy}
		<div
			role="presentation"
			class="grab fgrab"
			title="Fade in {fmtSeconds(fadeIn)} — drag to change, double-click to clear"
			onpointerdown={(e) => onFadeDown(e, 'in')}
			ondblclick={(e) => onFadeClear(e, 'in')}
			onclick={stop}
			style="left:{Math.max(0, Math.min(fadeInPx, width - HANDLE))}px"
		>
			<span class="square"></span>
		</div>
		<div
			role="presentation"
			class="grab fgrab"
			title="Fade out {fmtSeconds(fadeOut)} — drag to change, double-click to clear"
			onpointerdown={(e) => onFadeDown(e, 'out')}
			ondblclick={(e) => onFadeClear(e, 'out')}
			onclick={stop}
			style="right:{Math.max(0, Math.min(fadeOutPx, width - HANDLE))}px"
		>
			<span class="square"></span>
		</div>
	{/if}

	{#each keyframes as k, i (i)}
		<div
			role="presentation"
			class="kf"
			class:blocked={tooled}
			title="Keyframe at {stamp(k.time)} — click to go there"
			onpointerdown={stop}
			onclick={(e) => onKeyframeClick(e, k)}
			style="left:{Math.min(Math.max(k.time * pxPerSec, HANDLE / 2), Math.max(HANDLE / 2, width - HANDLE / 2)) - HANDLE / 2}px;top:{Math.max(0, height - HANDLE - 2)}px"
		>
			<span class="diamond"></span>
		</div>
	{/each}

	{#if !tooled && width > 24}
		<div
			role="presentation"
			class="edge l"
			class:locked
			onpointerdown={(e) => onedge(e, 'l')}
			style="top:{HANDLE_ZONE}px"
		></div>
		<div class="halo l" class:on={trimEdge === 'l'} class:locked></div>
		<div
			role="presentation"
			class="edge r"
			class:locked
			onpointerdown={(e) => onedge(e, 'r')}
			style="top:{HANDLE_ZONE}px"
		></div>
		<div class="halo r" class:on={trimEdge === 'r'} class:locked></div>
	{/if}

	{#if readout}
		<div class="readout" style="left:{readout.x}px;top:{Math.max(2, readout.y - 26)}px">{readout.text}</div>
	{/if}
</div>

<style>
	.ov {
		position: absolute;
		/* `--bw` is the clip's border width, set on the clip: absolute children are
		   placed from its padding box, and the clip's time starts at its border box. */
		left: calc(-1 * var(--bw, 0px));
		top: calc(-1 * var(--bw, 0px));
		bottom: calc(-1 * var(--bw, 0px));
		z-index: 2;
		pointer-events: none;
		user-select: none;
	}

	.ramps {
		position: absolute;
		left: 0;
		top: 0;
		pointer-events: none;
	}
	.shade {
		fill: var(--scrim);
		fill-opacity: 0.5;
	}
	.ramp {
		stroke: var(--kerf-300);
		stroke-opacity: 0.7;
		stroke-width: 1;
	}

	/* The volume line: unseen at unity until the clip is hovered or selected, a
	   dim mark when it has been moved off it so a pulled-down clip says so. */
	.vline {
		position: absolute;
		left: 0;
		right: 0;
		height: 0;
		border-top: 1.5px solid var(--kerf-300);
		box-shadow: 0 0 0 1px color-mix(in srgb, var(--scrim) 45%, transparent);
		opacity: 0;
		pointer-events: none;
	}
	.vline.bent {
		opacity: 0.55;
	}
	.ov.active .vline,
	:global(.kclip:hover) .vline {
		opacity: 0.95;
	}

	/* Everything grabbable is inert until the clip is hovered or selected. */
	.grab {
		position: absolute;
		opacity: 0;
		pointer-events: none;
		touch-action: none;
	}
	.ov.interactive.active .grab,
	:global(.kclip:hover) .ov.interactive .grab {
		opacity: 1;
		pointer-events: auto;
	}
	.vgrab {
		height: 10px;
		cursor: ns-resize;
	}
	.fgrab {
		top: 0;
		width: 14px;
		height: 14px;
		cursor: ew-resize;
	}
	.square {
		position: absolute;
		left: 2px;
		top: 2px;
		width: 10px;
		height: 10px;
		border-radius: 2px;
		background: var(--kerf-400);
		border: var(--line-width) solid var(--surface-void);
		box-sizing: border-box;
	}
	.fgrab:hover .square {
		background: var(--kerf-200);
	}

	.kf {
		position: absolute;
		width: 14px;
		height: 14px;
		cursor: pointer;
		pointer-events: auto;
		touch-action: none;
	}
	.kf.blocked {
		pointer-events: none;
	}
	.diamond {
		position: absolute;
		left: 3px;
		top: 3px;
		width: 8px;
		height: 8px;
		transform: rotate(45deg);
		background: var(--kerf-300);
		border: var(--line-width) solid var(--surface-void);
		box-sizing: border-box;
	}
	.kf:hover .diamond {
		background: var(--kerf-200);
	}

	.edge {
		position: absolute;
		bottom: 0;
		width: 6px;
		cursor: ew-resize;
		pointer-events: auto;
		touch-action: none;
		z-index: 3;
	}
	.edge.locked {
		cursor: not-allowed;
	}
	.edge.l {
		left: 0;
	}
	.edge.r {
		right: 0;
	}

	/* Trim halos: the edge zone lit on hover and for as long as it is being trimmed. */
	.halo {
		position: absolute;
		top: 0;
		bottom: 0;
		width: 12px;
		opacity: 0;
		pointer-events: none;
		transition: opacity var(--dur-fast) var(--ease-out);
	}
	.halo.l {
		left: 0;
		border-left: 2px solid var(--kerf-400);
		background: linear-gradient(to right, color-mix(in srgb, var(--kerf-400) 45%, transparent), transparent);
	}
	.halo.r {
		right: 0;
		border-right: 2px solid var(--kerf-400);
		background: linear-gradient(to left, color-mix(in srgb, var(--kerf-400) 45%, transparent), transparent);
	}
	.edge:hover + .halo:not(.locked),
	.halo.on {
		opacity: 1;
	}

	.readout {
		position: absolute;
		padding: 2px 6px;
		border-radius: 3px;
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		line-height: 1.2;
		white-space: nowrap;
		color: var(--text-primary);
		background: color-mix(in srgb, var(--surface-void) 88%, transparent);
		border: var(--line-width) solid var(--border-strong);
		pointer-events: none;
	}
</style>
