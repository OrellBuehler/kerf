<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import type { Clip, Track } from '$lib/types';
	import { clipDuration } from '$lib/types';

	const pxPerSec = $derived(ui.zoom);
	const duration = $derived(Math.max(editor.duration, 8));
	const contentW = $derived(Math.max(760, Math.ceil(duration * pxPerSec) + 48));
	const tickStep = $derived(pxPerSec >= 60 ? 2 : pxPerSec >= 28 ? 5 : 10);
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

	const sceneXs = $derived.by(() => {
		const xs: number[] = [];
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'video') continue;
			for (const c of t.clips) {
				const an = editor.analysisFor(c.asset_id);
				if (!an) continue;
				for (const sc of an.scene_changes) {
					if (sc > c.source_in && sc < c.source_out) {
						xs.push((c.timeline_start + (sc - c.source_in)) * pxPerSec);
					}
				}
			}
		}
		return xs;
	});

	function silenceRegions(c: Clip): { left: number; width: number }[] {
		const an = editor.analysisFor(c.asset_id);
		if (!an) return [];
		return an.silence_segments
			.filter((s) => s.end > c.source_in && s.start < c.source_out)
			.map((s) => {
				const a = Math.max(s.start, c.source_in);
				const b = Math.min(s.end, c.source_out);
				return { left: (c.timeline_start + (a - c.source_in)) * pxPerSec, width: (b - a) * pxPerSec };
			});
	}

	// ---- waveforms -----------------------------------------------------------

	const WAVE_BUCKETS = 240;
	let waveforms = $state<Record<string, number[]>>({});

	$effect(() => {
		const wanted = new Set<string>();
		for (const t of editor.timeline.tracks) {
			if (t.kind !== 'audio') continue;
			for (const c of t.clips) wanted.add(c.asset_id);
		}
		for (const assetId of wanted) {
			if (waveforms[assetId]) continue;
			waveforms[assetId] = [];
			editor.waveform(assetId, WAVE_BUCKETS).then((peaks) => {
				waveforms = { ...waveforms, [assetId]: peaks };
			});
		}
	});

	function clipPeaks(c: Clip): number[] {
		const all = waveforms[c.asset_id];
		const dur = editor.assets.find((a) => a.id === c.asset_id)?.duration ?? 0;
		if (!all || all.length === 0 || dur <= 0) return [];
		const lo = Math.floor((c.source_in / dur) * all.length);
		const hi = Math.max(lo + 1, Math.ceil((c.source_out / dur) * all.length));
		return all.slice(lo, Math.min(hi, all.length));
	}

	// ---- interaction ---------------------------------------------------------

	function onClipClick(e: MouseEvent, c: Clip, left: number) {
		e.stopPropagation();
		if (ui.tool === 'razor') {
			const x = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
			const at = c.timeline_start + Math.max(0.05, x / pxPerSec);
			void editor.split(c.id, at);
			return;
		}
		editor.selectedClipId = c.id;
		void editor.select(c.asset_id);
	}

	function onLaneSeek(e: MouseEvent) {
		const x = e.clientX - (e.currentTarget as HTMLElement).getBoundingClientRect().left;
		ui.seek(x / pxPerSec);
	}
</script>

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
		<div style="flex:1"></div>
		<button
			title="Zoom out"
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
			onclick={() => (ui.zoom = Math.min(96, ui.zoom + 8))}
			style="background:none;border:none;cursor:pointer;color:var(--text-muted);display:grid;place-items:center"
			><Icon n="zoom-in" s={14} /></button
		>
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)"
			>{ui.snap ? 'snap on' : 'snap off'}</span
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
					style="height:{trackHeight(t)};border-bottom:1px solid var(--border-subtle);display:flex;align-items:center;gap:8px;padding:0 10px"
				>
					<span
						style="font-family:var(--font-mono);font-size:11px;font-weight:600;color:var(--text-secondary);width:20px"
						>{t.name}</span
					>
					<span style="font-size:11px;color:var(--text-muted);flex:1">{t.kind === 'video' ? 'Video' : 'Audio'}</span>
					<Icon n={t.kind === 'video' ? 'eye' : 'volume-2'} s={12} color="var(--text-disabled)" />
				</div>
			{/each}
		</div>

		<!-- scrollable track area -->
		<div style="flex:1;overflow-x:auto;overflow-y:hidden;position:relative">
			<div style="width:{contentW}px;position:relative">
				<!-- ruler -->
				<div
					role="presentation"
					onclick={onLaneSeek}
					style="height:var(--ruler-h);border-bottom:1px solid var(--border-subtle);position:relative;background:var(--surface-app);cursor:text"
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
				</div>

				<!-- grid lines -->
				{#each ticks as t, i (t)}
					<span
						style="position:absolute;left:{t * pxPerSec}px;top:var(--ruler-h);bottom:0;width:1px;background:{i % 2 ? 'var(--timeline-grid)' : 'var(--timeline-grid-major)'}"
					></span>
				{/each}

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
						onclick={onLaneSeek}
						style="height:{trackHeight(t)};border-bottom:1px solid var(--border-subtle);position:relative"
					>
						{#each t.clips as c (c.id)}
							{@const left = c.timeline_start * pxPerSec}
							{@const width = Math.max(6, clipDuration(c) * pxPerSec)}
							{@const selected = editor.selectedClipId === c.id}
							<button
								onclick={(e) => onClipClick(e, c, left)}
								style="position:absolute;left:{left}px;top:5px;height:calc(100% - 10px);width:{width}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;padding:0 7px;cursor:{ui.tool === 'razor' ? 'crosshair' : 'pointer'};text-align:left;background:{t.kind === 'audio' ? 'var(--track-audio)' : 'var(--track-video)'};border:{selected ? '1.5px solid var(--kerf-400)' : `1px solid ${t.kind === 'audio' ? 'var(--track-audio-edge)' : 'var(--track-video-edge)'}`};box-shadow:{selected ? '0 0 0 1px var(--kerf-500)' : 'none'}"
							>
								{#if t.kind === 'audio'}
									{@const peaks = clipPeaks(c)}
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
								<span
									style="position:relative;font-size:10px;font-weight:600;color:rgba(255,255,255,.92);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
									>{editor.assetName(c.asset_id)}</span
								>
							</button>
						{/each}
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
