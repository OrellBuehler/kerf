<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import { formatTimecode } from '$lib/timecode';
	import Badge from './Badge.svelte';
	import TrimMonitor from './TrimMonitor.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import { getPreviewFrame, getTimelineFrame, setPreviewBounds, startPlayback } from '$lib/api';
	import { saveCoverFrame } from '$lib/file-actions';
	import { gpuPreview } from '$lib/gpu-preview.svelte';
	import {
		anyCovered,
		boundsReport,
		describeWhy,
		holePolygon,
		nextSeq,
		parseCssColor,
		routePreview,
		sameReport,
		samplePoints,
		surfaceShowing
	} from '$lib/preview-bounds';
	import { createFramePump } from '$lib/frame-pump.svelte';
	import { singleFlight } from '$lib/single-flight';
	import type { PreviewBoundsReport } from '$lib/types';
	import { toast } from '$lib/notifications.svelte';
	import { createFrameGate, PLAYBACK_FPS } from '$lib/playback-sync';
	import { clipDuration } from '$lib/types';
	import type { TextOverlay } from '$lib/types';
	import { LINE_HEIGHT, boxPadding, containRect, dragPosition, isVisibleAt, sampleOverlay, scaledSize } from '$lib/titles';

	const duration = $derived(Math.max(editor.duration, 0.001));
	/** The transport bar's own width: what it can show depends on it, not on the window. */
	let barWidth = $state(0);
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

	// The frame under the playhead: fetched one at a time (frame-pump.svelte.ts), as FFmpeg's JPEG
	// or — with the GPU preview on — drawn by the backend in the native surface, in which case
	// `gpuShown` is true and the pane paints nothing over it.
	const frames = createFramePump({
		time: () => ui.time,
		timeline: () => editor.timeline,
		previewEpoch: () => ui.previewEpoch,
		routeVia: () => routeVia,
		routeOverlays: () => routeOverlays,
		boundsEpoch: () => boundsEpoch,
		hasClips: () => hasClips,
		streaming: () => streaming,
		timelineFrame: (t) => getTimelineFrame(t, 960),
		previewFrame: (t, overlays) => getPreviewFrame(t, 960, overlays),
		answered: (result) => gpuPreview.note(result),
		gpuFailed: (error) => {
			gpuPreview.noteJpeg(`the GPU preview failed: ${error instanceof Error ? error.message : String(error)}`);
			hideSurface();
		}
	});
	const frameUrl = $derived(frames.frameUrl);
	const gpuShown = $derived(frames.gpuShown);
	const imgAspect = $derived(frames.imgAspect);

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
					frames.showStreamed(f.jpeg);
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

	// Keep the preview in step with the playhead *and* the edit state: re-render on every playhead
	// move, on every timeline change (an Inspector edit reassigns `editor.timeline`), when a proxy
	// becomes ready, on a change of route (the GPU takes a frame over, or hands it back for a title
	// box) and when the surface moved. Suspended while the stream is feeding frames, so the two
	// don't fight over the pane. What it depends on is what `run` reads, and nothing else.
	$effect(() => frames.run());

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

	const layerBox = $derived.by(() => {
		if ((!frameUrl && !gpuShown) || !imgAspect) return 'inset:0';
		const r = containRect(imgAspect, aspect);
		return `left:${r.left}%;top:${r.top}%;width:${r.width}%;height:${r.height}%`;
	});

	const titlesHere = $derived.by(() => {
		if (ui.playing || empty) return [];
		const here = editor.overlays.filter((o) => isVisibleAt(o, ui.time));
		return here.sort((a, b) => Number(a.id === editor.selectedOverlayId) - Number(b.id === editor.selectedOverlayId));
	});

	// ---- the GPU preview: how the frame is produced, and where the surface is ------------
	//
	// With the setting off none of this runs: the route is the JPEG path, nothing is observed and
	// no command is sent. With it on, the backend is asked for each frame and says which renderer
	// made it; the surface it draws into sits behind (or, on X11, over) the frame below.

	/** Something of the page is on top of the frame (a dialog, a menu): only the poll below can
	 *  know, since the DOM does not say. Only a surface above the page is hurt by it. */
	let covered = $state(false);
	const route = $derived(
		routePreview({
			enabled: settings.gpuPreview,
			supported: gpuPreview.status?.supported ?? false,
			overlaysCapable: gpuPreview.status?.overlays ?? false,
			streaming,
			empty,
			titlesShown: titlesHere.length > 0,
			trimMonitor: !!ui.trimMonitor,
			guides: showGuides,
			covered
		})
	);
	// The route is a fresh object whenever any input is recomputed (the titles under the playhead
	// are a new array each tick); what an effect may depend on is its primitives, which only
	// notify when they change.
	const routeVia = $derived(route.via);
	const routeOverlays = $derived(route.overlays);
	const routeWhy = $derived(route.why);
	const technique = $derived(gpuPreview.status?.technique ?? null);
	/** The native surface is what is on show in the frame. */
	const onSurface = $derived(routeVia === 'gpu' && surfaceShowing(route, gpuPreview.renderer) && gpuShown);

	let rootEl = $state<HTMLElement | null>(null);
	let frameEl = $state<HTMLElement | null>(null);
	let matteEl = $state<HTMLElement | null>(null);
	let backdropEl = $state<HTMLElement | null>(null);
	let surroundEl = $state<HTMLElement | null>(null);
	/** Bumped when the backend has taken a new place for the surface: the frame is drawn again. */
	let boundsEpoch = $state(0);
	let wantedBounds: PreviewBoundsReport | null = null;
	let sentBounds: PreviewBoundsReport | null = null;
	const sendBounds = singleFlight(async () => {
		const next = wantedBounds;
		if (!next || sameReport(sentBounds, next)) return;
		await setPreviewBounds(next);
		const moved = !sentBounds || next.x !== sentBounds.x || next.y !== sentBounds.y || next.width !== sentBounds.width || next.height !== sentBounds.height;
		sentBounds = next;
		if (next.visible && moved) boundsEpoch++;
	});

	/** The frame's content box (inside its border) in CSS pixels, or null when it is not laid out. */
	function frameRect() {
		const el = frameEl;
		if (!el || !el.isConnected) return null;
		const r = el.getBoundingClientRect();
		const w = el.clientWidth;
		const h = el.clientHeight;
		return w > 0 && h > 0 ? { left: r.left + el.clientLeft, top: r.top + el.clientTop, width: w, height: h } : null;
	}

	/** Whether anything but the frame (and what it holds) is the topmost thing at the frame's points. */
	function frameCovered(): boolean {
		const el = frameEl;
		const rect = frameRect();
		if (!el || !rect) return false;
		return anyCovered(samplePoints(rect), (x, y) => {
			const hit = document.elementFromPoint(x, y);
			return hit ? el.contains(hit) : null;
		});
	}

	function measure(wantSurface: boolean): PreviewBoundsReport {
		return boundsReport({
			rect: frameRect(),
			dpr: window.devicePixelRatio,
			viewport: { width: window.innerWidth, height: window.innerHeight },
			visible: wantSurface && document.visibilityState === 'visible',
			matte: matteEl ? parseCssColor(getComputedStyle(matteEl).backgroundColor) : null,
			backdrop: backdropEl ? parseCssColor(getComputedStyle(backdropEl).backgroundColor) : null,
			seq: nextSeq()
		});
	}

	/** The surface must not be on show (the GPU path failed outright): hide it now, whatever the route says. */
	function hideSurface() {
		if (!settings.gpuPreview || !frameEl) return;
		wantedBounds = measure(false);
		sendBounds.request();
	}

	/** Measure the frame and tell the backend (newest wins). */
	function pushBounds() {
		if (!settings.gpuPreview || !frameEl) return;
		covered = frameCovered();
		wantedBounds = measure(route.via === 'gpu');
		sendBounds.request();
	}

	// The observers live as long as the frame does and the setting is on; what they report is read
	// when they fire, so a change of route or theme does not tear them down.
	$effect(() => {
		if (!settings.gpuPreview || !frameEl) return;
		const el = frameEl;
		const update = () => pushBounds();
		// Size changes are observed; position changes (a dock move, a scrolled pane, a window move,
		// a different monitor's ratio) are caught by the window events and a slow poll.
		const observer = new ResizeObserver(update);
		observer.observe(el);
		if (rootEl) observer.observe(rootEl);
		observer.observe(document.documentElement);
		window.addEventListener('resize', update);
		// A scroll event fires per wheel tick in every scrollable ancestor (capture phase): measure at
		// most once a frame.
		let scrollFrame = 0;
		const onScroll = () => {
			if (scrollFrame) return;
			scrollFrame = requestAnimationFrame(() => {
				scrollFrame = 0;
				update();
			});
		};
		window.addEventListener('scroll', onScroll, true);
		document.addEventListener('visibilitychange', update);
		// A dialog or a menu opening is not an event the frame hears about: look often, and straight
		// after a click, a right-click or a key.
		const poll = setInterval(update, 150);
		const soon = () => requestAnimationFrame(() => requestAnimationFrame(update));
		for (const type of ['pointerup', 'contextmenu', 'keyup']) window.addEventListener(type, soon, true);
		untrack(update);
		return () => {
			observer.disconnect();
			window.removeEventListener('resize', update);
			window.removeEventListener('scroll', onScroll, true);
			if (scrollFrame) cancelAnimationFrame(scrollFrame);
			document.removeEventListener('visibilitychange', update);
			for (const type of ['pointerup', 'contextmenu', 'keyup']) window.removeEventListener(type, soon, true);
			clearInterval(poll);
			covered = false;
			// The panel is gone (a workspace switch, the setting turned off): the surface must not
			// stay over whatever is there now.
			wantedBounds = boundsReport({
				rect: null,
				dpr: 1,
				viewport: { width: 0, height: 0 },
				visible: false,
				matte: null,
				backdrop: null,
				seq: nextSeq()
			});
			sendBounds.request();
		};
	});

	// A different route, theme or technique changes what is reported (visible, the colours) at once.
	$effect(() => {
		void routeVia;
		void settings.theme;
		void technique;
		untrack(pushBounds);
	});

	// Turning the setting off forgets the renderer, so the status bar goes quiet; while it is on, a
	// frame that is the JPEG by the page's own choice (playback, a dialog over the frame) says so.
	$effect(() => {
		if (!settings.gpuPreview) {
			frames.surfaceGone();
			gpuPreview.clear();
		} else if (routeVia === 'jpeg' && routeWhy) {
			gpuPreview.noteJpeg(describeWhy(routeWhy));
		}
	});

	// Under a transparent webview (technique `window`) the surface is seen through the page: every
	// element that paints behind the frame is made transparent, and the pane's own surround is
	// painted by a layer with a hole where the frame is. Nothing else on the page changes.
	$effect(() => {
		if (!onSurface || technique !== 'window' || !rootEl || !frameEl || !surroundEl) return;
		const tagged: HTMLElement[] = [];
		for (let el = rootEl.parentElement; el; el = el.parentElement) {
			el.setAttribute('data-surface-hole', '');
			tagged.push(el);
		}
		const surround = surroundEl;
		const cut = () => {
			const outer = surround.getBoundingClientRect();
			const frame = frameRect();
			surround.style.clipPath = frame
				? holePolygon(outer, { x: frame.left - outer.left, y: frame.top - outer.top, width: frame.width, height: frame.height })
				: '';
		};
		cut();
		const observer = new ResizeObserver(cut);
		observer.observe(surround);
		observer.observe(frameEl);
		return () => {
			observer.disconnect();
			for (const el of tagged) el.removeAttribute('data-surface-hole');
			surround.style.clipPath = '';
		};
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

	function onPreviewContextMenu(e: MouseEvent) {
		contextMenu.show(e, [
			{
				label: ui.playing ? 'Pause' : 'Play',
				icon: ui.playing ? 'pause' : 'play',
				shortcut: settings.shortcut('playback.toggle'),
				disabled: empty,
				action: () => ui.togglePlay()
			},
			{ type: 'separator' },
			{ label: 'Go to start', icon: 'skip-back', shortcut: settings.shortcut('playback.toStart'), disabled: empty, action: () => ui.seek(0) },
			{
				label: 'Go to end',
				icon: 'skip-forward',
				shortcut: settings.shortcut('playback.toEnd'),
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
				action: () => void saveCoverFrame()
			}
		]);
	}
</script>

<svelte:window
	onkeydowncapture={(e) => {
		// Abandoning a drag is all Escape does then — not also "clear the selection".
		if (e.key === 'Escape' && tdrag && !tdrag.committing) {
			cancelTitleDrag();
			e.stopPropagation();
		}
	}}
	onblur={cancelTitleDrag}
/>

<div
	bind:this={rootEl}
	style="flex:1;min-height:0;display:flex;flex-direction:column;{onSurface && technique === 'window' ? 'position:relative;background:transparent' : 'background:var(--surface-void)'}"
>
	{#if onSurface && technique === 'window'}
		<!-- The pane's surround, with a hole where the frame is: the surface shows through there. -->
		<div bind:this={surroundEl} aria-hidden="true" style="position:absolute;inset:0;background:var(--surface-void);pointer-events:none"></div>
	{/if}
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
				bind:this={frameEl}
				data-gpu-frame={onSurface ? 'surface' : undefined}
				style="position:relative;aspect-ratio:{aspect};{frameBox};border-radius:4px;background:{gpuShown ? 'transparent' : 'radial-gradient(120% 120% at 30% 20%, var(--surface-active) 0%, var(--surface-raised) 55%, var(--surface-void) 100%)'};border:var(--line-width) solid var(--border-default);box-shadow:var(--shadow-md)"
			>
				{#if settings.gpuPreview}
					<!-- Colour probes: what the backend paints around the picture and behind the page. -->
					<div bind:this={matteEl} aria-hidden="true" style="position:absolute;width:0;height:0;background:var(--frame-matte)"></div>
					<div bind:this={backdropEl} aria-hidden="true" style="position:absolute;width:0;height:0;background:var(--surface-app)"></div>
				{/if}
				{#if gpuShown}
					<!-- Drawn by the GPU in the native surface: nothing here covers it. -->
				{:else if frameUrl}
					<img src={frameUrl} alt="preview frame" style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:var(--frame-matte)"
						onload={(e) => {
							const im = e.currentTarget as HTMLImageElement;
							if (im.naturalWidth && im.naturalHeight) frames.pictureAspect(im.naturalWidth / im.naturalHeight);
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
				<!-- A roll, slip or slide being dragged: the frames either side of the edit
				     stand in for the playhead's frame until it is let go. -->
				{#if ui.trimMonitor}
					<TrimMonitor monitor={ui.trimMonitor} />
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
	<!-- Transport: go to start, play / pause, go to end, the timecode with the
	     timeline's rate, and the scrub bar. J / K / L stay on the keyboard. It narrows
	     gracefully: below ~380 px the duration and the rate go, then the skip buttons'
	     gaps close — the controls themselves stay. -->
	<div
		bind:clientWidth={barWidth}
		style="height:40px;flex:none;display:flex;align-items:center;gap:{barWidth < 380 ? 6 : 10}px;padding:0 {barWidth < 380 ? 8 : 14}px;border-top:var(--line-width) solid var(--border-default);background:var(--surface-app)"
	>
		<button
			title={settings.withShortcut('Go to start', 'playback.toStart')}
			aria-label="Go to start"
			disabled={empty}
			onclick={() => ui.seek(0)}
			style="background:none;border:none;cursor:{empty ? 'default' : 'pointer'};color:var(--text-secondary);opacity:{empty ? 0.4 : 1};display:grid;place-items:center;padding:3px"
		>
			<Icon n="skip-back" s={14} />
		</button>
		<button
			title={settings.withShortcut(ui.playing ? 'Pause' : 'Play', 'playback.toggle')}
			aria-label={ui.playing ? 'Pause' : 'Play'}
			onclick={() => ui.togglePlay()}
			style="background:var(--surface-hover);border:none;border-radius:var(--radius-sm);cursor:pointer;color:var(--text-primary);display:grid;place-items:center;padding:4px 6px"
		>
			<Icon n={ui.playing ? 'pause' : 'play'} s={16} />
		</button>
		<button
			title={settings.withShortcut('Go to end', 'playback.toEnd')}
			aria-label="Go to end"
			disabled={empty}
			onclick={() => ui.seek(editor.duration)}
			style="background:none;border:none;cursor:{empty ? 'default' : 'pointer'};color:var(--text-secondary);opacity:{empty ? 0.4 : 1};display:grid;place-items:center;padding:3px"
		>
			<Icon n="skip-forward" s={14} />
		</button>
		<span
			title="Timeline timecode · {editor.fps.toFixed(3)} fps · non-drop"
			style="font-family:var(--font-mono);font-size:12px;color:var(--kerf-300);font-weight:500;white-space:nowrap">{tc(ui.time)}</span
		>
		<div
			role="presentation"
			onclick={scrub}
			style="flex:1;min-width:24px;height:4px;border-radius:999px;background:var(--surface-inset);position:relative;cursor:pointer"
		>
			<div
				style="position:absolute;inset:0 auto 0 0;width:{empty ? 0 : (ui.time / duration) * 100}%;background:var(--kerf-500);border-radius:999px"
			></div>
			<div
				style="position:absolute;left:{empty ? 0 : (ui.time / duration) * 100}%;top:50%;width:11px;height:11px;border-radius:50%;background:var(--kerf-400);transform:translate(-50%,-50%);box-shadow:0 0 0 3px var(--surface-app)"
			></div>
		</div>
		{#if barWidth >= 380}
			<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap">{tc(duration)}</span>
			<span
				title="Timeline frame rate; non-drop timecode"
				style="font-size:11px;color:var(--text-muted);white-space:nowrap">{Number(editor.fps.toFixed(3))} fps</span
			>
		{/if}
	</div>
</div>
