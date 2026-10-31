<script lang="ts">
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { settings } from '#lib/settings.svelte';
	import { beginDrag } from '#lib/drag';
	import { resizeObserverFor } from '#lib/realm';
	import { report } from '#lib/ops';
	import { tip } from '#lib/tip';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { containRect } from '#lib/titles';
	import { keyTime } from '#lib/channels';
	import { DEFAULT_TRANSFORM, clipDuration, type PropertyTrack } from '#lib/types';
	import { layersAt, readout, renderCanvas, snapTargets, type Chrome, type Guide } from '#lib/transform-handles';
	import {
		addKeyTracks,
		dragKey,
		hereKey,
		keyAt,
		keyTimes,
		nearestOnPath,
		pathData,
		pathFrame,
		pathKeys,
		pathTicks,
		positionKeyed,
		samplePath,
		seekTarget,
		snapKeyTime,
		tickPath,
		withTracks,
		type PathKey,
		type Pos
	} from '#lib/motion-path';

	// The selected clip's motion path over the Preview: where its centre goes over the clip's whole
	// duration (the eased curves of position x / y), a dot on each position key and tick marks at equal
	// steps of time, so a slow stretch reads as ticks close together. Click a dot to go to that key, drag
	// it to move the key (one `set_property_tracks` edit on release; Shift keeps to an axis, snapping is
	// the transform handles'; Escape, pointercancel, a lost capture or a blur abandon it), double-click the
	// path to key the position there. While a dot is held the path redraws as the edit would leave it.
	// The maths is `motion-path.ts`.

	let {
		active,
		frameAspect,
		chrome,
		shown = $bindable(false)
	}: {
		/** Paused, not streaming, nothing standing in for the picture, not a previewed proposal. */
		active: boolean;
		frameAspect: number;
		/** The platform chrome the safe-area guides shade, when they are drawn: snap targets too. */
		chrome: Chrome | null;
		/** A path is drawn over the picture (the GPU surface cannot sit above it). */
		shown?: boolean;
	} = $props();

	/** Pointer travel (CSS px) before a press on a dot is a drag and not a click. */
	const TRAVEL = 3;
	/** Half the length of a tick mark (CSS px). */
	const TICK_HALF = 4;

	const canvas = $derived(renderCanvas(editor.timeline, editor.assets));
	const rect = $derived(containRect(canvas.width / canvas.height, frameAspect));
	const layers = $derived(active && ui.motionPath ? layersAt(editor.timeline, editor.assets, canvas, ui.time) : []);
	const lockedTrack = (clipId: string) => editor.timeline.tracks.some((t) => t.locked && t.clips.some((c) => c.id === clipId));

	/** The one selected picture under the playhead, when its position moves. */
	const layer = $derived.by(() => {
		const mine = layers.filter((l) => editor.selectedSet.has(l.clip.id));
		return mine.length === 1 && !lockedTrack(mine[0].clip.id) && positionKeyed(mine[0].clip) ? mine[0] : null;
	});
	const clip = $derived(layer?.clip ?? null);
	const duration = $derived(clip ? clipDuration(clip) : 0);
	const frame = $derived(layer ? pathFrame(canvas, layer.box, layer.pose) : null);

	type Drag = {
		clipId: string;
		time: number;
		tracks: PropertyTrack[];
		pos: Pos;
		guides: Guide[];
		pointer: [number, number];
		moved: boolean;
		committing: boolean;
	};
	let drag = $state<Drag | null>(null);
	let rootEl = $state<HTMLElement | null>(null);
	let cssWidth = $state(0);

	$effect(() => {
		if (!rootEl) return;
		const el = rootEl;
		cssWidth = el.getBoundingClientRect().width;
		const ro = resizeObserverFor(el, (entries) => (cssWidth = entries.at(-1)?.contentRect.width ?? el.clientWidth));
		ro.observe(el);
		return () => ro.disconnect();
	});

	const live = $derived(drag && clip && drag.clipId === clip.id ? drag : null);
	/** The clip as the edit under the pointer would leave it. */
	const drawn = $derived(clip && live ? withTracks(clip, live.tracks) : clip);
	const keys = $derived(drawn && frame ? pathKeys(drawn, frame, duration) : []);
	const samples = $derived(drawn && frame ? samplePath(drawn, frame, duration, editor.fps) : []);
	const line = $derived(pathData(samples));
	const ticks = $derived(
		drawn && frame ? pathTicks(drawn, frame, duration, editor.fps, keys.map((k) => k.time)) : []
	);
	const marks = $derived(cssWidth > 0 ? tickPath(ticks, (TICK_HALF * canvas.width) / cssWidth) : '');
	const here = $derived(clip ? hereKey(keys, keyTime(clip, ui.time), editor.fps) : -1);

	$effect(() => {
		shown = layer !== null;
	});

	const pct = (v: number, of: number) => `${(v / of) * 100}%`;
	const keyName = (c: { timeline_start: number }, k: PathKey) => `Position key at ${timeDisplay.fmt(c.timeline_start + k.time)}`;
	const hint = $derived({
		text: 'Drag to move this position key',
		detail: `Click to go to it; double-click the path to add a key. ${settings.modLabel('shift')} keeps to one axis; ${settings.modLabel('alt')} ${ui.snap ? 'stops' : 'starts'} snapping.`
	});

	function goTo(c: { timeline_start: number }, k: PathKey) {
		ui.seek(seekTarget(c.timeline_start, duration, k.time, editor.fps), { snap: 'exact' });
	}

	/** One backend edit per gesture: the draft stays drawn until the edit has landed. */
	async function write(clipId: string, tracks: PropertyTrack[], d: Drag | null) {
		if (d) drag = { ...d, committing: true };
		try {
			await editor.setPropertyTracks(clipId, tracks);
		} catch (err) {
			report(err);
		} finally {
			if (d) drag = null;
		}
	}

	function begin(e: PointerEvent, key: PathKey) {
		const c = clip;
		const f = frame;
		if (e.button !== 0 || drag || !rootEl || editor.locked || !c || !f) return;
		const r = rootEl.getBoundingClientRect();
		if (r.width <= 0) return;
		e.stopPropagation();
		e.preventDefault();
		ui.pause();
		const k = canvas.width / r.width;
		const from: [number, number] = [e.clientX, e.clientY];
		const targets = snapTargets(canvas, layers.filter((l) => l.clip.id !== c.id).map((l) => l.box), chrome);
		const times = keyTimes(c, duration);
		let d: Drag = {
			clipId: c.id,
			time: key.time,
			tracks: [],
			pos: { pos_x: key.pos_x, pos_y: key.pos_y },
			guides: [],
			pointer: [e.clientX - r.left, e.clientY - r.top],
			moved: false,
			committing: false
		};
		drag = d;
		beginDrag(e, {
			move(ev) {
				if (!d.moved && Math.hypot(ev.clientX - from[0], ev.clientY - from[1]) < TRAVEL) return;
				const snap = { on: ui.snap !== ev.altKey, targets, threshold: 6 * k };
				const got = dragKey(c, key, f, (ev.clientX - from[0]) * k, (ev.clientY - from[1]) * k, snap, ev.shiftKey, times);
				d = { ...d, ...got, moved: true, pointer: [ev.clientX - r.left, ev.clientY - r.top] };
				drag = d;
			},
			commit() {
				if (!d.moved) {
					drag = null;
					goTo(c, key);
				} else if (d.tracks.length === 0) {
					drag = null;
				} else {
					void write(c.id, d.tracks, d);
				}
			},
			abandon() {
				drag = null;
			}
		});
	}

	function addKey(e: MouseEvent) {
		const c = clip;
		if (!c || !rootEl || drag || editor.locked) return;
		const r = rootEl.getBoundingClientRect();
		if (r.width <= 0) return;
		const k = canvas.width / r.width;
		const at = { x: (e.clientX - r.left) * k, y: (e.clientY - r.top) * k };
		if (keyAt(keys, at, 8 * k) >= 0) return;
		const near = nearestOnPath(samples, at);
		if (!near) return;
		const tracks = addKeyTracks(c, snapKeyTime(c.timeline_start, near.t, duration, editor.fps));
		if (tracks.length === 0) return;
		e.preventDefault();
		e.stopPropagation();
		void write(c.id, tracks, null);
	}
