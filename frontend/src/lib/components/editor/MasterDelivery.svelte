<script lang="ts">
	// The cut's measured loudness and true peak against what a delivery is held to, in a popover beside the master strip
	// (`MixerPopover.svelte`): each line is what it reads, how far that is from the line, the line itself, and — when it is off — what to do,
	// naming the control. When the fix is in the master FX the popover says so and opens them (`onfx`).
	//
	// Nothing in it is a meter the engine does not feed: it is the numbers `get_levels` returned, worded by `deliveryCheck`
	// (`levels-view.ts`) from the engine's own thresholds. It says when they are an estimate (the browser harness) or of a cut that has
	// changed since.
	import MixerPopover from './MixerPopover.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import { LEVELS_TARGET_LUFS } from '#lib/levels';
	import { noteTone, type DeliveryRow, type DeliveryTarget, type Tone } from '#lib/levels-view';

	let {
		rows,
		target,
		scope,
		stale,
		estimated,
		verdict,
		notes,
		anchor,
		onfx,
		onclose
	}: {
		rows: DeliveryRow[];
		/** The loudness it is judged against, and where that came from. */
		target: DeliveryTarget;
		/** What was measured: "Whole cut · 2:00", or the in / out range. */
		scope: string;
		stale: boolean;
		estimated: boolean;
		/** The engine's one-line verdict on the mix, and every line of advice it wrote — the numbers above, in its own words. */
		verdict: { text: string; tone: Tone } | null;
		notes: string[];
		anchor: HTMLElement;
		/** Open the master FX, where the limiter, the ceiling and true peak are. */
		onfx: () => void;
		onclose: () => void;
	} = $props();

	/** The fix is in the FX when the advice names them. */
	const inFx = $derived(rows.some((r) => r.advice && /\(FX\)|Limiter|True peak|ceiling/.test(r.advice)));
</script>

<MixerPopover label="Delivery" name={stale ? 'Delivery, out of date: the cut changed after this was measured' : 'Delivery'} {anchor} {onclose}>
	<p class="against">Against {target.lufs} LUFS, {target.name}, and −1 dBTP.</p>
	<p class="scope" class:stale>
		{stale ? 'Out of date — the cut changed after this was measured. ' : ''}{scope}
		{#if estimated}<Badge tone="warning" style="height:16px;padding:0 5px;font-size:var(--text-2xs)">estimated</Badge>{/if}
	</p>
	<dl class:stale aria-label="The cut against its delivery">
		{#each rows as row (row.id)}
			<div class="row {row.tone}">
				<dt>{row.label}</dt>
				<dd class="value">{row.value}</dd>
				<dd class="vs"><span class="dot" aria-hidden="true"></span>{row.delta}<span class="line"> · {row.target}</span></dd>
				{#if row.advice}<dd class="advice">{row.advice}</dd>{/if}
			</div>
		{/each}
	</dl>
	{#if inFx}
		<Btn size="sm" onclick={onfx}>Open the master FX</Btn>
	{/if}
	{#if notes.length > 0}
		<details class="notes">
			<summary>{verdict ? verdict.text : 'What the engine says'} <span class="sub">(the engine judges by {LEVELS_TARGET_LUFS} LUFS)</span></summary>
			<ul>
				{#each notes as note (note)}
					<li class={noteTone(note)}>{note}</li>
				{/each}
			</ul>
		</details>
	{/if}
</MixerPopover>

<style>
	.against {
		margin: 0;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-secondary);
	}
	.sub {
		color: var(--text-muted);
	}
	.scope {
		margin: 0;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 2px 6px;
		font-size: var(--text-2xs);
		line-height: 1.35;
		color: var(--text-muted);
	}
	.scope.stale {
		color: var(--orange-400);
	}
	dl {
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	dl.stale {
		opacity: 0.55;
	}
	.row {
		display: grid;
		grid-template-columns: 1fr auto;
		gap: 2px 8px;
		align-items: baseline;
	}
	dt {
		font: var(--type-overline);
		letter-spacing: var(--tracking-wide);
		text-transform: uppercase;
		color: var(--text-muted);
	}
	dd {
		margin: 0;
	}
	.value {
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-primary);
	}
	.vs {
		grid-column: 1 / -1;
		display: flex;
		align-items: center;
		gap: 5px;
		font-size: var(--text-xs);
		color: var(--text-secondary);
	}
	.line {
		color: var(--text-muted);
	}
	.dot {
		flex: none;
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--success);
	}
	.row.warn .dot {
		background: var(--warning);
	}
	.row.warn .vs {
		color: var(--text-primary);
	}
	.notes {
		font-size: var(--text-xs);
		color: var(--text-secondary);
	}
	.notes summary {
		cursor: pointer;
		line-height: 1.4;
	}
	.notes ul {
		margin: 6px 0 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 5px;
	}
	.notes li {
		position: relative;
		padding-left: 12px;
		line-height: 1.4;
	}
	.notes li::before {
		content: '';
		position: absolute;
		left: 0;
		top: 0.55em;
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--text-muted);
	}
	.notes li.ok::before {
		background: var(--success);
	}
	.notes li.warn::before {
		background: var(--warning);
	}
	.advice {
		grid-column: 1 / -1;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-secondary);
	}
</style>
