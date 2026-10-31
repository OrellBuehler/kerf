<script lang="ts">
	// The Sound FX panel: Kerf's synthesized sound effects. Search and a category over a grid of sounds; the one picked is
	// auditioned and shaped (length, pitch, intensity, tail, reverse, a new take) in the Shape section; Add puts it on an
	// `FX` lane with its hit on the playhead, and a tile can be dragged onto an audio lane to land its hit at the drop. Under
	// that, Auto sound design finds the moments of the cut that want a sound.
	//
	// The pick, Audition and Add live in a bar along the bottom that does not scroll, so a sound can be picked, heard and
	// added without the controls going out of sight under the grid. Nothing reaches the cut until Add — a slider only moves
	// the draft the audition renders. The state is `sfxPanel`'s (a workspace switch rebuilds this panel and must not lose
	// the pick or a list of moments); the rules are `sfx-panel.ts`'s.
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import EmptyState from './EmptyState.svelte';
	import SectionHead from './SectionHead.svelte';
	import SfxTile from './SfxTile.svelte';
	import SfxSketch from './SfxSketch.svelte';
	import SfxShape from './SfxShape.svelte';
	import SfxAuto from './SfxAuto.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { sfxPanel } from '#lib/sfx-panel.svelte';
	import { beginDrag } from '#lib/drag';
	import { CATEGORY_OPTIONS, anchorNote, glyphOf, gridStep, hueOf, noMatchLine, shapeLine } from '#lib/sfx-panel';
	import { timeDisplay } from '#lib/time-display.svelte';
	import type { SfxPreset } from '#lib/types';

	$effect(() => {
		void sfxPanel.load();
		// A sound still playing is for a panel that is gone.
		return () => sfxPanel.stop();
	});

	const visible = $derived(sfxPanel.visible);
	const spec = $derived(sfxPanel.spec);
	const preset = $derived(sfxPanel.preset);
	const hue = $derived(preset ? hueOf(preset.category) : 'var(--text-muted)');
	const tabStop = $derived(Math.max(0, visible.findIndex((p) => p.kind === sfxPanel.kind)));
	const auditioning = $derived(sfxPanel.auditioning('panel'));
	const blocked = $derived(sfxPanel.blocked);
	const keys = $derived(settings.shortcut('sfx.add') || undefined);

	let grid = $state<HTMLElement | null>(null);

	// ---- the grid's keys: arrows move focus, Space / click pick, Enter adds -----------------------------------
	function options(): HTMLElement[] {
		return grid ? [...grid.querySelectorAll<HTMLElement>('[role="option"]')] : [];
	}

	function onGridKey(e: KeyboardEvent) {
		const items = options();
		const at = items.indexOf((e.target as HTMLElement).closest<HTMLElement>('[role="option"]') as HTMLElement);
		if (at < 0) return;
		if (e.key === 'Enter') {
			e.preventDefault();
			void sfxPanel.addKind(visible[at].kind);
			return;
		}
		if (e.key === 'ArrowLeft' || e.key === 'ArrowRight' || e.key === 'ArrowUp' || e.key === 'ArrowDown' || e.key === 'Home' || e.key === 'End') {
			// How many tiles sit in a row, read off where they are drawn.
			const top = items[0].offsetTop;
			const columns = Math.max(1, items.filter((t) => t.offsetTop === top).length);
			const to = gridStep(at, items.length, columns, e.key);
			e.preventDefault();
			items[to]?.focus();
		}
	}

	function clearSearch() {
		sfxPanel.query = '';
	}

	function onSearchKey(e: KeyboardEvent) {
		if (e.key === 'Escape' && sfxPanel.query) {
			e.preventDefault();
			e.stopPropagation();
			clearSearch();
		}
	}

	// ---- picking, and dragging to a lane --------------------------------------------------------------------
	/** A tile drag just ended: the click that follows its release is not a pick. */
	let dragged = false;
	const SLOP = 4;

	function pick(p: SfxPreset) {
		if (dragged) {
			dragged = false;
			return;
		}
		sfxPanel.select(p.kind);
	}

	function onTileDown(e: PointerEvent, p: SfxPreset) {
		dragged = false;
		if (e.button !== 0) return;
		const [x0, y0] = [e.clientX, e.clientY];
		const root = document.documentElement;
		const cursor = root.style.cursor;
		let active = false;
		const end = () => {
			root.style.cursor = cursor;
			ui.dndSfx = null;
			ui.dndPoint = null;
		};
		// The pointer is the tile's until it is let go; the lanes find the drop by where it is (`ui.dndPoint`), as a bin drag does.
		beginDrag(e, {
			move(m) {
				if (!active) {
					if (Math.hypot(m.clientX - x0, m.clientY - y0) < SLOP) return;
					sfxPanel.select(p.kind, { audition: false });
					sfxPanel.stop();
					const payload = sfxPanel.dragPayload();
					if (!payload) return;
					active = true;
					ui.dndSfx = payload;
					root.style.cursor = 'grabbing';
				}
				ui.dndPoint = { x: m.clientX, y: m.clientY };
			},
			commit(u) {
				const payload = ui.dndSfx;
				end();
				if (!active || !payload) return;
				dragged = true;
				ui.sfxDropTarget?.(payload, u.clientX, u.clientY);
			},
			abandon() {
				end();
				if (active) dragged = true;
			}
		});
	}

	const dragging = $derived(ui.dndSfx !== null);
