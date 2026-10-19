<script lang="ts">
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { settings } from '#lib/settings.svelte';
	import { onWindow } from '#lib/window-events';
	import { report } from '#lib/ops';
	import { tip } from '#lib/tip';
	import { containRect } from '#lib/titles';
	import { isKeyed } from '#lib/channels';
	import type { Property, Transform } from '#lib/types';
	import {
		changed,
		croppable,
		cropPose,
		geometryOf,
		layersAt,
		movePose,
		pictureBox,
		readout,
		ROTATE_STEP,
		renderCanvas,
		resizeCursor,
		rotatePose,
		scalePose,
		snapTargets,
		sweep,
		topLayerAt,
		type Box,
		type Chrome,
		type Edge,
		type Guide,
		type Layer
	} from '#lib/transform-handles';

	// A clip's picture in the Preview, as a box the export's own geometry puts it in: drag it to move
	// it, a corner to scale it about its centre, the knob to turn it, an edge to crop it. A click on a
	// picture selects its clip (the topmost one under the pointer). Nothing is written while the
	// pointer is down — the box is a ghost of the pose, drawn exactly where the export will place it —
	// and one `set_transform_at` lands on release: a key at the playhead for a number that is keyed,
	// its static value otherwise. Escape, pointercancel, a lost capture or a blur abandon the gesture.

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
		/** A box is drawn over the picture (the GPU surface cannot sit above it). */
		shown?: boolean;
	} = $props();

	const canvas = $derived(renderCanvas(editor.timeline, editor.assets));
	const rect = $derived(containRect(canvas.width / canvas.height, frameAspect));
	const layers = $derived(active ? layersAt(editor.timeline, editor.assets, canvas, ui.time) : []);
	const lockedTrack = (clipId: string) => editor.timeline.tracks.some((t) => t.locked && t.clips.some((c) => c.id === clipId));
	const selected = $derived(layers.find((l) => l.clip.id === editor.selectedClipId && !lockedTrack(l.clip.id)) ?? null);

	type Kind = 'move' | 'scale' | 'rotate' | 'crop';
	type Drag = {
		kind: Kind;
		edge?: Edge;
		layer: Layer;
		from: [number, number];
		last: [number, number];
		swept: number;
		/** Canvas px per CSS px. */
		k: number;
		origin: { left: number; top: number };
		patch: Partial<Transform>;
		ghost: Box | null;
		guides: Guide[];
		pointer: [number, number];
		moved: boolean;
		committing: boolean;
	};
	let drag = $state<Drag | null>(null);
	let rootEl = $state<HTMLElement | null>(null);
	let capture: { el: Element; id: number } | null = null;

	const live = $derived(drag && selected && drag.layer.clip.id === selected.clip.id ? drag : null);
	const box = $derived(live?.ghost ?? selected?.box ?? null);
	const crop = $derived(selected ? croppable(selected.clip, canvas) : false);
	$effect(() => {
		shown = box !== null;
	});

	function release() {
		if (capture) {
			try {
				capture.el.releasePointerCapture(capture.id);
			} catch {
				// already released
			}
		}
		capture = null;
	}

	/** Abandon the gesture without writing anything. */
	function cancel() {
		if (!drag || drag.committing) return;
		drag = null;
		release();
	}

	function toCanvas(d: Pick<Drag, 'k' | 'origin'>, e: PointerEvent): [number, number] {
		return [(e.clientX - d.origin.left) * d.k, (e.clientY - d.origin.top) * d.k];
	}

	function begin(e: PointerEvent, layer: Layer, kind: Kind, edge?: Edge) {
		if (e.button !== 0 || drag || !rootEl || editor.locked) return;
		e.stopPropagation();
		e.preventDefault();
		ui.pause();
		const r = rootEl.getBoundingClientRect();
		if (r.width <= 0) return;
		const geom = { k: canvas.width / r.width, origin: { left: r.left, top: r.top } };
		const at = toCanvas(geom, e);
		drag = { kind, edge, layer, from: at, last: at, swept: 0, ...geom, patch: {}, ghost: null, guides: [], pointer: [e.clientX - r.left, e.clientY - r.top], moved: false, committing: false };
		try {
			(e.currentTarget as Element).setPointerCapture(e.pointerId);
			capture = { el: e.currentTarget as Element, id: e.pointerId };
		} catch {
			// best-effort: the window blur and buttons checks are the fallback
		}
	}

	function onHitDown(e: PointerEvent) {
		if (e.button !== 0 || drag || !rootEl || editor.locked) return;
		const r = rootEl.getBoundingClientRect();
		if (r.width <= 0) return;
		const k = canvas.width / r.width;
		const hit = topLayerAt(layers, (e.clientX - r.left) * k, (e.clientY - r.top) * k, (c) => lockedTrack(c.id));
		if (!hit) return;
		if (editor.selectedClipId !== hit.clip.id) editor.selectClip(hit.clip.id);
		begin(e, hit, 'move');
	}

	function onMove(e: PointerEvent) {
		const d = drag;
		if (!d || d.committing) return;
		if ((e.buttons & 1) === 0) {
			cancel();
			return;
		}
		const at = toCanvas(d, e);
		const snap = { on: ui.snap !== e.altKey, targets: snapTargets(canvas, layers.filter((l) => l.clip.id !== d.layer.clip.id).map((l) => l.box), chrome), threshold: 6 * d.k };
		const start = d.layer.pose;
		const b = d.layer.box;
		let swept = d.swept;
		let m;
		switch (d.kind) {
			case 'move':
				m = movePose(start, b, at[0] - d.from[0], at[1] - d.from[1], canvas, snap, e.shiftKey);
				break;
			case 'scale':
				m = scalePose(start, b, d.from, at, snap);
				break;
			case 'rotate':
				swept += sweep(b, d.last, at);
				m = rotatePose(start, swept, snap.on, e.shiftKey);
				break;
			case 'crop':
				m = cropPose(start, d.layer, d.edge!, at, canvas, snap);
				break;
		}
		const pose = { ...start, ...m.patch };
		const g = geometryOf(d.layer.src, d.layer.grid, canvas, pose);
		const travel = Math.hypot(at[0] - d.from[0], at[1] - d.from[1]) / d.k;
		drag = {
			...d,
			last: at,
			swept,
			patch: m.patch,
			guides: m.guides,
			ghost: g ? pictureBox(g) : d.ghost,
			pointer: [e.clientX - d.origin.left, e.clientY - d.origin.top],
			moved: d.moved || (d.kind === 'rotate' ? Math.abs(swept) >= 0.5 : travel >= 3)
		};
	}

	/** One backend edit per gesture: the ghost keeps the pose it was let go at until the edit has landed. */
	async function onUp() {
		const d = drag;
		if (!d || d.committing) return;
		release();
		const patch = d.moved ? changed(d.layer.pose, d.patch) : {};
		if (Object.keys(patch).length === 0) {
			drag = null;
			return;
		}
		drag = { ...d, committing: true };
		try {
			await editor.setTransformAt(d.layer.clip.id, Math.round(d.layer.local * 1000) / 1000, patch);
		} catch (err) {
			report(err);
		} finally {
			drag = null;
		}
	}

	function onLostCapture() {
		if (drag && !drag.committing) cancel();
	}

	const pct = (v: number, of: number) => `${(v / of) * 100}%`;
	const keyedHere = $derived(
		live ? (Object.keys(changed(live.layer.pose, live.patch)) as Property[]).some((p) => isKeyed(live.layer.clip, p)) : false
	);
	const CORNERS = [
		{ at: 'left:0;top:0', base: 225 },
		{ at: 'left:100%;top:0', base: 315 },
		{ at: 'left:0;top:100%', base: 135 },
		{ at: 'left:100%;top:100%', base: 45 }
	];
	const EDGES: { edge: Edge; at: string; base: number; size: string }[] = [
		{ edge: 'left', at: 'left:0;top:50%', base: 180, size: 'width:6px;height:16px' },
		{ edge: 'right', at: 'left:100%;top:50%', base: 0, size: 'width:6px;height:16px' },
		{ edge: 'top', at: 'left:50%;top:0', base: 270, size: 'width:16px;height:6px' },
		{ edge: 'bottom', at: 'left:50%;top:100%', base: 90, size: 'width:16px;height:6px' }
	];
	const hint = $derived(
		`Drag to move, a corner to scale, the knob to turn${crop ? ', an edge to crop' : ''}. ${settings.modLabel('shift')} keeps to one axis or turns in ${ROTATE_STEP}° steps; ${settings.modLabel('alt')} ${ui.snap ? 'stops' : 'starts'} snapping.`
	);
