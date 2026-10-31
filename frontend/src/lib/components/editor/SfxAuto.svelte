<script lang="ts">
	// Auto sound design: ask the engine which moments of the cut want a sound (its cuts, transitions, titles, punch-ins, drops
	// in the music and markers named `sfx`), look through them, tick the ones to keep and add them all as one edit. Finding is a
	// read; nothing is placed until Add. The list belongs to the cut it was found on — once the cut changes it says so and
	// asks to be found again rather than place effects at times that have moved.
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import EmptyState from './EmptyState.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import SectionHead from './SectionHead.svelte';
	import { tick } from 'svelte';
	import { editor } from '#lib/state.svelte';
	import { sfxPanel } from '#lib/sfx-panel.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { CATEGORY_OPTIONS, DENSITY_OPTIONS, addLabel, addedLine, glyphOf, hueOf, nothingFoundLine, pickLine, suggestionRows, undoLine } from '#lib/sfx-panel';
	import { tip } from '#lib/tip';

	const rows = $derived(sfxPanel.found ? suggestionRows(sfxPanel.found) : []);
	const picked = $derived(sfxPanel.pickedCount);
	const empty = $derived(editor.duration <= 0);
	const stale = $derived(sfxPanel.stale);
	const blocked = $derived(sfxPanel.blocked);
	/** Why Add is off, in a line (also its tooltip); null when it can go. */
	const why = $derived(blocked ?? (stale ? 'The cut changed since — find the moments again' : picked === 0 ? 'Tick at least one moment' : null));
	/** Why Find moments is off, or null: a proposal being previewed is not the cut the moments would be read from. */
	const findWhy = $derived(editor.previewingStaged ? 'Exit the proposal preview first — moments are found on your cut' : empty ? 'Add clips to the timeline first' : null);

	let section = $state<HTMLElement | null>(null);

	/** Add what is ticked, and — the list and its button being gone — leave the keyboard on Find moments rather than nowhere. */
	async function addPicked() {
		if (!(await sfxPanel.addPicked())) return;
		await tick();
		section?.querySelector<HTMLElement>('[data-testid="sfx-find"]')?.focus();
	}

	const gain = (db: number) => `${db > 0 ? '+' : ''}${db} dB`;
</script>

