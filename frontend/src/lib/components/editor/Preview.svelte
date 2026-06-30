<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { getTimelineFrame } from '$lib/api';
	import { clipDuration } from '$lib/types';

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

	const resolution = $derived.by(() => {
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.width && v?.height ? `${v.width}×${v.height}` : '—';
	});
	const fpsLabel = $derived.by(() => {
		const v = previewAsset?.streams.find((s) => s.kind === 'video');
		return v?.fps ? v.fps.toFixed(3) : '';
	});

	function tc(s: number): string {
		const total = Math.max(0, s);
		const m = Math.floor(total / 60);
		const sec = Math.floor(total % 60);
		const frames = Math.floor((total % 1) * 24);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}:${frames.toString().padStart(2, '0')}`;
	}

	let frameUrl = $state<string | null>(null);
	let inFlight = false;
	let queued: number | null = null; // latest wanted timeline time, or null to clear

	// Single-flight decode: only ever one composite in flight, and `queued` always
	// holds the *latest* wanted timeline time. Scrubbing/playback collapses to one
	// render + one pending target instead of a backlog of stale frames that must
	// all drain before the frame under the cursor appears (the cause of lag).
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
			if (url) frameUrl = url;
		} catch {
			/* ignore decode errors — keep the last good frame */
		}
		inFlight = false;
		if (queued !== null) pump(); // a newer target arrived mid-decode — go to it
	}

	// Keep the preview in step with the playhead *and* the edit state: re-render on
	// every playhead move, on every timeline change (an Inspector edit reassigns
	// `editor.timeline`), and when a proxy becomes ready.
	$effect(() => {
		const t = ui.time;
		void editor.timeline;
		void ui.previewEpoch;
		if (!hasClips) {
			frameUrl = null;
			queued = null;
			return;
		}
		queued = t;
		pump();
	});

	function scrub(e: MouseEvent) {
		const el = e.currentTarget as HTMLElement;
		const x = e.clientX - el.getBoundingClientRect().left;
		ui.seek((x / el.clientWidth) * duration);
	}
</script>

<div style="flex:1;min-height:0;display:flex;flex-direction:column;background:var(--surface-void)">
	<div style="flex:1;min-height:0;display:grid;place-items:center;padding:20px;position:relative">
		{#if empty}
			<div style="display:flex;flex-direction:column;align-items:center;gap:12px;color:var(--text-disabled)">
				<Icon n="clapperboard" s={30} /><span style="font-size:13px">No media loaded</span>
			</div>
		{:else}
			<div
				style="position:relative;aspect-ratio:16/9;max-height:100%;max-width:100%;width:min(100%, 720px);border-radius:4px;overflow:hidden;background:radial-gradient(120% 120% at 30% 20%, #2b3a49 0%, #161d24 55%, #0d1116 100%);border:1px solid var(--border-default);box-shadow:var(--shadow-md)"
			>
				{#if frameUrl}
					<img src={frameUrl} alt="preview frame" style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain;background:#000" />
				{:else}
					<div style="position:absolute;inset:0;background:linear-gradient(115deg, transparent 40%, rgba(226,157,46,.06) 60%)"></div>
					<div style="position:absolute;inset:0;display:grid;place-items:center;color:rgba(255,255,255,.22)">
						<Icon n={ui.playing ? 'pause' : 'play'} s={44} />
					</div>
				{/if}
				<div style="position:absolute;left:14px;top:12px;display:flex;gap:6px">
					<Badge tone="kerf">{previewAsset?.name ?? 'preview'}</Badge>
					{#if ui.analyzing}<Badge tone="agent" dot>analyzing</Badge>{/if}
				</div>
				<div
					style="position:absolute;right:14px;top:12px;font-family:var(--font-mono);font-size:11px;color:rgba(255,255,255,.55)"
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
		style="height:40px;flex:none;display:flex;align-items:center;gap:12px;padding:0 16px;border-top:1px solid var(--border-default);background:var(--surface-app)"
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
