<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import { formatTimecode } from '$lib/timecode';
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import { exportCover, getTimelineFrame, inTauri, pickCoverPath, revealPath, startPlayback } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { createFrameGate, PLAYBACK_FPS } from '$lib/playback-sync';
	import { clipDuration } from '$lib/types';
	import type { TextOverlay } from '$lib/types';
	import { LINE_HEIGHT, boxPadding, containRect, dragPosition, isVisibleAt, sampleOverlay, scaledSize } from '$lib/titles';

	const duration = $derived(Math.max(editor.duration, 0.001));
	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));
	const empty = $derived(!hasClips);

	/** The video clip under the playhead, and the matching source time. */
	const atPlayhead = $derived.by(() => {
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'video') continue;
			for (const c of t.clips) {
				const end = c.timeline_start + clipDuration(c);
				if (ui.time >= c.timeline_start && ui.time < end) {
					// Source advances by the speed magnitude per timeline second (and
					// backwards for a reversed clip).
					const sp = c.speed ?? 1;
					const mag = Math.max(Math.abs(sp), 0.01);
					const srcOffset = (ui.time - c.timeline_start) * mag;
					const srcTime = sp < 0 ? c.source_out - srcOffset : c.source_in + srcOffset;
					return { assetId: c.asset_id, srcTime };
				}
			}
		}
		return null;
	});

	// The asset actually shown in the preview is the clip's source under the
	// playhead — not the media-bin selection, which may be a different asset.
	const previewAsset = $derived(
		atPlayhead ? editor.assets.find((a) => a.id === atPlayhead.assetId) : undefined
	);

	// Once the project has a delivery frame, that is the shape on screen — showing
	// the source's dimensions here would label the picture with a size it isn't.
	const resolution = $derived.by(() => {
		const d = editor.timeline.format;
		if (d) return `${d.width}×${d.height}`;
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.width && v?.height ? `${v.width}×${v.height}` : '—';
	});
	const fpsLabel = $derived.by(() => {
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.fps ? v.fps.toFixed(3) : '';
	});

	function tc(s: number): string {
		return formatTimecode(s, editor.fps);
	}

	let frameUrl = $state<string | null>(null);
	let inFlight = false;
	let queued: number | null = null; // latest wanted timeline time, or null to clear

	// Single-flight decode: only ever one composite in flight, and `queued` always
	// holds the *latest* wanted timeline time. Scrubbing collapses to one render +
	// one pending target instead of a backlog of stale frames that must all drain
	// before the frame under the cursor appears (the cause of lag).
	async function pump() {
		if (inFlight || queued === null) return;
		const t = queued;
		queued = null;
		inFlight = true;
		try {
			// The *composited* timeline still — every visible clip with its color,
			// effects, transform and overlays applied, so Inspector edits show up
			// live. (Desktop only — null in the browser.)
			const url = await getTimelineFrame(t, 960);
			// Playback may have taken over while this was decoding; a still landing
			// on top of live frames would show as a stutter.
			if (url && !streaming) frameUrl = url;
		} catch {
			/* ignore decode errors — keep the last good frame */
		}
		inFlight = false;
		if (queued !== null) pump(); // a newer target arrived mid-decode — go to it
	}

	// ---- playback: a streamed frame source instead of per-frame decodes -------
	//
	// While playing forward at 1×, one long-lived ffmpeg composites the timeline
	// from the playhead and pushes frames up as they render. Scrubbing, shuttle
	// and the paused frame keep the per-frame path below: that one is right when
	// you want *one* frame, and streaming is right when you want all of them.
	let streaming = $state(false);
	let stopStream: (() => void) | null = null;
	/** Bumped when the stream has fallen so far behind the clock that it is worth
	 *  restarting it from where playback has actually got to. */
	let resyncs = $state(0);

	// Every seek or edit restarts the stream, so a timeline that cannot render
	// would raise the same failure on each restart; say it once per spell.
	let lastPlaybackError = { message: '', at: 0 };
	function reportPlaybackError(message: string) {
		const now = Date.now();
		if (message === lastPlaybackError.message && now - lastPlaybackError.at < 15_000) return;
		lastPlaybackError = { message, at: now };
		toast.error(`Playback preview failed: ${message}`);
	}

	function endStream() {
		stopStream?.();
		stopStream = null;
		streaming = false;
	}

	$effect(() => {
		// Restart the stream whenever what it is rendering changes: play/pause,
		// shuttle rate, a deliberate seek, or an edit to the timeline it
		// composited from. Deliberately *not* `ui.time` — that ticks with every
		// animation frame during playback, and depending on it would tear down
		// and respawn ffmpeg 60 times a second.
		const play = ui.playing && ui.rate === 1;
		void ui.seekEpoch;
		void resyncs;
		void editor.timeline;
		void ui.previewEpoch;
		const from = untrack(() => ui.time);
		if (!play || !hasClips) {
			endStream();
			return;
		}
		let live = true;
		const verdict = createFrameGate();
		streaming = true;
		const stop = startPlayback(from, PLAYBACK_FPS, (f) => {
			if (!live) return;
			// The audio clock owns time; picture chases it.
			switch (verdict(ui.time - f.time)) {
				case 'resync':
					// Compositing can't keep up with real time on this timeline. Playing
					// the backlog out would run the picture in slow motion against the
					// sound, and dropping it forever would freeze the pane — so jump the
					// stream forward to where playback has actually got to.
					live = false;
					resyncs++;
					return;
				case 'skip':
					// A frame the clock has moved past would drag the picture behind the
					// sound, so wait for one that still applies.
					return;
				case 'show':
					frameUrl = f.jpeg;
			}
		}, reportPlaybackError);
		stopStream = stop;
		return () => {
			live = false;
			stop();
			if (stopStream === stop) stopStream = null;
			streaming = false;
		};
	});

	// Keep the preview in step with the playhead *and* the edit state: re-render on
	// every playhead move, on every timeline change (an Inspector edit reassigns
	// `editor.timeline`), and when a proxy becomes ready. Suspended while the
	// stream is feeding frames, so the two don't fight over the pane.
	$effect(() => {
		const t = ui.time;
		void editor.timeline;
		void ui.previewEpoch;
		if (!hasClips) {
			frameUrl = null;
			queued = null;
			return;
		}
		if (streaming) return;
		queued = t;
		pump();
	});

	function scrub(e: MouseEvent) {
		const el = e.currentTarget as HTMLElement;
		const x = e.clientX - el.getBoundingClientRect().left;
		ui.seek((x / el.clientWidth) * duration);
	}

	// The preview pane is the delivery frame, not a fixed 16:9 window: cutting a
	// Reel against a landscape box would hide the very crop that decides the shot.
	const delivery = $derived(editor.timeline.format ?? null);
	const aspect = $derived(delivery ? delivery.width / delivery.height : 16 / 9);
	// Which axis binds depends on the pane's shape as much as the frame's — a 1:1
	// frame is height-bound in a wide pane, and choosing by `aspect < 1` alone
	// squashed it to the pane's height at full width. `100cqh` reads the pane's
	// own height (it declares `container-type:size`), so one rule covers every
	// orientation: never wider than the pane or 720px, never taller than the pane.
	const frameBox = $derived(`width:min(100%, 720px, calc(100cqh * ${aspect}))`);

	// Roughly where a phone's UI covers a vertical video: the top status strip,
	// the caption / button rail along the bottom, and the action column on the
	// right. Percentages of the frame, deliberately generous — every app differs,
	// so this is "keep the subject out of here", not a pixel contract.
	const CHROME = { top: 0.08, bottom: 0.2, right: 0.14 };
	const showGuides = $derived(settings.safeAreas && !!delivery && aspect < 1.2);


	// ---- titles: move and resize them where they are drawn -----------------------
	//
	// A title is centred on (pos_x, pos_y), both fractions of the frame, and its
	// font is `size` of the frame's height — so the box is laid out in the same
	// units: `cqh` against this layer (a size container, exactly the frame the
	// engine draws into), with the browser's own text metrics standing in for
	// drawtext's. The layer covers the picture rather than the pane, because a
	// source that is not the frame's shape is letterboxed inside it and the
	// fractions are of what is drawn.

	let imgAspect = $state<number | null>(null);
	const layerBox = $derived.by(() => {
		if (!frameUrl || !imgAspect) return 'inset:0';
		const r = containRect(imgAspect, aspect);
		return `left:${r.left}%;top:${r.top}%;width:${r.width}%;height:${r.height}%`;
	});

	const titlesHere = $derived.by(() => {
		if (ui.playing || empty) return [];
		const here = editor.overlays.filter((o) => isVisibleAt(o, ui.time));
		return here.sort((a, b) => Number(a.id === editor.selectedOverlayId) - Number(b.id === editor.selectedOverlayId));
	});

	type TitleDrag = {
		id: string;
		mode: 'move' | 'resize';
		at: number;
		startX: number;
		startY: number;
		w: number;
		h: number;
		from: { x: number; y: number };
		startSize: number;
		cx: number;
		cy: number;
		startDist: number;
		x: number;
		y: number;
		size: number;
		moved: boolean;
		committing: boolean;
	};
	let tdrag = $state<TitleDrag | null>(null);
	let layerEl = $state<HTMLElement | null>(null);
	let capture: { el: Element; id: number } | null = null;

	function releaseTitleCapture() {
		if (capture) {
			try {
				capture.el.releasePointerCapture(capture.id);
			} catch {
				// already released
			}
		}
		capture = null;
	}

	/** Abandon the gesture without writing anything: the pointer is gone (cancel,
	 *  Escape, a window blur) and wherever it last reported is not to be trusted. */
	function cancelTitleDrag() {
		if (!tdrag || tdrag.committing) return;
		tdrag = null;
		releaseTitleCapture();
	}

	function onTitleDown(e: PointerEvent, o: TextOverlay, mode: 'move' | 'resize') {
		if (e.button !== 0 || tdrag || !layerEl) return;
		e.stopPropagation();
		e.preventDefault();
		editor.selectOverlay(o.id);
		ui.pause();
		const rect = layerEl.getBoundingClientRect();
		const box = (e.currentTarget as HTMLElement).closest('[data-title-box]')!.getBoundingClientRect();
		const cx = box.left + box.width / 2;
		const cy = box.top + box.height / 2;
		const pose = sampleOverlay(o, ui.time);
		tdrag = {
			id: o.id,
			mode,
			at: ui.time,
			startX: e.clientX,
			startY: e.clientY,
			w: rect.width,
			h: rect.height,
			from: { x: pose.x, y: pose.y },
			startSize: o.size,
			cx,
			cy,
			startDist: Math.hypot(e.clientX - cx, e.clientY - cy),
			x: pose.x,
			y: pose.y,
			size: o.size,
			moved: false,
			committing: false
		};
		try {
			(e.currentTarget as Element).setPointerCapture(e.pointerId);
			capture = { el: e.currentTarget as Element, id: e.pointerId };
		} catch {
			// best-effort: the window blur and buttons checks below are the fallback
		}
	}

	function onTitleMove(e: PointerEvent) {
		const d = tdrag;
		if (!d || d.committing) return;
		if ((e.buttons & 1) === 0) {
			cancelTitleDrag();
			return;
		}
		if (d.mode === 'move') {
			const p = dragPosition(d.from, e.clientX - d.startX, e.clientY - d.startY, d.w, d.h);
			tdrag = { ...d, ...p, moved: d.moved || Math.hypot(e.clientX - d.startX, e.clientY - d.startY) >= 3 };
		} else {
			const dist = Math.hypot(e.clientX - d.cx, e.clientY - d.cy);
			tdrag = { ...d, size: scaledSize(d.startSize, d.startDist, dist), moved: d.moved || Math.abs(dist - d.startDist) >= 2 };
		}
	}

	const round4 = (v: number) => Math.round(v * 10000) / 10000;

	/** One backend edit per gesture, so undo steps over the whole drag. The local
	 *  pose is kept until the edit has landed so the box never snaps back first. */
	async function onTitleUp() {
		const d = tdrag;
		if (!d || d.committing) return;
		releaseTitleCapture();
		if (!d.moved) {
			tdrag = null;
			return;
		}
		tdrag = { ...d, committing: true };
		try {
			if (d.mode === 'move') await editor.moveOverlay(d.id, round4(d.x), round4(d.y), d.at);
			else await editor.updateOverlay(d.id, { size: round4(d.size) });
		} catch (err) {
			toast.error(err instanceof Error ? err.message : String(err));
		} finally {
			tdrag = null;
		}
	}

	function onTitleLostCapture() {
		if (tdrag && !tdrag.committing) cancelTitleDrag();
	}

	/** An ffmpeg colour is not always a CSS one (`yellow@0.9`); the drag ghost
	 *  falls back to white rather than to invisible. */
	const ghostColor = (c: string) =>
		typeof CSS !== 'undefined' && CSS.supports('color', c) ? c : 'var(--text-on-video)';

	const HANDLES = [
		{ at: 'left:0;top:0', cursor: 'nwse-resize' },
		{ at: 'left:100%;top:0', cursor: 'nesw-resize' },
		{ at: 'left:0;top:100%', cursor: 'nesw-resize' },
		{ at: 'left:100%;top:100%', cursor: 'nwse-resize' }
	];

	/** Write the frame under the playhead as a cover image — the thumbnail a
	 *  platform shows before anyone presses play. Rendered at the full delivery
	 *  frame from the original media, so it is the picture people actually see,
	 *  not the downscaled preview on screen. */
	async function saveCover() {
		if (!inTauri()) {
			toast.info('Cover frames are rendered with FFmpeg in the desktop app.');
			return;
		}
		const path = await pickCoverPath();
		if (!path) return;
		try {
			const out = await exportCover(ui.time, path);
			toast.success(`Cover saved → ${out}`, {
				action: { label: 'Show in folder', onClick: () => void revealPath(out).catch(() => {}) }
			});
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function onPreviewContextMenu(e: MouseEvent) {
		contextMenu.show(e, [
			{
				label: ui.playing ? 'Pause' : 'Play',
				icon: ui.playing ? 'pause' : 'play',
				shortcut: 'Space',
				disabled: empty,
				action: () => ui.togglePlay()
			},
			{ type: 'separator' },
			{ label: 'Go to start', icon: 'skip-back', shortcut: 'Home', disabled: empty, action: () => ui.seek(0) },
			{
				label: 'Go to end',
				icon: 'skip-forward',
				shortcut: 'End',
				disabled: empty,
				action: () => ui.seek(editor.duration)
			},
			{ type: 'separator' },
			{
				label: settings.safeAreas ? 'Hide safe areas' : 'Show safe areas',
				icon: 'crop',
				disabled: !delivery,
				action: () => void settings.setSafeAreas(!settings.safeAreas)
			},
			{ type: 'separator' },
			{
				label: 'Save cover frame…',
				icon: 'image',
				disabled: empty,
				action: () => void saveCover()
			}
		]);
	}
</script>

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape' && tdrag && !tdrag.committing) cancelTitleDrag();
	}}
	onblur={cancelTitleDrag}
