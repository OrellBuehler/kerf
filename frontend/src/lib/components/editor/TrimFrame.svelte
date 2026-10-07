<script lang="ts">
	/* One frame of the trim monitor: the picture at a source time, with what it is
	 * (an `Out` or an `In`, which clip, which timecode) underneath. It asks the
	 * backend for the frame of the *source* — the footage is what a roll, slip or
	 * slide moves, so the composite (effects, other tracks) would show the wrong
	 * thing — and keeps showing the last frame it has while the next is decoded:
	 * a drag changes the time every few milliseconds, and one decode is in flight
	 * at a time (`singleFlight`), reading the newest time when it starts, so the
	 * frame on screen trails the pointer by one decode rather than by a backlog.
	 * Outside the desktop app there is no decoder (`getFrame` answers `null`), so the
	 * harness draws the same stand-in frame its playback does, stamped with the source
	 * time — which makes the monitor explorable under `bun run dev`. */
	import Icon from './Icon.svelte';
	import { getFrame, inTauri } from '$lib/api';
	import { sampleFrameUrl } from '$lib/sample-frame';
	import { editor } from '$lib/state.svelte';
	import { singleFlight } from '$lib/single-flight';
	import type { MonitorCell } from '$lib/trim-tools';

	let { cell }: { cell: MonitorCell } = $props();

	/** Wide enough to read a face, small enough that a decode is quick. */
	const FRAME_WIDTH = 480;

	let url = $state<string | null>(null);
	let wanted = { assetId: '', time: 0 };

	const flight = singleFlight(async () => {
		const mine = wanted;
		try {
			// A keyframe-snapped frame, as scrubbing takes: this is a glance, not a grade.
			const got = await getFrame(mine.assetId, mine.time, FRAME_WIDTH, false);
			if (got) url = got;
		} catch {
			// A frame that would not decode leaves the last good one up.
		}
	});

	$effect(() => {
		if (!inTauri()) {
			url = sampleFrameUrl(cell.time);
			return;
		}
		wanted = { assetId: cell.assetId, time: cell.time };
		flight.request();
	});
</script>

<div style="flex:1;min-width:0;min-height:0;display:flex;flex-direction:column;gap:5px">
	<div
		style="flex:1;min-height:0;position:relative;display:grid;place-items:center;background:var(--frame-matte);border:var(--line-width) solid var(--border-default);border-radius:3px;overflow:hidden"
	>
		{#if url}
			<img src={url} alt="{cell.label} frame" style="position:absolute;inset:0;width:100%;height:100%;object-fit:contain" />
		{:else}
			<Icon n="film" s={26} color="color-mix(in srgb,var(--text-on-video) 22%,transparent)" />
		{/if}
	</div>
	<div style="display:flex;align-items:baseline;gap:6px;min-width:0;font-size:11px">
		<span
			style="flex:none;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:{cell.label ===
			'Out'
				? 'var(--diff-remove)'
				: 'var(--diff-add)'}">{cell.label}</span
		>
		<span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:var(--text-secondary)"
			>{editor.assetName(cell.assetId)}</span
		>
		<span style="flex:none;font-family:var(--font-mono);color:var(--kerf-300)" title="Source timecode">{cell.timecode}</span>
	</div>
</div>
