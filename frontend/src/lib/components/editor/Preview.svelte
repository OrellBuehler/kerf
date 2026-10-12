<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import PreviewEmpty from './PreviewEmpty.svelte';
	import { formatTimecode } from '$lib/timecode';
	import ScrubBar from './ScrubBar.svelte';
	import TrimMonitor from './TrimMonitor.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import { onWindow } from '$lib/window-events';
	import { windows } from '$lib/windows.svelte';
	import { resizeObserverFor, windowOf } from '$lib/realm';
	import { getPreviewFrame, getTimelineFrame, setPreviewBounds, startPlayback } from '$lib/api';
	import { saveCoverFrame } from '$lib/file-actions';
	import { gpuPreview } from '$lib/gpu-preview.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { isProxyWaitMessage, previewSourceNote, previewSourceWidth } from '$lib/proxy-info';
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
	import { measuredWidth, transportParts } from '$lib/transport-bar';
	import { RESIZE_SETTLE_MS, capToSource, dprQuery, requestWidth } from '$lib/preview-size';
	import type { PlayAhead, PreviewBoundsReport } from '$lib/types';
	import { toast } from '$lib/notifications.svelte';
	import { createFrameGate, createLadder, playbackFps } from '$lib/playback-sync';
	import { GpuPlayback } from '$lib/gpu-playback';
	import { audio } from '$lib/audio';
	import { CANVAS_PAINTER, createStreamPainter, jpegDataUrl } from '$lib/frame-transport';
	import { decodeJpeg, paintPicture } from '$lib/frame-canvas';
	import { renderedDuration, renderedFps } from '$lib/timecode';
	import { logFrontend } from '$lib/log';
	import { clipDuration } from '$lib/types';
	import type { TextOverlay } from '$lib/types';
	import { boxPadding, containRect, dragPosition, inkBox, isVisibleAt, sampleOverlay, scaledSize, titleFamily } from '$lib/titles';
	import { tip } from '$lib/tip';

	const duration = $derived(Math.max(editor.duration, 0.001));
	/** The transport bar's own width, or undefined until it has been measured (and while it
	 *  reads 0: hidden, mid-rebuild). What the bar can show depends on it, not on the window;
	 *  an unknown width shows all of it (`transportParts`). */
	let barEl = $state<HTMLElement | null>(null);
	let barWidth = $state<number | undefined>(undefined);
	const parts = $derived(transportParts(barWidth));
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

	// Under "Proxy only" a clip whose proxy is still building is not decoded from the original: the
	// last good frame stays, and this says what the preview is waiting for.
	const waitingOnProxy = $derived.by(() => {
		const note = previewSourceNote(
			editor.timeline,
			editor.assets,
			mediaStatus.proxies,
			settings.previewSource,
			settings.proxySize,
			ui.time
		);
		return note?.tone === 'waiting' ? note : null;
	});

	// Once the project has a delivery frame, that is the shape on screen — showing
	// the source's dimensions here would label the picture with a size it isn't.
	const resolution = $derived.by(() => {
		const d = editor.timeline.format;
		if (d) return `${d.width}×${d.height}`;
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.width && v?.height ? `${v.width}×${v.height}` : '—';
	});
	/** The rate the way the transport's reads (`30 fps`, `29.97 fps`), not `30.000`. */
	const fpsLabel = $derived.by(() => {
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.fps ? `${Number(v.fps.toFixed(3))} fps` : '';
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
		width: () => frameWidth,
		timelineFrame: (t, width) => getTimelineFrame(t, width),
		previewFrame: (t, overlays, width, order, play) => getPreviewFrame(t, width, overlays, order, play),
		answered: (result) => gpuPreview.note(result),
		gpuFailed: (error) => {
			gpuPreview.noteJpeg(`the GPU preview failed: ${error instanceof Error ? error.message : String(error)}`);
			hideSurface();
		},
		playback: () => gpuPlay,
		playClock: () => ({ clock: ui.time, latency: audio.outputLatency(), fps: playFps }),
		startup: () => ladder.startup ?? undefined,
		playHandover: (pb, ahead, now, why) => handOverToStream(pb, ahead, now, why),
		playWaiting: (pb, message) => {
			if (gpuPlay !== pb) return;
			pb.end();
			gpuPlay = null;
			playbackWaiting = message;
		}
	});
	const frameUrl = $derived(frames.frameUrl);
	const gpuShown = $derived(frames.gpuShown);
	const streamShown = $derived(frames.streamShown);
	const imgAspect = $derived(frames.imgAspect);

	// ---- playback: a streamed frame source instead of per-frame decodes -------
	//
	// While playing forward at 1×, one long-lived ffmpeg composites the timeline
	// from the playhead and pushes frames up as they render. Scrubbing, shuttle
	// and the paused frame keep the per-frame path below: that one is right when
	// you want *one* frame, and streaming is right when you want all of them.
	//
	// The stream plays at the cut's own frame rate (`renderedFps`: the cut as it renders, as the
	// engine reads it) and the width the pane asks for. Its frames are raw JPEG bytes; the ones
	// the gate lets through become a data URL for the pane's one `<img>` (`frame-transport.ts`: with
	// `CANVAS_PAINTER` on, a canvas decoded by `createImageBitmap` instead). A machine that cannot hold
	// the rate is not played behind the sound: the gate says so and the stream restarts from the
	// playhead one rung cheaper (`createLadder`, `playback-sync.ts`), halving the rate before it
	// narrows the picture.
	//
	// **With the GPU preview on, the GPU may play the session instead** (`gpu-playback.ts`): the frame
	// pump (`frame-pump.svelte.ts`) asks the backend for the picture of the time that is audible now,
	// once per animation frame, and the first answer decides the session — `gpu`, or `stream` with the
	// reasons, in which case the stream below starts as it always did. Once the GPU has the session it
	// keeps it until the engine says where it will stop drawing (`ahead`), and the stream is started
	// just early enough to carry on from there; the GPU's last frame stays on the surface until the
	// stream's first one is shown. No flipping back: a session given up on stays the stream's until
	// the playhead is moved on purpose or the cut is edited.
	let streaming = $state(false);
	let stopStream: (() => void) | null = null;
	let canvasEl = $state<HTMLCanvasElement | null>(null);
	/** Bumped when the stream has fallen so far behind the clock that it is worth
	 *  restarting it from where playback has actually got to. */
	let resyncs = $state(0);
	/** How far down the ladder the stream is for this cut at this width, and where the next one starts.
	 *  Not reactive: it is read when the stream (re)starts, not watched. */
	const ladder = createLadder();
	/** The GPU's play session, being decided or played (null: the stream's, or not playing). The pump reads it. */
	let gpuPlay = $state.raw<GpuPlayback | null>(null);
	/** Starts the stream beside the GPU's session (`takeover`: that session stays on the screen until the
	 *  stream's first frame, from `handover.at` on, is shown); set by the playback effect while it runs. */
	let beginStream: ((handover: PlayAhead | null, takeover: boolean) => void) | null = null;
	/** The play the GPU was given up on in: not tried again until the playhead is moved on purpose or the
	 *  cut is edited (a restart of the stream for lag is neither). */
	let gpuBarred: { seek: number; timeline: unknown } | null = null;
	/** The rate the GPU's session was asked for at: the cut's own. */
	let playFps = 30;

	// Every seek or edit restarts the stream, so a timeline that cannot render
	// would raise the same failure on each restart; say it once per spell.
	let lastPlaybackError = { message: '', at: 0 };
	/** Set while playback cannot start because "Proxy only" is waiting for a clip's proxy: the
	 *  message the backend gave (naming the clip and how far its proxy is). Not an error — it
	 *  clears when a proxy lands (the stream restarts then) and the picture on show stays. */
	let playbackWaiting = $state<string | null>(null);
	function reportPlaybackError(message: string) {
		if (isProxyWaitMessage(message)) {
			playbackWaiting = message;
			return;
		}
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

	/** The GPU's session goes to the stream: at once (`now`), or when the playhead has got to where the GPU stops. */
	function handOverToStream(pb: GpuPlayback, ahead: PlayAhead | null, now: boolean, why: string) {
		if (gpuPlay !== pb || !beginStream || pb.handingOver) return;
		logFrontend(
			'info',
			`GPU playback hands over to the stream${now ? ' at once' : ` at ${ahead?.handover.toFixed(2)} s`}: ${why}` +
				` (${pb.drawn} frames drawn, ${pb.repeated} repeated, round trip ${Math.round(pb.roundTrip * 1000)} ms)`,
			'playback'
		);
		gpuBarred = { seek: ui.seekEpoch, timeline: editor.timeline };
		if (now) pb.end();
		pb.handOver();
		beginStream(ahead, true);
	}

	$effect(() => {
		// Restart the stream whenever what it is rendering changes: play/pause,
		// shuttle rate, a deliberate seek, an edit to the timeline it composited
		// from, or the width the pane asks for. Deliberately *not* `ui.time` — that
		// ticks with every animation frame during playback, and depending on it would
		// tear down and respawn ffmpeg 60 times a second.
		const play = ui.playing && ui.rate === 1;
		void ui.seekEpoch;
		void resyncs;
		const timeline = editor.timeline;
		const epoch = ui.previewEpoch;
		const box = boxWidth;
		playbackWaiting = null;
		if (!play || !hasClips) {
			endStream();
			return;
		}
		// Whether the GPU plays this session is decided here, once: the route as it is now (the setting, a
		// surface, nothing over the frame that the surface cannot show) and no earlier give-up of this play.
		const gpu = untrack(() => {
			const barred = gpuBarred !== null && gpuBarred.seek === ui.seekEpoch && gpuBarred.timeline === timeline;
			return settings.gpuPreview && route.via === 'gpu' && !barred;
		});
		let active = true;
		let stopped: (() => void) | null = null;

		/** Start the stream; returns what stops it. With `takeover` the GPU's session is still on the screen
		 *  and the stream does not take the pane until its first frame at or past where the GPU stops. */
		const startStream = (handover: PlayAhead | null, takeover: boolean): (() => void) => {
			// The width is the one the still is asked at, held to the source at the playhead; read
			// untracked, since that source changes as the playhead crosses clips and the stream
			// is not restarted for it.
			// A stream that takes a session over from the GPU carries on in the time the GPU draws: the audible
			// one (the clock less the sound's output latency), and from where the engine said it may start
			// (`handover`), or the fade or transition the refused frame sits in is cut off.
			const { rung, request, from, offset, lead, cheaper, heal } = untrack(() =>
				ladder.start(
					{ timeline, epoch, width: box },
					{
						fps: renderedFps(editor.timeline, editor.assets),
						width: frameWidth,
						playhead: ui.time,
						duration: renderedDuration(editor.timeline),
						latency: takeover ? (gpuPlay?.latency ?? 0) : 0,
						earliest: takeover ? (handover?.handover ?? null) : null
					}
				)
			);
			let live = true;
			let taken = !takeover;
			// At the cheapest rung there is nothing to restart into, and a restart costs an ffmpeg spawn on a
			// machine that is already short: the frames are skipped to the resync, as they were before.
			// A stream a lag restart started a moment ago is not offered a restart for healing either: a stall that
			// comes with every start would be restarted over and over, paying itself again each time.
			const verdict = createFrameGate({
				fps: rung.fps,
				overloadAfter: cheaper ? undefined : Infinity,
				healWatch: heal ? undefined : Infinity,
				lead
			});
			const painter = CANVAS_PAINTER
				? createStreamPainter({
						decode: decodeJpeg,
						paint: (picture) => {
							if (!canvasEl) return;
							paintPicture(canvasEl, picture);
							frames.streamDrawn(picture.width / picture.height);
						}
					})
				: null;
			let restarted = '';
			// A stream that takes the pane over from the GPU is not `streaming` (the route is still the
			// surface's) until its first frame is shown.
			streaming = !takeover;
			const stop = startPlayback(
				from,
				request,
				(f) => {
					if (!live) return;
					// The audio clock owns time; picture chases it.
					const call = verdict(ui.time - offset - f.time, performance.now() / 1000);
					switch (call) {
						case 'resync':
						case 'degrade': {
							// Compositing can't keep up with real time on this timeline. Playing
							// the backlog out would run the picture in slow motion against the
							// sound, and dropping it forever would freeze the pane — so jump the
							// stream forward to where playback has actually got to, and when it
							// is slow rather than stalled, at a cost the machine can carry.
							live = false;
							restarted = call;
							ladder.restarted(call, verdict.floor());
							resyncs++;
							return;
						}
						case 'skip':
							// A frame the clock has moved past would drag the picture behind the
							// sound, so wait for one that still applies.
							return;
						case 'show':
							if (!taken) {
								// The GPU draws up to where the engine said it stops: the stream's frames before that
								// are not the pane's, and the GPU's last one stays until one at or past it is here.
								if (handover && f.time < handover.at - 0.5 / rung.fps) return;
								taken = true;
								streaming = true;
								const pb = gpuPlay;
								gpuPlay = null;
								pb?.end();
							}
							// Only a frame that is shown is turned into anything: the data URL is built here, not
							// before the gate has had its say.
							if (painter) painter.push(f.jpeg);
							else frames.showStreamed(jpegDataUrl(f.jpeg));
					}
				},
				reportPlaybackError
			);
			stopStream = stop;
			return () => {
				live = false;
				painter?.stop();
				stop();
				if (stopStream === stop) stopStream = null;
				streaming = false;
				// What the gate dropped is the one number that says whether this stream was smooth, so a stream
				// that dropped anything leaves a line in the log (a clean one would only be noise).
				ladder.ended(verdict.floor());
				const seen = verdict.stats();
				const painted = painter?.stats();
				if (seen.skipped > 0 || restarted || (painted && painted.failed + painted.discarded + painted.replaced > 0)) {
					logFrontend(
						'info',
						`playback stream ended: ${rung.fps} fps at ${rung.width} px, shown ${seen.shown}, skipped ${seen.skipped}` +
							`${restarted ? `, restarted (${restarted})` : ''}${painted ? `, painter ${JSON.stringify(painted)}` : ''}`,
						'playback'
					);
				}
			};
		};

		beginStream = (handover, takeover) => {
			if (!active || stopped) return;
			stopped = startStream(handover, takeover);
		};
		if (gpu) {
			// The pump takes it from here: its first request decides, and a `stream` answer comes back as `handOverToStream`.
			playFps = untrack(() => playbackFps(renderedFps(editor.timeline, editor.assets)));
			gpuPlay = new GpuPlayback(playFps, untrack(() => ui.time));
		} else {
			beginStream(null, false);
		}
		return () => {
			active = false;
			beginStream = null;
			const pb = gpuPlay;
			gpuPlay = null;
			pb?.end();
			stopped?.();
			stopped = null;
			if (pb && (pb.drawn > 0 || pb.repeated > 0) && !pb.handingOver) {
				logFrontend(
					'info',
					`GPU playback ended: ${pb.drawn} frames drawn, ${pb.repeated} repeated, round trip ${Math.round(pb.roundTrip * 1000)} ms`,
					'playback'
				);
			}
		};
	});

	// Keep the preview in step with the playhead *and* the edit state: re-render on every playhead
	// move, on every timeline change (an Inspector edit reassigns `editor.timeline`), when a proxy
	// becomes ready, on a change of route (the GPU takes a frame over, or hands it back for a title
	// box) and when the surface moved. Suspended while the stream is feeding frames, so the two
	// don't fight over the pane. What it depends on is what `run` reads, and nothing else.
	$effect(() => frames.run());

	// Read the bar's width when it mounts (again after the next frame, once layout has
	// settled), when it resizes and when the window does. A read that finds no width
	// (a hidden dock tab, a rebuild) leaves it unknown rather than zero.
	$effect(() => {
		const el = barEl;
		if (!el) return;
		// In whichever window the panel is: measured again when it moves to another.
		void windows.version;
		const win = windowOf(el);
		const read = () => (barWidth = measuredWidth(el.clientWidth));
		read();
		const settled = win.requestAnimationFrame(read);
		const observer = resizeObserverFor(el, read);
		observer.observe(el);
		win.addEventListener('resize', read);
		return () => {
			win.cancelAnimationFrame(settled);
			observer.disconnect();
			win.removeEventListener('resize', read);
		};
	});

	// The preview pane is the delivery frame, not a fixed 16:9 window: cutting a
	// Reel against a landscape box would hide the very crop that decides the shot.
	const delivery = $derived(editor.timeline.format ?? null);
	const aspect = $derived(delivery ? delivery.width / delivery.height : 16 / 9);
	// Which axis binds depends on the pane's shape as much as the frame's — a 1:1
	// frame is height-bound in a wide pane, and choosing by `aspect < 1` alone
	// squashed it to the pane's height at full width. `100cqh` reads the pane's
	// own height (it declares `container-type:size`), so one rule covers every
	// orientation: never wider than the pane, never taller than the pane. (A 720 px cap used to sit
	// here: 62 % of the pane at 1920×1080, a postage stamp at 2560×1440.)
	const frameBox = $derived(`width:min(100%, calc(100cqh * ${aspect}))`);

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
		if ((!frameUrl && !gpuShown && !streamShown) || !imgAspect) return 'inset:0';
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
	let rootEl = $state<HTMLElement | null>(null);
	/** The panel is in a window of its own. The surface and the bounds it is told are the editor
	 *  window's, so there the frame is the JPEG, as it is with the setting off. */
	const detached = $derived.by(() => {
		void windows.version;
		return rootEl ? windowOf(rootEl) !== window : false;
	});
	const route = $derived(
		routePreview({
			enabled: settings.gpuPreview,
			supported: gpuPreview.status?.supported ?? false,
			detached,
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

	let frameEl = $state<HTMLElement | null>(null);
	let matteEl = $state<HTMLElement | null>(null);
	let backdropEl = $state<HTMLElement | null>(null);
	let surroundEl = $state<HTMLElement | null>(null);

	// How wide a frame to ask for: the picture's box as it is drawn (CSS px, at this window's pixel
	// ratio — a detached window may be on another screen), in the steps of `preview-size.ts`. Null
	// until the box has been measured, so the pump asks nothing at a guess; a box with no width (the
	// panel is in a hidden tab) is not a measurement, and keeps the last one or waits. A change settles
	// first (`RESIZE_SETTLE_MS`): dragging a panel edge is a stream of sizes, and each width asked for
	// is an FFmpeg composite. A window moved to a monitor of another pixel ratio fires neither the
	// observer nor `resize`, so the ratio is watched through its own media query.
	let boxWidth = $state<number | null>(null);
	$effect(() => {
		const el = frameEl;
		if (!el) return;
		void windows.version;
		const win = windowOf(el);
		const measure = () => {
			if (el.clientWidth > 0) boxWidth = requestWidth(el.clientWidth, win.devicePixelRatio);
		};
		measure();
		let timer: ReturnType<typeof setTimeout> | undefined;
		const settle = () => {
			clearTimeout(timer);
			timer = setTimeout(measure, RESIZE_SETTLE_MS);
		};
		const observer = resizeObserverFor(el, settle);
		observer.observe(el);
		win.addEventListener('resize', settle);
		let ratio: MediaQueryList | null = null;
		const onRatio = () => {
			settle();
			watchRatio();
		};
		const watchRatio = () => {
			ratio?.removeEventListener('change', onRatio);
			ratio = win.matchMedia(dprQuery(win.devicePixelRatio));
			ratio.addEventListener('change', onRatio);
		};
		watchRatio();
		return () => {
			clearTimeout(timer);
			observer.disconnect();
			win.removeEventListener('resize', settle);
			ratio?.removeEventListener('change', onRatio);
		};
	});
	// What is asked is the box's width, held to the widest source the preview actually reads: the default
	// proxy is 1280 px wide, and a 1440 or 1920 px frame composited from it is the same picture upscaled, at
	// several times the decode cost on every scrub frame. With a delivery frame set the canvas is not the
	// source's shape (a 16:9 proxy under a 9:16 canvas is cropped and scaled by the fit), so the source's
	// width says nothing about the canvas's and the box's is asked as it is — the backend never goes past
	// the frame's own width.
	const sourceWidth = $derived(
		editor.timeline.format
			? null
			: previewSourceWidth(editor.timeline, editor.assets, mediaStatus.proxies, settings.previewSource, settings.proxySize, ui.time)
	);
	const frameWidth = $derived(boxWidth === null ? null : capToSource(boxWidth, sourceWidth));
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
		if (!settings.gpuPreview || !frameEl || detached) return;
		wantedBounds = measure(false);
		sendBounds.request();
	}

	/** Measure the frame and tell the backend (newest wins). */
	function pushBounds() {
		if (!settings.gpuPreview || !frameEl || detached) return;
		covered = frameCovered();
		wantedBounds = measure(route.via === 'gpu');
		sendBounds.request();
	}

	// The observers live as long as the frame does and the setting is on; what they report is read
	// when they fire, so a change of route or theme does not tear them down.
	$effect(() => {
		// In a window of its own there is no surface to place; the run before this one, if there
		// was one, reported the panel gone.
		if (!settings.gpuPreview || !frameEl || detached) return;
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

<div
	bind:this={rootEl}
	data-editor-surface
	{@attach onWindow({
		// Abandoning a drag is all Escape does then — not also "clear the selection". These
		// listen to the window the panel is in, which is a detached window's when it is in one.
		keydowncapture: (e: KeyboardEvent) => {
			if (e.key === 'Escape' && tdrag && !tdrag.committing) {
				cancelTitleDrag();
				e.stopPropagation();
			}
		},
		blur: cancelTitleDrag
	})}
	style="flex:1;min-height:0;display:flex;flex-direction:column;background:{onSurface && technique === 'window' ? 'transparent' : 'var(--surface-void)'}"
>
	<div
		role="presentation"
		oncontextmenu={onPreviewContextMenu}
		style="flex:1;min-height:0;display:grid;place-items:center;padding:20px;position:relative;container-type:size"
	>
		{#if onSurface && technique === 'window'}
			<!-- The pane's surround, with a hole where the frame is: the surface shows through
			     there. It covers the pane and nothing else: a positioned layer paints above the
			     unpositioned transport bar, so one spanning this whole root hid every control in
			     the bar but the scrub dot. -->
			<div bind:this={surroundEl} aria-hidden="true" style="position:absolute;inset:0;background:var(--surface-void);pointer-events:none"></div>
		{/if}
		{#if empty}
			<PreviewEmpty />
		{:else}
			<div
				bind:this={frameEl}
				data-gpu-frame={onSurface ? 'surface' : undefined}
				data-request-width={frameWidth ?? undefined}
				style="position:relative;aspect-ratio:{aspect};{frameBox};border-radius:4px;background:{gpuShown ? 'transparent' : 'radial-gradient(120% 120% at 30% 20%, var(--surface-active) 0%, var(--surface-raised) 55%, var(--surface-void) 100%)'};border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-md)"
			>
				{#if settings.gpuPreview}
					<!-- Colour probes: what the backend paints around the picture and behind the page. -->
					<div bind:this={matteEl} aria-hidden="true" style="position:absolute;width:0;height:0;background:var(--frame-matte)"></div>
					<div bind:this={backdropEl} aria-hidden="true" style="position:absolute;width:0;height:0;background:var(--surface-app)"></div>
				{/if}
				<!-- With `CANVAS_PAINTER` on: the playback stream's frames, drawn here as they are decoded.
				     Mounted from the start, so the first frame has somewhere to land; shown while it holds the
				     last one played, until a fetched frame (the settled still, the GPU's surface) takes the
				     pane back. Off, the stream goes through the `<img>` below, which is the pane's one picture
				     and so never swaps for the still. -->
				{#if CANVAS_PAINTER}
					<canvas
						bind:this={canvasEl}
						aria-label="playback frame"
						style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:var(--frame-matte);border-radius:3px;display:{streamShown && !gpuShown ? 'block' : 'none'}"
					></canvas>
				{/if}
				{#if gpuShown}
					<!-- Drawn by the GPU in the native surface: nothing here covers it. -->
				{:else if streamShown}
					<!-- The canvas above is the picture. -->
				{:else if frameUrl}
					<img src={frameUrl} decoding="async" alt="preview frame" style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:var(--frame-matte);border-radius:3px"
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
							{@const ink = inkBox(o)}
							<div
								role="presentation"
								data-title-box
								use:tip={selected ? undefined : `Select “${o.text}”`}
								onpointerdown={(e) => onTitleDown(e, o, 'move')}
								onpointermove={onTitleMove}
								onpointerup={onTitleUp}
								onpointercancel={cancelTitleDrag}
								onlostpointercapture={onTitleLostCapture}
								style="position:absolute;left:{(live?.x ?? pose.x) * 100}%;top:{(live?.y ?? pose.y) * 100}%;transform:translate(-50%,-50%);padding:{pad}cqh;font-size:{(live?.size ?? o.size) * 100}cqh;line-height:{ink.height};font-weight:{o.bold ? 700 : 400};font-family:{titleFamily(o)};white-space:nowrap;color:{live?.moved ? ghostColor(o.color) : 'transparent'};pointer-events:auto;touch-action:none;cursor:{live ? 'grabbing' : 'move'};user-select:none;outline:{selected ? '1.5px solid var(--kerf-400)' : '1px dashed color-mix(in srgb,var(--text-on-video) 35%,transparent)'};outline-offset:0;background:{live?.moved ? 'color-mix(in srgb,var(--scrim) 25%,transparent)' : 'transparent'}"
							>
								<span style="position:relative;top:{ink.shift}em">{o.text}</span>
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
				<!-- The HUD: what is on screen and where it is decoded from. Every chip is on a scrim with the
				     on-video text colour, never bare text on the picture (a snow scene or the Light theme's pale
				     placeholder swallowed it), and the timecode is the transport's, not a second copy here. -->
				<div
					style="position:absolute;left:12px;right:12px;top:10px;display:flex;align-items:flex-start;justify-content:space-between;gap:8px;pointer-events:none"
				>
					<div style="display:flex;flex-wrap:wrap;gap:6px;min-width:0;max-width:62%">
						<span data-hud="name" class="hud" style="pointer-events:auto" use:tip={previewAsset?.name}>{previewAsset?.name ?? 'preview'}</span>
						{#if playbackWaiting}<span class="hud" style="pointer-events:auto" use:tip={playbackWaiting} data-testid="playback-waiting"
								><i class="dot" style="background:var(--orange-500)"></i>playback waits for the proxy</span
							>{/if}
						{#if waitingOnProxy}<span class="hud" style="pointer-events:auto" use:tip={waitingOnProxy.title}
								><i class="dot" style="background:var(--orange-500)"></i>{waitingOnProxy.text.replace('preview: ', '')}</span
							>{/if}
						{#if ui.analyzing}<span class="hud"><i class="dot" style="background:var(--agent-400)"></i>{ui.analysisLabel ?? 'analyzing'}</span>{/if}
					</div>
					<span data-hud="format" class="hud" style="flex:none;font-family:var(--font-mono);font-weight:500"
						>{resolution}{fpsLabel ? ` · ${fpsLabel}` : ''}</span
					>
				</div>
			</div>
		{/if}
	</div>
	<!-- Transport: go to start, play / pause, go to end, the timecode with the
	     timeline's rate, and the scrub bar. J / K / L stay on the keyboard. It narrows
	     gracefully: below ~380 px of a measured width the duration and the rate go and
	     the gaps close — the controls themselves always stay (`transport-bar.ts`). -->
	<div
		bind:this={barEl}
		style="height:40px;flex:none;display:flex;align-items:center;gap:{parts.tight ? 6 : 10}px;padding:0 {parts.tight ? 8 : 14}px;border-top:var(--line-width) solid var(--border-default);background:var(--surface-app)"
	>
		<button
			use:tip={settings.tip('Go to start', 'playback.toStart')}
			aria-label="Go to start"
			disabled={empty}
			onclick={() => ui.seek(0)}
			style="background:none;border:none;cursor:{empty ? 'default' : 'pointer'};color:var(--text-secondary);opacity:{empty ? 0.4 : 1};display:grid;place-items:center;padding:3px"
		>
			<Icon n="skip-back" s={14} />
		</button>
		<button
			use:tip={settings.tip(ui.playing ? 'Pause' : 'Play', 'playback.toggle')}
			aria-label={ui.playing ? 'Pause' : 'Play'}
			onclick={() => ui.togglePlay()}
			style="background:var(--surface-hover);border:none;border-radius:var(--radius-sm);cursor:pointer;color:var(--text-primary);display:grid;place-items:center;padding:4px 6px"
		>
			<Icon n={ui.playing ? 'pause' : 'play'} s={16} />
		</button>
		<button
			use:tip={settings.tip('Go to end', 'playback.toEnd')}
			aria-label="Go to end"
			disabled={empty}
			onclick={() => ui.seek(editor.duration)}
			style="background:none;border:none;cursor:{empty ? 'default' : 'pointer'};color:var(--text-secondary);opacity:{empty ? 0.4 : 1};display:grid;place-items:center;padding:3px"
		>
			<Icon n="skip-forward" s={14} />
		</button>
		<span
			use:tip={`Timeline timecode · ${editor.fps.toFixed(3)} fps · non-drop`}
			style="font-family:var(--font-mono);font-size:12px;color:var(--kerf-300);font-weight:500;white-space:nowrap">{tc(ui.time)}</span
		>
		<ScrubBar {duration} {empty} />
		{#if parts.duration}
			<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap">{tc(duration)}</span>
		{/if}
		{#if parts.rate}
			<span
				use:tip={'Timeline frame rate; non-drop timecode'}
				style="font-size:11px;color:var(--text-muted);white-space:nowrap">{Number(editor.fps.toFixed(3))} fps</span
			>
		{/if}
	</div>
</div>

<style>
	/* A HUD chip: on the picture, so on a scrim with the on-video text — legible over a snow field and
	   over the Light theme's pale placeholder alike. */
	.hud {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		min-width: 0;
		max-width: 100%;
		height: 19px;
		padding: 0 7px;
		box-sizing: border-box;
		border-radius: var(--radius-sm);
		font-size: 10px;
		font-weight: 600;
		line-height: 1;
		letter-spacing: 0.02em;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		color: var(--text-on-video);
		background: color-mix(in srgb, var(--scrim) 60%, transparent);
	}
	.hud .dot {
		flex: none;
		width: 5px;
		height: 5px;
		border-radius: 50%;
	}
</style>