<section bind:this={section} aria-label="Auto sound design" data-testid="sfx-auto">
	<SectionHead label="Auto sound design" />
	<p class="lede">
		Kerf looks through the cut for moments that want a sound — cuts, titles, punch-ins, drops in the music, markers named
		<code>sfx</code> — suggests one for each, and adds the ones you keep as a single edit.
	</p>

	<div class="field">
		<span class="lbl">Density</span>
		<ChoiceChips label="Density" options={DENSITY_OPTIONS} value={sfxPanel.density} onpick={(v) => (sfxPanel.density = v)} testid="sfx-density" />
	</div>
	<div class="field">
		<span class="lbl">Include</span>
		<ChoiceChips label="Kinds of effect to include" options={CATEGORY_OPTIONS} value={sfxPanel.autoCategory} onpick={(v) => (sfxPanel.autoCategory = v)} testid="sfx-auto-category" />
	</div>

	<div style="margin-top:10px">
		<Btn
			variant="secondary"
			icon="wand-sparkles"
			pending={sfxPanel.finding || findWhy !== null}
			title={findWhy ?? (sfxPanel.found ? 'Look through the cut again' : 'Look through the cut for moments that want a sound')}
			data-testid="sfx-find"
			onclick={() => void sfxPanel.find()}>{sfxPanel.finding ? 'Looking…' : sfxPanel.found ? 'Find again' : 'Find moments'}</Btn
		>
	</div>

	{#if sfxPanel.finding}
		<div class="working" role="status">
			<ProgressBar label="Finding moments for sound effects" />
			<span>Looking through the cut…</span>
		</div>
	{:else if sfxPanel.findError}
		<div data-testid="sfx-find-error">
			<EmptyState icon="alert-triangle" title="Couldn't find moments" hint={sfxPanel.findError} pad={18}>
				<Btn size="sm" onclick={() => void sfxPanel.find()}>Try again</Btn>
			</EmptyState>
		</div>
	{:else if sfxPanel.found}
		{#if rows.length === 0}
			<div data-testid="sfx-none">
				<EmptyState icon="wand-sparkles" title="Nothing to add" hint={nothingFoundLine(sfxPanel.density, sfxPanel.autoCategory)} pad={18} />
			</div>
		{:else}
			<div class="bar">
				<span class="count" data-testid="sfx-count" aria-live="polite">{pickLine(picked, rows.length)}</span>
				{#if stale}<Badge tone="warning">out of date</Badge>{/if}
				<span style="flex:1"></span>
				<Btn size="sm" variant="ghost" pending={picked === rows.length} onclick={() => sfxPanel.pickAll()}>All</Btn>
				<Btn size="sm" variant="ghost" pending={picked === 0} onclick={() => sfxPanel.pickNone()}>None</Btn>
			</div>
			<div class="add">
				<Btn
					variant="primary"
					icon="plus"
					pending={why !== null || editor.locked}
					title={why ?? 'Add the ticked effects to the timeline as one edit'}
					data-testid="sfx-add-picked"
					onclick={() => void addPicked()}>{addLabel(picked)}</Btn
				>
			</div>
			<ul class="list" class:stale aria-label="Suggested sound effects" data-testid="sfx-suggestions">
				{#each rows as r (r.index)}
					<li class="item">
						<label class="k-check pick">
							<input
								type="checkbox"
								checked={sfxPanel.picks.has(r.index)}
								aria-label="{r.label} at {timeDisplay.fmt(r.at)}"
								onchange={() => sfxPanel.toggle(r.index)}
							/>
							<span class="glyph" style="color:{hueOf(r.category)}"><Icon n={glyphOf(r.kind)} s={14} /></span>
							<span class="what">
								<span class="head">
									<span class="time">{timeDisplay.fmt(r.at)}</span>
									<span class="kind">{r.label}</span>
									<span class="src">{r.source}</span>
									{#if r.gainDb !== null}<span class="db" use:tip={'Gain the effect will have: lower over speech'}>{gain(r.gainDb)}</span>{/if}
								</span>
								<span class="reason" use:tip={r.reason}>{r.reason}</span>
							</span>
						</label>
						<IconBtn size={28} data-tone="quiet" title="Move the playhead to {timeDisplay.fmt(r.at)}" onclick={() => sfxPanel.jump(r.at)}>
							<Icon n="crosshair" s={14} />
						</IconBtn>
					</li>
				{/each}
			</ul>
		{/if}
	{:else if sfxPanel.lastAdded !== null}
		<p class="done" role="status" data-testid="sfx-added">
			<Icon n="circle-check" s={14} color="var(--success)" />{addedLine(sfxPanel.lastAdded)}. {undoLine(sfxPanel.lastAdded)}
		</p>
	{/if}
</section>

<style>
	.lede {
		margin: 0 0 10px;
		font-size: var(--text-sm);
		line-height: 1.5;
		color: var(--text-muted);
	}
	.lede code {
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-secondary);
	}
	.field {
		display: flex;
		flex-direction: column;
		gap: 5px;
		margin-top: 8px;
	}
	.lbl {
		font: var(--type-label);
		color: var(--text-secondary);
	}
	.working {
		display: flex;
		flex-direction: column;
		gap: 7px;
		margin-top: 14px;
		font-size: var(--text-sm);
		color: var(--text-muted);
	}
	.bar {
		display: flex;
		align-items: center;
		gap: 6px;
		margin: 14px 0 4px;
	}
	.count {
		font-size: var(--text-sm);
		color: var(--text-secondary);
	}
	.list {
		margin: 0;
		padding: 0;
		list-style: none;
		border-top: var(--line-width) solid var(--border-subtle);
	}
	.list.stale {
		opacity: 0.55;
	}
	.item {
		display: flex;
		align-items: flex-start;
		gap: 2px;
		border-bottom: var(--line-width) solid var(--border-subtle);
	}
	.pick {
		flex: 1;
		min-width: 0;
		align-items: flex-start;
		gap: 8px;
		padding: 7px 0;
	}
	.pick input {
		margin-top: 1px;
	}
	.glyph {
		display: inline-flex;
		margin-top: 0;
	}
	.what {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.head {
		display: flex;
		align-items: baseline;
		gap: 7px;
		min-width: 0;
	}
	.time {
		font-family: var(--font-mono);
		font-size: var(--text-sm);
		color: var(--text-primary);
		white-space: nowrap;
	}
	.kind {
		font-size: var(--text-sm);
		font-weight: 500;
		color: var(--text-primary);
		white-space: nowrap;
	}
	.src {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.db {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-muted);
		white-space: nowrap;
	}
	.reason {
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-muted);
	}
	.add {
		display: flex;
		margin: 2px 0 10px;
	}
	.add :global(.k-btn) {
		flex: 1;
	}
	.done {
		display: flex;
		align-items: center;
		gap: 7px;
		margin: 14px 0 0;
		font-size: var(--text-sm);
		color: var(--text-secondary);
	}
</style>
