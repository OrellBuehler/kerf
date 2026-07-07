<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { toast } from 'svelte-sonner';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import type { Clip, StreamKind, Track } from '$lib/types';
	import { clipDuration } from '$lib/types';

	const pxPerSec = $derived(ui.zoom);
	const duration = $derived(Math.max(editor.duration, 8));
	const contentW = $derived(Math.max(760, Math.ceil(duration * pxPerSec) + 48));
	// Tick spacing follows zoom for label density, but also steps up so the total
	// tick count stays bounded on long timelines (otherwise a 1h cut at high zoom
	// would render ~3600 spans, twice — ruler labels + grid lines).
	const TICK_CAP = 240;
	const tickStep = $derived.by(() => {
		const base = pxPerSec >= 60 ? 2 : pxPerSec >= 28 ? 5 : 10;
		const ladder = [2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600];
		for (const step of ladder) {
			if (step >= base && duration / step <= TICK_CAP) return step;
		}
		return ladder[ladder.length - 1];
	});
	const ticks = $derived(Array.from({ length: Math.floor(duration / tickStep) + 1 }, (_, i) => i * tickStep));
	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));

	function fmt(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
	}

	function trackHeight(t: Track): string {
		return t.kind === 'video' ? 'var(--track-h-video)' : 'var(--track-h-audio)';
	}

	// ---- analysis overlays mapped from source-time to timeline-time ----------

	/** Timeline-seconds position of a source-time point inside a clip, honoring
	 * the clip's speed (and reverse direction). */
	function srcToTimeline(c: Clip, src: number): number {
		const sp = c.speed ?? 1;
		const mag = Math.max(Math.abs(sp), 0.01);
		const off = sp < 0 ? c.source_out - src : src - c.source_in;
		return c.timeline_start + off / mag;
	}

	const sceneXs = $derived.by(() => {
		const xs: number[] = [];
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'video') continue;
			for (const c of t.clips) {
				const an = editor.analysisFor(c.asset_id);
				if (!an) continue;
				for (const sc of an.scene_changes) {
					if (sc > c.source_in && sc < c.source_out) {
						xs.push(srcToTimeline(c, sc) * pxPerSec);
					}
				}
			}
		}
		return xs;
	});

	// ---- beat grid (tempo analysis → ruler ticks + snap targets) --------------

	/** Ignore tempo estimates below this confidence. */
	const BEAT_MIN_CONF = 0.25;
	/** Hide (and stop snapping to) the grid when beats land closer than this, px. */
	const BEAT_MIN_PX = 4;

	const beatTimes = $derived.by(() => {
		const ts: number[] = [];
		let period = Infinity; // shortest on-timeline beat interval, seconds
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'audio') continue;
			for (const c of t.clips) {
				const tempo = editor.analysisFor(c.asset_id)?.tempo;
				if (!tempo || tempo.confidence < BEAT_MIN_CONF || tempo.bpm <= 0) continue;
				const mag = Math.max(Math.abs(c.speed ?? 1), 0.01);
				period = Math.min(period, 60 / tempo.bpm / mag);
				for (const b of tempo.beats) {
					if (b >= c.source_in && b <= c.source_out) ts.push(srcToTimeline(c, b));
				}
			}
		}
		if (ts.length === 0 || period * pxPerSec < BEAT_MIN_PX) return [];
		ts.sort((a, b) => a - b);
		// Overlapping clips of one asset repeat the same beats; drop the copies.
		return ts.filter((b, i) => i === 0 || b - ts[i - 1] > 0.005);
	});

	function silenceRegions(c: Clip): { left: number; width: number }[] {
		const an = editor.analysisFor(c.asset_id);
		if (!an) return [];
		return an.silence_segments
			.filter((s) => s.end > c.source_in && s.start < c.source_out)
			.map((s) => {
				const a = Math.max(s.start, c.source_in);
				const b = Math.min(s.end, c.source_out);
				const x1 = srcToTimeline(c, a) * pxPerSec;
				const x2 = srcToTimeline(c, b) * pxPerSec;
				return { left: Math.min(x1, x2), width: Math.abs(x2 - x1) };
			});
	}

	// ---- waveforms -----------------------------------------------------------

	const WAVE_BUCKETS = 240;
	// Resolved peaks per asset (reactive). In-flight requests are tracked in a
	// plain Set, not written into `waveforms`, so the fetch effect doesn't
	// re-trigger itself by mutating the very state it reads.
	let waveforms = $state<Record<string, number[]>>({});
	const inflight = new Set<string>();

	$effect(() => {
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'audio') continue;
			for (const c of t.clips) {
				const id = c.asset_id;
				if (waveforms[id] || inflight.has(id)) continue;
				inflight.add(id);
				editor.waveform(id, WAVE_BUCKETS).then((peaks) => {
					inflight.delete(id);
					waveforms = { ...waveforms, [id]: peaks };
				});
			}
		}
	});

	const assetDuration = $derived.by(() => {
		const m = new Map<string, number>();
		for (const a of editor.assets) m.set(a.id, a.duration);
		return m;
	});

	/** Peaks for a clip's source range, down-sampled to about one bar per rendered
	 *  pixel (max-aggregated so loud transients survive at low zoom). Without this
	 *  a clip emits up to WAVE_BUCKETS spans regardless of how wide it actually is. */
	function clipPeaks(c: Clip, width: number): number[] {
		const all = waveforms[c.asset_id];
		const dur = assetDuration.get(c.asset_id) ?? 0;
		if (!all || all.length === 0 || dur <= 0) return [];
		const lo = Math.floor((c.source_in / dur) * all.length);
		const hi = Math.max(lo + 1, Math.ceil((c.source_out / dur) * all.length));
		const slice = all.slice(lo, Math.min(hi, all.length));
		const target = Math.max(1, Math.min(Math.ceil(width), slice.length));
		if (target >= slice.length) return slice;
		const out = new Array<number>(target);
		for (let i = 0; i < target; i++) {
			const start = Math.floor((i / target) * slice.length);
			const end = Math.max(start + 1, Math.floor(((i + 1) / target) * slice.length));
			let peak = 0;
			for (let j = start; j < end && j < slice.length; j++) peak = Math.max(peak, slice[j]);
			out[i] = peak;
		}
		return out;
	}

	// ---- interaction: select / razor split / drag-to-move --------------------

	type Drag = {
		clipId: string;
		kind: StreamKind;
		origTrackId: string;
		origStart: number;
		grabSec: number; // pointer offset within the clip (seconds)
		dur: number;
		start: number; // current ghost start (seconds)
		trackId: string; // current ghost destination track
		moved: boolean;
	};
	let drag = $state<Drag | null>(null);

	// ---- edge-dragging trim ---------------------------------------------------

	type TrimDrag = {
		clipId: string;
		trackId: string;
		edge: 'l' | 'r';
		min: number; // dragged-edge bounds, timeline seconds
		max: number;
		origStart: number;
		origEnd: number;
		pos: number; // current ghost position of the dragged edge
		moved: boolean;
	};
	let trimDrag = $state<TrimDrag | null>(null);

	/** Shortest a clip may get when edge-trimming, seconds. */
	const MIN_CLIP = 0.05;

	function onEdgePointerDown(e: PointerEvent, c: Clip, t: Track, edge: 'l' | 'r') {
		if (e.button !== 0 || ui.tool === 'razor') return; // razor falls through to split
		e.stopPropagation();
		editor.selectedClipId = c.id;
		void editor.select(c.asset_id);
		const asset = editor.assets.find((a) => a.id === c.asset_id);
		// A still image loops, so its source window can grow without limit.
		const still = asset?.streams.some((s) => s.image) ?? false;
		const sp = c.speed ?? 1;
		const mag = Math.max(Math.abs(sp), 0.01);
		const start = c.timeline_start;
		const end = start + clipDuration(c);
		const clips = [...(editor.timeline.tracks.find((tr) => tr.id === t.id)?.clips ?? [])].sort(
			(a, b) => a.timeline_start - b.timeline_start
		);
		const i = clips.findIndex((x) => x.id === c.id);
		// Unused source on the side being extended: a forward clip's left edge
		// draws on the handle below source_in, its right edge on the handle past
		// source_out; a reversed clip plays backwards, so the sides swap.
		const headHandle = sp < 0 ? Math.max(0, (asset?.duration ?? c.source_out) - c.source_out) : c.source_in;
		const tailHandle = sp < 0 ? c.source_in : Math.max(0, (asset?.duration ?? c.source_out) - c.source_out);
		let min: number;
		let max: number;
		if (edge === 'l') {
			const prev = i > 0 ? clips[i - 1] : null;
			const prevEnd = prev ? prev.timeline_start + clipDuration(prev) : 0;
			min = Math.max(0, prevEnd, still ? 0 : start - headHandle / mag);
			max = end - MIN_CLIP;
		} else {
			const nextStart = i >= 0 && i < clips.length - 1 ? clips[i + 1].timeline_start : Infinity;
			min = start + MIN_CLIP;
			max = Math.min(nextStart, still ? Infinity : end + tailHandle / mag);
		}
		trimDrag = {
			clipId: c.id,
			trackId: t.id,
			edge,
			min,
			max,
			origStart: start,
			origEnd: end,
			pos: edge === 'l' ? start : end,
			moved: false
		};
	}

	function onTrimMove(e: PointerEvent) {
		if (!trimDrag) return;
		const lane = document.querySelector(`[data-lane][data-track-id="${trimDrag.trackId}"]`) as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		const pos = Math.min(trimDrag.max, Math.max(trimDrag.min, snapPoint(laneTime(e.clientX, laneLeft), trimDrag.trackId, trimDrag.clipId)));
		const orig = trimDrag.edge === 'l' ? trimDrag.origStart : trimDrag.origEnd;
		const moved = trimDrag.moved || Math.abs(pos - orig) >= 2 / pxPerSec;
		trimDrag = { ...trimDrag, pos, moved };
	}

	function onTrimUp() {
		if (!trimDrag) return;
		const d = trimDrag;
		trimDrag = null;
		if (!d.moved) return;
		const clip = editor.timeline.tracks.find((t) => t.id === d.trackId)?.clips.find((c) => c.id === d.clipId);
		if (!clip) return;
		const sp = clip.speed ?? 1;
		const mag = Math.max(Math.abs(sp), 0.01);
		if (d.edge === 'r') {
			const newDur = d.pos - clip.timeline_start;
			// The end of playback maps to source_out forward, source_in reversed.
			if (sp < 0) void editor.trim(d.clipId, clip.source_out - newDur * mag, undefined).catch(err);
			else void editor.trim(d.clipId, undefined, clip.source_in + newDur * mag).catch(err);
		} else {
			const delta = (d.pos - d.origStart) * mag; // > 0 shortens from the left
			if (sp < 0) void editor.trim(d.clipId, undefined, clip.source_out - delta, d.pos).catch(err);
			else void editor.trim(d.clipId, clip.source_in + delta, undefined, d.pos).catch(err);
		}
	}

	/** Snap a time point to 0, the playhead, a beat, or another clip's edges. */
	function snapPoint(time: number, trackId: string, clipId: string): number {
		if (!ui.snap) return time;
		const threshold = 8 / pxPerSec;
		const cands: number[] = [0, ui.time, ...beatTimes];
		const track = editor.timeline.tracks.find((t) => t.id === trackId);
		if (track)
			for (const c of track.clips) {
				if (c.id === clipId) continue;
				cands.push(c.timeline_start, c.timeline_start + clipDuration(c));
			}
		let best = time;
		let bestD = threshold;
		for (const cand of cands) {
			const d = Math.abs(cand - time);
			if (d < bestD) {
				bestD = d;
				best = cand;
			}
		}
		return best;
	}

	const laneTime = (clientX: number, laneLeft: number) => (clientX - laneLeft) / pxPerSec;

	function err(e: unknown) {
		toast.error(e instanceof Error ? e.message : String(e));
	}

	function onClipPointerDown(e: PointerEvent, c: Clip, t: Track) {
		if (e.button !== 0) return;
		const lane = (e.currentTarget as HTMLElement).closest('[data-lane]') as HTMLElement | null;
		const laneLeft = lane?.getBoundingClientRect().left ?? 0;
		if (ui.tool === 'razor') {
			e.stopPropagation();
			const at = Math.max(c.timeline_start + 0.05, laneTime(e.clientX, laneLeft));
			void editor.split(c.id, at).catch(err);
			return;
		}
		editor.selectedClipId = c.id;
		void editor.select(c.asset_id);
		drag = {
			clipId: c.id,
			kind: t.kind,
			origTrackId: t.id,
			origStart: c.timeline_start,
			grabSec: laneTime(e.clientX, laneLeft) - c.timeline_start,
			dur: clipDuration(c),
			start: c.timeline_start,
			trackId: t.id,
			moved: false
		};
	}

	function laneUnder(clientX: number, clientY: number): HTMLElement | null {
		for (const el of document.elementsFromPoint(clientX, clientY)) {
			if (el instanceof HTMLElement && el.dataset.lane !== undefined) return el;
		}
		return null;
	}

	/** Snap a candidate start to 0, the playhead, a beat, or another clip's edges. */
	function snapStart(start: number, trackId: string, clipId: string, dur: number): number {
		const clamped = Math.max(0, start);
		if (!ui.snap) return clamped;
		const threshold = 8 / pxPerSec;
		const cands: number[] = [0, ui.time];
		for (const b of beatTimes) cands.push(b, b - dur); // land either edge on a beat
		const track = editor.timeline.tracks.find((t) => t.id === trackId);
		if (track)
			for (const c of track.clips) {
				if (c.id === clipId) continue;
				const cs = c.timeline_start;
				const ce = c.timeline_start + clipDuration(c);
				cands.push(cs, ce, cs - dur); // align heads, butt after, butt before
			}
		let best = clamped;
		let bestD = threshold;
		for (const cand of cands) {
			const d = Math.abs(cand - start);
			if (d < bestD) {
				bestD = d;
				best = Math.max(0, cand);
			}
		}
		return best;
	}

	function onPointerMove(e: PointerEvent) {
		if (trimDrag) {
			onTrimMove(e);
			return;
		}
		if (!drag) return;
		const lane = laneUnder(e.clientX, e.clientY);
		let trackId = drag.trackId;
		let laneLeft: number;
		if (lane && lane.dataset.kind === drag.kind) {
			trackId = lane.dataset.trackId!;
			laneLeft = lane.getBoundingClientRect().left;
		} else {
			const cur = document.querySelector(`[data-lane][data-track-id="${trackId}"]`) as HTMLElement | null;
			laneLeft = cur?.getBoundingClientRect().left ?? 0;
		}
		const start = snapStart(laneTime(e.clientX, laneLeft) - drag.grabSec, trackId, drag.clipId, drag.dur);
		const movedEnough =
			drag.moved || trackId !== drag.origTrackId || Math.abs(start - drag.origStart) >= 3 / pxPerSec;
		drag = { ...drag, start: movedEnough ? start : drag.start, trackId, moved: movedEnough };
	}

	function onPointerUp() {
		if (trimDrag) {
			onTrimUp();
			return;
		}
		if (!drag) return;
		const d = drag;
		drag = null;
		if (!d.moved) {
			ui.seek(d.origStart + d.grabSec); // a plain click on the clip seeks there
			return;
		}
		const trackArg = d.trackId !== d.origTrackId ? d.trackId : undefined;
		if (trackArg === undefined && Math.abs(d.start - d.origStart) < 1e-6) return; // no-op
		void editor.move(d.clipId, d.start, trackArg).catch(err);
	}

	function onLaneSeek(e: MouseEvent) {
		const x = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		ui.seek(x / pxPerSec);
	}

	// ---- drop a bin asset onto a track (HTML5 drag-and-drop) ------------------

	let dropGhost = $state<{ trackId: string; start: number; dur: number; ok: boolean } | null>(null);

	// The bin clears `ui.dndAsset` on dragend; mirror that to drop the ghost.
	$effect(() => {
		if (!ui.dndAsset) dropGhost = null;
	});

	function dropStart(e: DragEvent, t: Track, dur: number): number {
		const laneLeft = (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		return snapStart(laneTime(e.clientX, laneLeft), t.id, '', dur);
	}

	/** Whether [start, start+dur) would overlap an existing clip on the track —
	 *  the same invariant `move_clip` enforces, so adds stay consistent. */
	function wouldOverlap(trackId: string, start: number, dur: number): boolean {
		const track = editor.timeline.tracks.find((t) => t.id === trackId);
		if (!track) return false;
		const end = start + dur;
		return track.clips.some((c) => start < c.timeline_start + clipDuration(c) && c.timeline_start < end);
	}

	function onLaneDragOver(e: DragEvent, t: Track) {
		const a = ui.dndAsset;
		if (!a || a.kind !== t.kind) {
			dropGhost = null; // wrong-kind track: not a drop target
			return;
		}
		// Always allow the drop so the drop event fires reliably across webview
		// engines; onLaneDrop rejects overlaps. The ghost turns red to warn.
		e.preventDefault();
		if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
		const start = dropStart(e, t, a.duration);
		dropGhost = { trackId: t.id, start, dur: a.duration, ok: !wouldOverlap(t.id, start, a.duration) };
	}

	function onLaneDragLeave(e: DragEvent, t: Track) {
		// Only clear when truly leaving the lane (not entering one of its clips).
		const to = e.relatedTarget as Node | null;
		if (!to || !(e.currentTarget as HTMLElement).contains(to)) {
			if (dropGhost?.trackId === t.id) dropGhost = null;
		}
	}

	function onLaneDrop(e: DragEvent, t: Track) {
		const a = ui.dndAsset;
		dropGhost = null;
		ui.dndAsset = null;
		if (!a || a.kind !== t.kind) return;
		e.preventDefault();
		const start = dropStart(e, t, a.duration);
		if (wouldOverlap(t.id, start, a.duration)) {
			toast.error('Drop into free space — a clip would overlap another here');
			return;
		}
		void editor.add(a.id, 0, a.duration, t.id, start).catch(err);
	}

	// ---- tracks (add / remove) -----------------------------------------------

	const onAddTrack = (kind: StreamKind) => void editor.addTrack(kind).catch(err);
	const onRemoveTrack = (t: Track) =>
		void editor
			.removeTrack(t.id)
			.then(() =>
				toast(`Removed track ${t.name}`, {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				})
			)
			.catch(err);

	// ---- fades (toggle a default 0.5s fade on the selected clip) --------------

	const FADE_DEFAULT = 0.5;
	function toggleFadeIn() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, c.fade_in > 0 ? 0 : FADE_DEFAULT);
	}
	function toggleFadeOut() {
		const c = editor.selectedClip;
		if (c) void editor.setFade(c.id, undefined, c.fade_out > 0 ? 0 : FADE_DEFAULT);
	}

	// ---- context menus -------------------------------------------------------

	function removeClip(id: string, ripple: boolean) {
		void (ripple ? editor.rippleDelete(id) : editor.remove(id))
			.then(() => toast('Clip removed', { action: { label: 'Undo', onClick: () => void editor.undo() } }))
			.catch(err);
	}

	function trackItems(t: Track): MenuItem[] {
		return [
			{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
			{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
			{ type: 'separator' },
			{ label: `Remove track ${t.name}`, icon: 'trash', danger: true, action: () => onRemoveTrack(t) }
		];
	}

	function onClipContextMenu(e: MouseEvent, c: Clip, t: Track) {
		editor.selectedClipId = c.id;
		void editor.select(c.asset_id);
		const within = ui.time > c.timeline_start && ui.time < c.timeline_start + clipDuration(c);
		contextMenu.show(e, [
			{
				label: 'Split at playhead',
				icon: 'Scissors',
				shortcut: 'C',
				disabled: !within,
				action: () => void editor.split(c.id, ui.time).catch(err)
			},
			{ type: 'separator' },
			{
				label: c.fade_in > 0 ? 'Remove fade-in' : 'Add fade-in',
				action: () => void editor.setFade(c.id, c.fade_in > 0 ? 0 : FADE_DEFAULT).catch(err)
			},
			{
				label: c.fade_out > 0 ? 'Remove fade-out' : 'Add fade-out',
				action: () => void editor.setFade(c.id, undefined, c.fade_out > 0 ? 0 : FADE_DEFAULT).catch(err)
			},
			{ type: 'separator' },
			{ label: 'Remove', icon: 'trash', shortcut: 'Del', danger: true, action: () => removeClip(c.id, false) },
			{
				label: 'Ripple delete',
				icon: 'trash',
				shortcut: '⇧Del',
				danger: true,
				action: () => removeClip(c.id, true)
			}
		]);
	}

	const onLaneContextMenu = (e: MouseEvent, t: Track) => contextMenu.show(e, trackItems(t));
	const onTrackHeaderContextMenu = (e: MouseEvent, t: Track) => contextMenu.show(e, trackItems(t));

	// Right-click on empty timeline canvas (ruler / grid / below the tracks). Clip
	// and lane menus stopPropagation, so only the bare background reaches this.
	function onTimelineContextMenu(e: MouseEvent) {
		contextMenu.show(e, [
			{ label: 'Add video track', icon: 'video', action: () => onAddTrack('video') },
			{ label: 'Add audio track', icon: 'audio-waveform', action: () => onAddTrack('audio') },
			{ type: 'separator' },
			{
				label: ui.snap ? 'Disable snapping' : 'Enable snapping',
				icon: 'magnet',
				action: () => (ui.snap = !ui.snap)
			}
		]);
	}
</script>

<svelte:window onpointermove={onPointerMove} onpointerup={onPointerUp} />

<div
	style="height:296px;flex:none;border-top:1px solid var(--border-default);background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>
	<!-- timeline toolbar -->
	<div
		style="height:34px;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-subtle);flex:none"
	>
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>Timeline</span
		>
		{#if editor.busy}<Badge tone="agent" dot>working…</Badge>{/if}
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{fmt(duration)}</span>
		{#if editor.selectedClip}
			{@const sc = editor.selectedClip}
			<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
			<span
				style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
				>Fade</span
			>
			<button
				title="Toggle a {FADE_DEFAULT}s fade-in on the selected clip"
				onclick={toggleFadeIn}
				style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:1px solid var(--border-strong);background:{sc.fade_in >
				0
					? 'var(--surface-hover)'
					: 'transparent'};color:{sc.fade_in > 0 ? 'var(--kerf-300)' : 'var(--text-muted)'}">in</button
			>
			<button
				title="Toggle a {FADE_DEFAULT}s fade-out on the selected clip"
				onclick={toggleFadeOut}
				style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:1px solid var(--border-strong);background:{sc.fade_out >
				0
					? 'var(--surface-hover)'
					: 'transparent'};color:{sc.fade_out > 0 ? 'var(--kerf-300)' : 'var(--text-muted)'}">out</button
			>
		{/if}
		<div style="flex:1"></div>
		<button
			title="Zoom out"
			aria-label="Zoom out"
			onclick={() => (ui.zoom = Math.max(8, ui.zoom - 8))}
			style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center"
			><Icon n="zoom-out" s={14} /></button
		>
		<div style="width:90px;height:4px;border-radius:999px;background:var(--surface-inset);position:relative">
			<div
				style="position:absolute;inset:0 auto 0 0;width:{Math.round((ui.zoom / 96) * 100)}%;background:var(--neutral-600);border-radius:999px"
			></div>
		</div>
		<button
			title="Zoom in"
			aria-label="Zoom in"
			onclick={() => (ui.zoom = Math.min(96, ui.zoom + 8))}
			style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center"
			><Icon n="zoom-in" s={14} /></button
		>
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)"
			>{ui.snap ? 'snap on' : 'snap off'}</span
		>
		<span style="width:1px;height:16px;background:var(--border-strong);margin:0 4px"></span>
		<button
			title="Add a video track"
			onclick={() => onAddTrack('video')}
			style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:1px solid var(--border-strong);background:transparent;color:var(--text-muted)"
			>+ V</button
		>
		<button
			title="Add an audio track"
			onclick={() => onAddTrack('audio')}
			style="font-size:10px;padding:2px 7px;border-radius:4px;cursor:pointer;border:1px solid var(--border-strong);background:transparent;color:var(--text-muted)"
			>+ A</button
		>
	</div>

	<div style="flex:1;display:flex;min-height:0">
		<!-- track headers -->
		<div
			style="width:var(--track-header-w);flex:none;border-right:1px solid var(--border-default);background:var(--surface-app)"
		>
			<div style="height:var(--ruler-h);border-bottom:1px solid var(--border-subtle)"></div>
			{#each editor.timeline.tracks as t (t.id)}
				<div
					role="presentation"
					oncontextmenu={(e) => onTrackHeaderContextMenu(e, t)}
					style="height:{trackHeight(t)};border-bottom:1px solid var(--border-subtle);display:flex;align-items:center;gap:8px;padding:0 10px"
				>
					<span
						style="font-family:var(--font-mono);font-size:11px;font-weight:600;color:var(--text-secondary);width:20px"
						>{t.name}</span
					>
					<span style="font-size:11px;color:var(--text-muted);flex:1">{t.kind === 'video' ? 'Video' : 'Audio'}</span>
					{#if t.kind === 'audio'}
						<button
							title={t.duck
								? 'Ducking on — this track dips under the rest of the mix on export'
								: 'Duck this track under the rest of the mix on export'}
							aria-label="Toggle ducking"
							onclick={() => void editor.setTrackDuck(t.id, !t.duck).catch(err)}
							style="background:{t.duck ? 'var(--kerf-500)' : 'none'};border:1px solid {t.duck
								? 'var(--kerf-500)'
								: 'var(--border-strong)'};border-radius:3px;cursor:pointer;color:{t.duck
								? '#fff'
								: 'var(--text-disabled)'};font-size:8px;font-weight:700;letter-spacing:.5px;padding:1px 4px"
							>DUCK</button
						>
					{/if}
					<Icon n={t.kind === 'video' ? 'eye' : 'volume-2'} s={12} color="var(--text-disabled)" />
					<button
						title="Remove track"
						aria-label="Remove track"
						onclick={() => onRemoveTrack(t)}
						style="background:none;border:none;cursor:pointer;color:var(--text-disabled);display:grid;place-items:center;padding:0"
						><Icon n="x" s={12} /></button
					>
				</div>
			{/each}
		</div>

		<!-- scrollable track area -->
		<div style="flex:1;overflow-x:auto;overflow-y:hidden;position:relative">
			<div
				role="presentation"
				oncontextmenu={onTimelineContextMenu}
				style="width:{contentW}px;position:relative"
			>
				<!-- ruler -->
				<div
					role="presentation"
					onclick={onLaneSeek}
					style="height:var(--ruler-h);border-bottom:1px solid var(--border-subtle);position:relative;background:var(--surface-app);cursor:pointer"
				>
					{#each ticks as t (t)}
						<span
							style="position:absolute;left:{t * pxPerSec + 4}px;top:7px;font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)"
							>{fmt(t)}</span
						>
					{/each}
					{#each sceneXs as x (x)}
						<span
							title="Detected scene cut"
							style="position:absolute;left:{x}px;bottom:0;width:0;height:0;border-left:4px solid transparent;border-right:4px solid transparent;border-top:5px solid var(--scene-marker);transform:translateX(-50%)"
						></span>
					{/each}
					{#each beatTimes as b (b)}
						<span
							title="Beat"
							style="position:absolute;left:{b * pxPerSec}px;bottom:0;width:1px;height:5px;background:var(--beat-marker);opacity:.75;pointer-events:none"
						></span>
					{/each}
					{#if ui.markIn !== null && ui.markOut !== null && ui.markOut > ui.markIn}
						<div
							style="position:absolute;left:{ui.markIn * pxPerSec}px;width:{(ui.markOut - ui.markIn) *
								pxPerSec}px;top:0;bottom:0;background:var(--selection-fill);pointer-events:none"
						></div>
					{/if}
					{#if ui.markIn !== null}
						<span
							title="Mark in {fmt(ui.markIn)} — I sets, ⇧I clears"
							style="position:absolute;left:{ui.markIn * pxPerSec}px;top:0;bottom:0;width:2px;background:var(--kerf-400);pointer-events:none"
						>
							<span
								style="position:absolute;top:0;left:2px;width:7px;height:7px;background:var(--kerf-400);clip-path:polygon(0 0,100% 0,0 100%)"
							></span>
						</span>
					{/if}
					{#if ui.markOut !== null}
						<span
							title="Mark out {fmt(ui.markOut)} — O sets, ⇧O clears"
							style="position:absolute;left:{ui.markOut * pxPerSec - 2}px;top:0;bottom:0;width:2px;background:var(--kerf-400);pointer-events:none"
						>
							<span
								style="position:absolute;top:0;right:2px;width:7px;height:7px;background:var(--kerf-400);clip-path:polygon(0 0,100% 0,100% 100%)"
							></span>
						</span>
					{/if}
				</div>

				<!-- grid lines -->
				{#if hasClips}
					{#each ticks as t, i (t)}
						<span
							style="position:absolute;left:{t * pxPerSec}px;top:var(--ruler-h);bottom:0;width:1px;background:{i % 2 ? 'var(--timeline-grid)' : 'var(--timeline-grid-major)'}"
						></span>
					{/each}
				{/if}

				{#if !hasClips}
					<div
						style="position:absolute;left:0;right:0;top:var(--ruler-h);bottom:0;display:grid;place-items:center;color:var(--text-disabled);font-size:12px"
					>
						Timeline empty — import media and queue a cut
					</div>
				{/if}

				<!-- tracks -->
				{#each editor.timeline.tracks as t (t.id)}
					<div
						role="presentation"
						data-lane
						data-track-id={t.id}
						data-kind={t.kind}
						onclick={onLaneSeek}
						oncontextmenu={(e) => onLaneContextMenu(e, t)}
						ondragover={(e) => onLaneDragOver(e, t)}
						ondragleave={(e) => onLaneDragLeave(e, t)}
						ondrop={(e) => onLaneDrop(e, t)}
						style="height:{trackHeight(t)};border-bottom:1px solid var(--border-subtle);position:relative"
					>
						{#each t.clips as c (c.id)}
							{@const left = c.timeline_start * pxPerSec}
							{@const width = Math.max(6, clipDuration(c) * pxPerSec)}
							{@const selected = editor.selectedClipId === c.id}
							{@const dragging = drag?.moved && drag.clipId === c.id}
							<button
								onpointerdown={(e) => onClipPointerDown(e, c, t)}
								oncontextmenu={(e) => onClipContextMenu(e, c, t)}
								onclick={(e) => e.stopPropagation()}
								style="position:absolute;left:{left}px;top:5px;height:calc(100% - 10px);width:{width}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;padding:0 7px;touch-action:none;opacity:{dragging
									? 0.4
									: 1};cursor:{ui.tool === 'razor' ? 'crosshair' : drag ? 'grabbing' : 'grab'};text-align:left;background:{t.kind === 'audio' ? 'var(--track-audio)' : 'var(--track-video)'};border:{selected ? '1.5px solid var(--kerf-400)' : `1px solid ${t.kind === 'audio' ? 'var(--track-audio-edge)' : 'var(--track-video-edge)'}`};box-shadow:{selected ? '0 0 0 1px var(--kerf-500)' : 'none'}"
							>
								{#if t.kind === 'audio'}
									{@const peaks = clipPeaks(c, width)}
									{#if peaks.length}
										<div style="position:absolute;inset:0;display:flex;align-items:center;gap:1px;padding:0 2px;opacity:.5">
											{#each peaks as p, i (i)}
												<span style="flex:1;height:{Math.max(6, p * 100)}%;background:var(--waveform);border-radius:1px"></span>
											{/each}
										</div>
									{:else}
										<div
											style="position:absolute;inset:0;background:repeating-linear-gradient(90deg, var(--waveform) 0 1px, transparent 1px 3px);opacity:.35;mask-image:linear-gradient(transparent 28%, #000 28%, #000 72%, transparent 72%)"
										></div>
									{/if}
									{#each silenceRegions(c) as r (r.left)}
										<span
											title="Detected silence"
											style="position:absolute;left:{r.left - left}px;top:3px;bottom:3px;width:{Math.max(2, r.width)}px;background:var(--silence-region);border:1px solid rgba(229,84,75,.3);border-radius:2px"
										></span>
									{/each}
								{/if}
								{#if c.fade_in > 0}
									<span
										title="Fade in {c.fade_in.toFixed(2)}s"
										style="position:absolute;left:0;top:0;bottom:0;width:{Math.min(c.fade_in * pxPerSec, width)}px;background:linear-gradient(to right, rgba(0,0,0,.7), transparent);pointer-events:none"
									></span>
								{/if}
								{#if c.fade_out > 0}
									<span
										title="Fade out {c.fade_out.toFixed(2)}s"
										style="position:absolute;right:0;top:0;bottom:0;width:{Math.min(c.fade_out * pxPerSec, width)}px;background:linear-gradient(to left, rgba(0,0,0,.7), transparent);pointer-events:none"
									></span>
								{/if}
								{#if c.transition_in}
									<span
										title="{c.transition_in.kind === 'crossfade' ? 'Crossfade' : 'Dip to black'} {c.transition_in.duration.toFixed(2)}s"
										style="position:absolute;left:0;top:0;bottom:0;width:{Math.min(
											c.transition_in.duration * pxPerSec,
											width
										)}px;background:linear-gradient(to right, rgba(120,140,255,.55), transparent);border-left:2px solid var(--kerf-400);pointer-events:none"
									></span>
								{/if}
								<span
									style="position:relative;font-size:10px;font-weight:600;color:rgba(255,255,255,.92);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
									>{editor.assetName(c.asset_id)}</span
								>
								{#if (c.speed ?? 1) !== 1}
									{@const sp = c.speed ?? 1}
									<span
										title="Speed {sp}×"
										style="position:absolute;right:3px;top:3px;font-size:9px;font-weight:700;color:#fff;background:rgba(0,0,0,.55);border-radius:3px;padding:1px 4px;pointer-events:none"
										>{sp < 0 ? `${Math.abs(sp)}× ⟲` : `${sp}×`}</span
									>
								{/if}
								{#if ui.tool === 'pointer' && width > 24}
									<span
										role="presentation"
										onpointerdown={(e) => onEdgePointerDown(e, c, t, 'l')}
										style="position:absolute;left:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
									></span>
									<span
										role="presentation"
										onpointerdown={(e) => onEdgePointerDown(e, c, t, 'r')}
										style="position:absolute;right:0;top:0;bottom:0;width:6px;cursor:ew-resize;z-index:3;touch-action:none"
									></span>
								{/if}
							</button>
						{/each}
						{#if drag?.moved && drag.trackId === t.id}
							<div
								style="position:absolute;left:{drag.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									6,
									drag.dur * pxPerSec
								)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:rgba(120,140,255,.16);pointer-events:none;z-index:25"
							></div>
						{/if}
						{#if trimDrag?.moved && trimDrag.trackId === t.id}
							{@const gl = trimDrag.edge === 'l' ? trimDrag.pos : trimDrag.origStart}
							{@const gr = trimDrag.edge === 'l' ? trimDrag.origEnd : trimDrag.pos}
							<div
								style="position:absolute;left:{gl * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									2,
									(gr - gl) * pxPerSec
								)}px;border:1.5px dashed var(--kerf-400);border-radius:2px;background:rgba(120,140,255,.16);pointer-events:none;z-index:25"
							></div>
						{/if}
						{#if dropGhost && dropGhost.trackId === t.id}
							<div
								style="position:absolute;left:{dropGhost.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(
									6,
									dropGhost.dur * pxPerSec
								)}px;border:1.5px dashed {dropGhost.ok
									? 'var(--kerf-400)'
									: 'var(--red-500)'};border-radius:2px;background:{dropGhost.ok
									? 'var(--selection-fill)'
									: 'var(--danger-surface)'};pointer-events:none;z-index:25"
							></div>
						{/if}
					</div>
				{/each}

				<!-- playhead -->
				<div
					style="position:absolute;left:{ui.time * pxPerSec}px;top:0;bottom:0;width:2px;background:var(--playhead);box-shadow:0 0 10px 1px var(--playhead-glow);z-index:30;pointer-events:none"
				>
					<span
						style="position:absolute;top:-1px;left:-5px;width:12px;height:9px;background:var(--playhead);clip-path:polygon(0 0,100% 0,50% 100%)"
					></span>
				</div>
			</div>
		</div>
	</div>
</div>