</script>

{#if active}
	<div
		bind:this={rootEl}
		role="presentation"
		data-transform-layer
		{@attach onWindow({
			keydowncapture: (e: KeyboardEvent) => {
				if (e.key === 'Escape' && drag && !drag.committing) {
					cancel();
					e.stopPropagation();
				}
			},
			blur: cancel
		})}
		onpointermove={onMove}
		onpointerup={onUp}
		onpointercancel={cancel}
		onlostpointercapture={onLostCapture}
		style="position:absolute;left:{rect.left}%;top:{rect.top}%;width:{rect.width}%;height:{rect.height}%;pointer-events:none"
	>
		<!-- A click on a picture selects its clip and can carry straight on into a move. -->
		<div role="presentation" data-transform-hit onpointerdown={onHitDown} style="position:absolute;inset:0;pointer-events:auto"></div>
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
		{#if selected && box}
			<div
				role="presentation"
				data-transform-box
				use:tip={live ? undefined : hint}
				onpointerdown={(e) => begin(e, selected, 'move')}
				style="position:absolute;left:{pct(box.cx - box.w / 2, canvas.width)};top:{pct(box.cy - box.h / 2, canvas.height)};width:{pct(box.w, canvas.width)};height:{pct(box.h, canvas.height)};transform:rotate({box.angle}rad);box-sizing:border-box;pointer-events:auto;touch-action:none;user-select:none;cursor:{live ? 'grabbing' : 'move'};outline:1.5px solid var(--kerf-400);outline-offset:0;background:{live?.moved ? 'color-mix(in srgb,var(--kerf-400) 14%,transparent)' : 'transparent'}"
			>
				<span aria-hidden="true" style="position:absolute;left:50%;top:-22px;height:22px;border-left:1.5px solid var(--kerf-400);pointer-events:none"></span>
				<span
					role="presentation"
					data-handle="rotate"
					onpointerdown={(e) => begin(e, selected, 'rotate')}
					style="position:absolute;left:50%;top:-22px;width:11px;height:11px;transform:translate(-50%,-50%);border-radius:50%;background:var(--kerf-400);border:1.5px solid var(--surface-app);cursor:grab;touch-action:none"
				></span>
				{#each CORNERS as h (h.at)}
					<span
						role="presentation"
						data-handle="scale"
						onpointerdown={(e) => begin(e, selected, 'scale')}
						style="position:absolute;{h.at};width:10px;height:10px;transform:translate(-50%,-50%);background:var(--kerf-400);border:1.5px solid var(--surface-app);border-radius:2px;cursor:{resizeCursor(h.base, box.angle)};touch-action:none"
					></span>
				{/each}
				{#if crop}
					{#each EDGES as h (h.edge)}
						<span
							role="presentation"
							data-handle="crop-{h.edge}"
							onpointerdown={(e) => begin(e, selected, 'crop', h.edge)}
							style="position:absolute;{h.at};{h.size};transform:translate(-50%,-50%);background:var(--surface-app);border:1.5px solid var(--kerf-400);border-radius:2px;cursor:{resizeCursor(h.base, box.angle)};touch-action:none"
						></span>
					{/each}
				{/if}
			</div>
			{#if live?.moved}
				<span
					data-transform-readout
					style="position:absolute;left:{live.pointer[0] + 14}px;top:{live.pointer[1] + 14}px;padding:1px 6px;border-radius:var(--radius-sm);font-size:10px;font-weight:600;font-family:var(--font-mono);white-space:nowrap;pointer-events:none;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) 70%,transparent)"
					>{readout(live.kind, { ...live.layer.pose, ...live.patch }, live.edge)}{keyedHere ? ' · key' : ''}</span
				>
			{/if}
		{/if}
	</div>
{/if}