</script>

<div class="fx" data-testid="sound-fx">
	<div class="scroll">
		<div class="layout">
			<header class="head">
				<div class="search">
					<span class="mag"><Icon n="search" s={13} color="var(--text-muted)" /></span>
					<input
						type="text"
						autocomplete="off"
						spellcheck="false"
						placeholder="Search sounds"
						aria-label="Search sound effects"
						data-testid="sfx-search"
						bind:value={sfxPanel.query}
						onkeydown={onSearchKey}
					/>
					{#if sfxPanel.query}
						<span class="clear"><IconBtn size={22} data-tone="quiet" title="Clear the search" onclick={clearSearch}><Icon n="x" s={12} /></IconBtn></span>
					{/if}
				</div>
				<ChoiceChips label="Category" options={CATEGORY_OPTIONS} value={sfxPanel.category} onpick={(v) => (sfxPanel.category = v)} testid="sfx-category" />
			</header>

			<div class="library">
				{#if sfxPanel.status === 'error'}
					<EmptyState icon="alert-triangle" title="Couldn't load the sound effects" hint={sfxPanel.loadError ?? undefined} pad={24}>
						<Btn size="sm" onclick={() => void sfxPanel.load()}>Try again</Btn>
					</EmptyState>
				{:else if sfxPanel.status !== 'ready'}
					<EmptyState icon="audio-waveform" title="Loading sounds…" pad={24} />
				{:else if visible.length === 0}
					<EmptyState icon="search" title="No sounds" hint={noMatchLine(sfxPanel.query, sfxPanel.category)} pad={24}>
						<Btn
							size="sm"
							onclick={() => {
								clearSearch();
								sfxPanel.category = 'all';
							}}>Show all sounds</Btn
						>
					</EmptyState>
				{:else}
					<div
						bind:this={grid}
						role="listbox"
						aria-label="Sound effects"
						class="grid"
						data-testid="sfx-grid"
						tabindex="-1"
						onkeydown={onGridKey}
					>
						{#each visible as p, i (p.kind)}
							<SfxTile
								preset={p}
								selected={p.kind === sfxPanel.kind}
								tabbable={i === tabStop}
								dragging={dragging && p.kind === sfxPanel.kind}
								onselect={() => pick(p)}
								onadd={() => void sfxPanel.addKind(p.kind)}
								onpointerdown={(e) => onTileDown(e, p)}
							/>
						{/each}
					</div>
					<p class="hint">Click to hear · Enter or double-click to add at the playhead · drag onto an audio track</p>
				{/if}
			</div>

			<div class="side">
				{#if spec && preset}
					<section aria-label="Shape of {preset.label}" data-testid="sfx-selected">
						<SectionHead label="Shape · {preset.label}" />
						<div class="env"><SfxSketch {preset} {hue} reverse={spec.reverse} height={34} label="Shape of {preset.label} over its length; the dashed line is where its hit lands" /></div>
						<p class="desc">{preset.description}</p>

						<SfxShape
							{spec}
							{preset}
							onchange={(patch) => {
								sfxPanel.shape(patch);
								return true;
							}}
							gain={sfxPanel.gainDb}
							ongain={(db) => sfxPanel.setGain(db)}
						/>
						<p class="note" data-testid="sfx-anchor-note">{anchorNote(spec)}</p>
					</section>
				{/if}

				<SfxAuto />
			</div>
		</div>
	</div>

	{#if spec && preset}
		<div class="bar" role="group" aria-label="Selected sound" data-testid="sfx-bar">
			<span class="gl" style="color:{hue}"><Icon n={glyphOf(preset.kind)} s={16} /></span>
			<span class="who">
				<span class="name">{preset.label}</span>
				<span class="sum" data-testid="sfx-summary">{shapeLine(spec)}</span>
			</span>
			<Btn
				variant="secondary"
				icon={auditioning ? 'circle-stop' : 'play'}
				title={auditioning ? 'Stop the sound' : 'Hear this sound as shaped'}
				data-testid="sfx-audition"
				onclick={() => spec && sfxPanel.toggleAudition(spec, 'panel')}>{auditioning ? 'Stop' : 'Audition'}</Btn
			>
			<Btn
				variant="primary"
				icon="plus"
				pending={editor.locked}
				title={{ text: blocked ?? 'Add at the playhead', keys, detail: `${anchorNote(spec)}\nPlayhead at ${timeDisplay.fmt(ui.time)}` }}
				data-testid="sfx-add"
				onclick={() => void sfxPanel.addDraft()}>Add at playhead</Btn
			>
		</div>
		{#if sfxPanel.noteOf('panel')}<p class="quiet" role="status" data-testid="sfx-audition-note">{sfxPanel.noteOf('panel')}</p>{/if}
	{/if}
</div>

<style>
	.fx {
		flex: 1;
		min-width: 0;
		min-height: 0;
		display: flex;
		flex-direction: column;
		background: var(--surface-panel);
		container-type: inline-size;
		container-name: sfx;
	}
	.scroll {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
	}
	.layout {
		display: block;
	}
	.head {
		display: flex;
		flex-direction: column;
		gap: 7px;
		padding: 9px 10px 8px;
		background: var(--surface-panel);
		border-bottom: var(--line-width) solid var(--border-subtle);
	}
	.search {
		position: relative;
		display: flex;
		align-items: center;
	}
	.search .mag {
		position: absolute;
		left: 9px;
		display: inline-flex;
		pointer-events: none;
	}
	.search input {
		box-sizing: border-box;
		width: 100%;
		height: var(--control-sm);
		padding: 0 28px 0 28px;
		font-family: var(--font-sans);
		font-size: var(--text-sm);
		color: var(--text-primary);
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-sm);
	}
	.search input::placeholder {
		color: var(--text-muted);
	}
	.search .clear {
		position: absolute;
		right: 3px;
		display: inline-flex;
	}
	.library {
		padding: 10px 10px 4px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
		gap: 6px;
		outline: none;
	}
	.hint {
		margin: 8px 0 0;
		font-size: var(--text-xs);
		line-height: 1.45;
		color: var(--text-muted);
	}
	.side {
		padding: 0 10px 14px;
	}
	.env {
		padding: 5px 7px;
		border-radius: var(--radius-sm);
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-subtle);
	}
	.desc {
		margin: 8px 0 8px;
		font-size: var(--text-sm);
		line-height: 1.5;
		color: var(--text-muted);
	}
	.note {
		margin: 8px 0 0;
		font-size: var(--text-xs);
		line-height: 1.5;
		color: var(--text-muted);
	}

	/* The pick and what to do with it: always in reach, whatever the grid or the moments have scrolled to. */
	.bar {
		flex: none;
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 10px;
		background: var(--surface-raised);
		border-top: var(--line-width) solid var(--border-default);
	}
	.bar .gl {
		display: inline-flex;
	}
	.who {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
	.name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-base);
		font-weight: 600;
		line-height: 1.1;
		color: var(--text-primary);
	}
	.sum {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.quiet {
		flex: none;
		margin: 0;
		padding: 0 10px 8px;
		background: var(--surface-raised);
		font-size: var(--text-xs);
		line-height: 1.45;
		color: var(--text-secondary);
	}

	/* Wide enough, the sounds keep their column and the shape and the moments get one of their own. */
	@container sfx (min-width: 700px) {
		.scroll {
			overflow: hidden;
		}
		.layout {
			display: grid;
			grid-template-columns: minmax(0, 1fr) minmax(320px, 380px);
			grid-template-rows: auto minmax(0, 1fr);
			height: 100%;
		}
		.head {
			grid-column: 1;
			grid-row: 1;
			border-bottom: 0;
		}
		.library {
			grid-column: 1;
			grid-row: 2;
			overflow-y: auto;
		}
		.side {
			grid-column: 2;
			grid-row: 1 / span 2;
			overflow-y: auto;
			padding-top: 4px;
			border-left: var(--line-width) solid var(--border-subtle);
		}
	}
</style>