/>

<div style="flex:1;min-height:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<div
		role="presentation"
		oncontextmenu={onPreviewContextMenu}
		style="flex:1;min-height:0;display:grid;place-items:center;padding:20px;position:relative;container-type:size"
	>
		{#if empty}
			<div style="display:flex;flex-direction:column;align-items:center;gap:12px;color:var(--text-disabled)">
				<Icon n="clapperboard" s={30} /><span style="font-size:13px">No media loaded</span>
			</div>
		{:else}
			<div
				style="position:relative;aspect-ratio:{aspect};{frameBox};border-radius:4px;background:radial-gradient(120% 120% at 30% 20%, var(--surface-active) 0%, var(--surface-raised) 55%, var(--surface-void) 100%);border:var(--line-width) solid var(--border-default);box-shadow:var(--shadow-md)"
			>
				{#if frameUrl}
					<img src={frameUrl} alt="preview frame" style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:var(--frame-matte)"
						onload={(e) => {
							const im = e.currentTarget as HTMLImageElement;
							if (im.naturalWidth && im.naturalHeight) imgAspect = im.naturalWidth / im.naturalHeight;
						}}
					/>
				{:else}
					<div style="position:absolute;inset:0;background:linear-gradient(115deg, transparent 40%, color-mix(in srgb,var(--kerf-500) 6%,transparent) 60%)"></div>
					<div style="position:absolute;inset:0;display:grid;place-items:center;color:color-mix(in srgb,var(--text-on-video) 22%,transparent)">
						<Icon n={ui.playing ? 'pause' : 'play'} s={44} />
					</div>
				{/if}
				{#if showGuides}
					<!-- Where the platform's own UI sits over the picture. Shaded, not
					     cropped: the pixels still render, they are just not yours to
					     put a face or a caption in. -->
					<div style="position:absolute;inset:0;pointer-events:none">
						<div style="position:absolute;left:0;right:0;top:0;height:{CHROME.top * 100}%;background:color-mix(in srgb,var(--scrim) 34%,transparent);border-bottom:1px dashed color-mix(in srgb,var(--text-on-video) 28%,transparent)"></div>
						<div style="position:absolute;left:0;right:0;bottom:0;height:{CHROME.bottom * 100}%;background:color-mix(in srgb,var(--scrim) 34%,transparent);border-top:1px dashed color-mix(in srgb,var(--text-on-video) 28%,transparent)"></div>
						<div
							style="position:absolute;right:0;top:{CHROME.top * 100}%;bottom:{CHROME.bottom * 100}%;width:{CHROME.right * 100}%;background:color-mix(in srgb,var(--scrim) 24%,transparent);border-left:1px dashed color-mix(in srgb,var(--text-on-video) 20%,transparent)"
						></div>
						<div
							style="position:absolute;left:5%;right:5%;top:5%;bottom:5%;border:var(--line-width) solid color-mix(in srgb,var(--text-on-video) 14%,transparent);border-radius:2px"
						></div>
					</div>
				{/if}
				<!-- Titles under the playhead, as boxes over where the engine draws them.
				     The text is transparent (the picture already has it) except while
				     it is being dragged, when the box carries a ghost of it. -->
				{#if titlesHere.length}
					<div
						bind:this={layerEl}
						style="position:absolute;{layerBox};container-type:size;pointer-events:none"
					>
						{#each titlesHere as o (o.id)}
							{@const live = tdrag?.id === o.id ? tdrag : null}
							{@const pose = sampleOverlay(o, ui.time)}
							{@const selected = editor.selectedOverlayId === o.id}
							{@const pad = boxPadding(o, delivery?.height) * 100}
							<div
								role="presentation"
								data-title-box
								title={selected ? undefined : `Select “${o.text}”`}
								onpointerdown={(e) => onTitleDown(e, o, 'move')}
								onpointermove={onTitleMove}
								onpointerup={onTitleUp}
								onpointercancel={cancelTitleDrag}
								onlostpointercapture={onTitleLostCapture}
								style="position:absolute;left:{(live?.x ?? pose.x) * 100}%;top:{(live?.y ?? pose.y) * 100}%;transform:translate(-50%,-50%);padding:{pad}cqh;font-size:{(live?.size ?? o.size) * 100}cqh;line-height:{LINE_HEIGHT};font-weight:{o.bold ? 700 : 400};font-family:{o.font ? `'${o.font.replace(/['\\]/g, '')}', ` : ''}sans-serif;white-space:nowrap;color:{live?.moved ? ghostColor(o.color) : 'transparent'};pointer-events:auto;touch-action:none;cursor:{live ? 'grabbing' : 'move'};user-select:none;outline:{selected ? '1.5px solid var(--kerf-400)' : '1px dashed color-mix(in srgb,var(--text-on-video) 35%,transparent)'};outline-offset:0;background:{live?.moved ? 'color-mix(in srgb,var(--scrim) 25%,transparent)' : 'transparent'}"
							>
								{o.text}
								{#if selected}
									{#each HANDLES as h (h.at)}
										<span
											role="presentation"
											onpointerdown={(e) => onTitleDown(e, o, 'resize')}
											style="position:absolute;{h.at};width:10px;height:10px;transform:translate(-50%,-50%);background:var(--kerf-400);border:1.5px solid var(--surface-app);border-radius:2px;cursor:{h.cursor};touch-action:none;font-size:0"
										></span>
									{/each}
								{/if}
							</div>
						{/each}
					</div>
				{/if}
				<div style="position:absolute;left:14px;top:12px;display:flex;gap:6px">
					<Badge tone="kerf">{previewAsset?.name ?? 'preview'}</Badge>
					{#if ui.analyzing}<Badge tone="agent" dot>{ui.analysisLabel ?? 'analyzing'}</Badge>{/if}
				</div>
				<div
					style="position:absolute;right:14px;top:12px;font-family:var(--font-mono);font-size:11px;color:color-mix(in srgb,var(--text-on-video) 55%,transparent)"
				>
					{resolution}{fpsLabel ? ` · ${fpsLabel}` : ''}
				</div>
				<div
					style="position:absolute;left:14px;bottom:12px;font-family:var(--font-mono);font-size:12px;color:var(--kerf-200)"
				>
					{tc(ui.time)}
				</div>
			</div>
		{/if}
	</div>
	<div
		style="height:40px;flex:none;display:flex;align-items:center;gap:12px;padding:0 16px;border-top:var(--line-width) solid var(--border-default);background:var(--surface-app)"
	>
		<button
			title={ui.playing ? 'Pause' : 'Play'}
			aria-label={ui.playing ? 'Pause' : 'Play'}
			onclick={() => ui.togglePlay()}
			style="background:none;border:none;cursor:pointer;color:var(--text-primary);display:grid;place-items:center"
		>
			<Icon n={ui.playing ? 'pause' : 'play'} s={16} />
		</button>
		<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-secondary)">{tc(ui.time)}</span>
		<div
			role="presentation"
			onclick={scrub}
			style="flex:1;height:4px;border-radius:999px;background:var(--surface-inset);position:relative;cursor:pointer"
		>
			<div
				style="position:absolute;inset:0 auto 0 0;width:{empty ? 0 : (ui.time / duration) * 100}%;background:var(--kerf-500);border-radius:999px"
			></div>
			<div
				style="position:absolute;left:{empty ? 0 : (ui.time / duration) * 100}%;top:50%;width:11px;height:11px;border-radius:50%;background:var(--kerf-400);transform:translate(-50%,-50%);box-shadow:0 0 0 3px var(--surface-app)"
			></div>
		</div>
		<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{tc(duration)}</span>
	</div>
</div>
