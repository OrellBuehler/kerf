<script lang="ts">
	// A stereo level meter drawn from `meter.ts`'s state: per channel a dim bar for the
	// peak, a bright one for the RMS, and a hold marker. Its scale is the fader's own taper
	// (`dbToFader`), so a mark on the scale beside it is at the height the fader puts
	// the same level — which is what lets one scale serve both.
	//
	// It only draws. The levels are the analysers' (`audio.meters()`), stepped by the
	// panel's frame loop.
	import { dbToFader, faderTicks, labelTicks } from '$lib/mixer';
	import { METER_FLOOR_DB, type StereoMeter } from '$lib/meter';

	let {
		meter,
		maxDb,
		scale = true,
		label
	}: {
		meter: StereoMeter;
		/** The top of the scale, dB — the fader beside it ends here. */
		maxDb: number;
		/** Print the dB marks beside the bars. */
		scale?: boolean;
		label: string;
	} = $props();

	let height = $state(0);

	/** A level as a place on the bars, `0..1`; the floor is the bottom. */
	const at = (db: number) => (db <= METER_FLOOR_DB ? 0 : dbToFader(db, maxDb));
	/** The bars' travel is the box less the padding the fader's rail is inset by. */
	const travel = $derived(Math.max(0, height - 18));
	const marks = $derived(labelTicks(faderTicks(maxDb), travel, 12));
	const ticks = $derived(faderTicks(maxDb));
	/** The colour zones, where the fader's scale puts them: green to -18 dB, amber
	 *  from -8 to -4, red from -1. */
	const zones = $derived.by(() => {
		const stop = (db: number) => `${(at(db) * 100).toFixed(1)}%`;
		return `linear-gradient(to top, var(--green-500) 0%, var(--green-500) ${stop(-18)}, var(--orange-500) ${stop(-8)}, var(--orange-500) ${stop(-4)}, var(--red-500) ${stop(-1)})`;
	});

	/** A bar clipped to `db`: the whole gradient, cut off from the top. */
	const cut = (db: number) => `clip-path:inset(${((1 - at(db)) * 100).toFixed(2)}% 0 0 0)`;
</script>

<div class="meter" role="img" aria-label={label} bind:clientHeight={height} style="--zones:{zones}">
	{#if scale}
		<div class="scale" aria-hidden="true">
			{#each marks as t (t.db)}
				<span class="mark" style="bottom:{(t.pos * 100).toFixed(2)}%">{t.db === 0 ? '0' : t.db > 0 ? `+${t.db}` : `−${-t.db}`}</span>
			{/each}
		</div>
	{/if}
	<div class="channels">
		{#each [meter.l, meter.r] as ch, i (i)}
			<div class="bar">
				{#each ticks as t (t.db)}
					<div class="grid" style="bottom:{(t.pos * 100).toFixed(2)}%"></div>
				{/each}
				<div class="level peak" style={cut(ch.peak)}></div>
				<div class="level rms" style={cut(ch.rms)}></div>
				{#if ch.hold > METER_FLOOR_DB}
					<div class="hold" class:hot={ch.hold >= -0.1} style="bottom:{(at(ch.hold) * 100).toFixed(2)}%"></div>
				{/if}
			</div>
		{/each}
	</div>
</div>

<style>
	.meter {
		position: relative;
		flex: none;
		display: flex;
		gap: 3px;
		align-self: stretch;
		min-height: 90px;
		/* The same inset as the fader's rail, so the two share one scale. */
		padding: 9px 0;
		box-sizing: border-box;
	}
	.scale {
		position: relative;
		width: 20px;
		font-family: var(--font-mono);
		font-weight: 500;
		color: var(--text-disabled);
	}
	.mark {
		position: absolute;
		right: 0;
		transform: translateY(50%);
		line-height: 1;
		font-size: 9px;
		white-space: nowrap;
	}
	.channels {
		display: flex;
		gap: 2px;
	}
	.bar {
		position: relative;
		width: 7px;
		overflow: hidden;
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-xs);
		box-sizing: border-box;
	}
	.grid {
		position: absolute;
		left: 0;
		right: 0;
		height: var(--line-width);
		background: var(--border-subtle);
	}
	/* Green, through amber, to red in the last few dB — where the scale puts them. */
	.level {
		position: absolute;
		inset: 0;
		background: var(--zones);
	}
	.peak {
		opacity: 0.4;
	}
	.hold {
		position: absolute;
		left: 0;
		right: 0;
		height: var(--line-emphasis);
		background: var(--text-primary);
		transform: translateY(50%);
	}
	.hold.hot {
		background: var(--red-400);
	}
</style>