</script>

{#if layer && clip && frame}
	<div
		bind:this={rootEl}
		role="presentation"
		data-motion-path
		style="position:absolute;left:{rect.left}%;top:{rect.top}%;width:{rect.width}%;height:{rect.height}%;pointer-events:none"
	>
		<svg
			viewBox="0 0 {canvas.width} {canvas.height}"
			preserveAspectRatio="none"
			aria-hidden="true"
			style="position:absolute;inset:0;width:100%;height:100%;overflow:visible;pointer-events:none"
		>
			<path d={line} fill="none" stroke="color-mix(in srgb,var(--scrim) 55%,transparent)" stroke-width="4" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
			<path d={line} data-path-line fill="none" stroke="var(--text-on-video)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
			<path d={marks} data-path-ticks fill="none" stroke="var(--text-on-video)" stroke-width="1" stroke-opacity="0.85" vector-effect="non-scaling-stroke" />
			<path
				d={line}
				data-path-hit
				role="presentation"
				fill="none"
				stroke="transparent"
				stroke-width="12"
				stroke-linecap="round"
				stroke-linejoin="round"
				vector-effect="non-scaling-stroke"
				ondblclick={addKey}
				style="pointer-events:stroke"
			/>
		</svg>
		{#if live}
			{#each live.guides as g (`${g.axis}${g.at}`)}
				<div
					aria-hidden="true"
					data-snap-guide={g.axis}
					style="position:absolute;pointer-events:none;{g.axis === 'x'
						? `left:${pct(g.at, canvas.width)};top:0;bottom:0;border-left:1px dashed`
						: `top:${pct(g.at, canvas.height)};left:0;right:0;border-top:1px dashed`} color-mix(in srgb,var(--text-on-video) 85%,transparent)"
				></div>
			{/each}
		{/if}
		{#each keys as k, i (k.time)}
			{@const on = i === here}
			{@const held = live?.time === k.time}
			<button
				type="button"
				data-path-key={i}
				data-time={String(k.time)}
				data-here={on ? '' : undefined}
				aria-label={keyName(clip, k)}
				use:tip={live ? undefined : hint}
				onpointerdown={(e) => begin(e, k)}
				onclick={(e) => {
					if (e.detail === 0) goTo(clip, k);
				}}
				style="position:absolute;left:{pct(k.x, canvas.width)};top:{pct(k.y, canvas.height)};width:{on || held ? 13 : 10}px;height:{on || held ? 13 : 10}px;padding:0;box-sizing:border-box;transform:translate(-50%,-50%);border-radius:50%;background:{on || held ? 'var(--kerf-400)' : 'var(--surface-app)'};border:2px solid {on || held ? 'var(--text-on-video)' : 'var(--kerf-400)'};box-shadow:0 0 0 1px color-mix(in srgb,var(--scrim) 55%,transparent);cursor:{live ? 'grabbing' : 'grab'};pointer-events:auto;touch-action:none"
			></button>
		{/each}
		{#if live?.moved}
			<span
				data-path-readout
				style="position:absolute;left:{live.pointer[0] + 14}px;top:{live.pointer[1] + 14}px;padding:1px 6px;border-radius:var(--radius-sm);font-size:10px;font-weight:600;font-family:var(--font-mono);white-space:nowrap;pointer-events:none;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) 70%,transparent)"
				>{readout('move', { ...DEFAULT_TRANSFORM, ...live.pos })} · key</span
			>
		{/if}
	</div>
{/if}
