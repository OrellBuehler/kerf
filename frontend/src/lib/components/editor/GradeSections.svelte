<script lang="ts">
	// A clip's grade beyond the Color numbers: a curve editor, three colour wheels and a LUT. Each gesture writes one
	// edit when it ends (a point or a puck let go, a run of arrow keys gone quiet, a number typed, a slider released, a file
	// chosen); Escape, pointercancel or a lost focus abandon a drag and put it back. Nothing here is keyable (`model/grade.rs`),
	// so there is no ◇. What is written is built from the grade of the clip the gesture *began* on, as the cut has it when it
	// ends (`grade-write.ts`), never from the clip on screen by then. The curve and the wheels' numbers are `grade.ts`'s, the TS
	// mirror of the engine's, so what is drawn here is what the export renders. The preview shows the grade through FFmpeg
	// (the GPU preview refuses a graded clip), so it matches the file. The sections' own controls are `CurveEditor`,
	// `GradeWheel` (which lays itself out from the box of `.wheels`, below) and the LUT's, here.
	import InspectorSection from './InspectorSection.svelte';
	import SliderRow from './SliderRow.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import CurveEditor from './CurveEditor.svelte';
	import GradeWheel from './GradeWheel.svelte';
	import { editor } from '#lib/state.svelte';
	import { report } from '#lib/ops';
	import { inspectLut, pickLutFile } from '#lib/api';
	import { clipById } from '#lib/link-groups';
	import { PERCENT } from '#lib/slider-row';
	import { curveChange, gradeWriter, wheelChange } from '#lib/grade-write';
	import { lutProblem } from '#lib/lut-problem';
	import { tip } from '#lib/tip';
	import { workspace } from '#lib/workspace.svelte';
	import { gradeSectionOrder } from '#lib/inspector-state';
	import {
		NEUTRAL_WHEELS,
		WHEEL_RANGES,
		curvesSummary,
		lutName,
		lutSummary,
		normalizeGrade,
		wheelsSummary,
		type CurveChannel,
		type WheelRange
	} from '#lib/grade';
	import type { Clip, CurvePoint, LutInfo, Wheel } from '#lib/types';

	let { clip }: { clip: Clip } = $props();

	const grade = $derived(normalizeGrade(clip.grade));

	const write = gradeWriter({
		gradeOf: (id) => {
			const c = clipById(editor.timeline, id);
			return c ? normalizeGrade(c.grade) : undefined;
		},
		set: (id, g) => editor.setGrade(id, g),
		report
	});

	// ---- curves ----------------------------------------------------------------

	const changeCurve = (clipId: string, channel: CurveChannel, change: (points: CurvePoint[]) => CurvePoint[] | null) =>
		write(clipId, curveChange(channel, change));

	// ---- wheels ----------------------------------------------------------------

	const RANGE_LABEL: Record<WheelRange, string> = { shadows: 'Shadows', midtones: 'Midtones', highlights: 'Highlights' };
	const wheels = $derived(grade.wheels ?? NEUTRAL_WHEELS);

	const changeWheel = (range: WheelRange) => (clipId: string, change: (wheel: Wheel) => Wheel) => write(clipId, wheelChange(range, change));

	// ---- LUT -------------------------------------------------------------------

	let info = $state<{ path: string; info: LutInfo | null; error: string | null } | null>(null);
	/** Bumped to look at the file again: "Choose the file again…" with the very path it has, and the section being opened. The file
	 *  may have come back (or gone) since the path was last looked at, and a path that has not changed is not looked at again by itself. */
	let checks = $state(0);
	let lutRoot = $state<HTMLElement | null>(null);
	const uid = $props.id();
	const lutPath = $derived(grade.lut?.path);
	$effect(() => {
		const path = lutPath;
		void checks;
		if (!path) {
			info = null;
			return;
		}
		let live = true;
		inspectLut(path).then(
			(i) => live && (info = { path, info: i, error: null }),
			(e) => live && (info = { path, info: null, error: e instanceof Error ? e.message : String(e) })
		);
		return () => {
			live = false;
		};
	});
	$effect(() => {
		const section = lutRoot?.closest('details');
		if (!section) return;
		const opened = () => {
			if (section.open) checks += 1;
		};
		section.addEventListener('toggle', opened);
		return () => section.removeEventListener('toggle', opened);
	});
	const lutFile = $derived(lutPath ? lutName(lutPath) : '');
	const looked = $derived(info && info.path === lutPath ? info : null);
	const problem = $derived(looked?.error ? lutProblem(looked.error) : null);
	const lutSize = $derived(looked?.info ? `${looked.info.size}³` : '');
	/** What the file's box says on hover: where it is, what it holds, and where in the chain it sits. */
	const fileTip = $derived(
		grade.lut
			? {
					text: grade.lut.path,
					detail: [looked?.info?.title ?? '', 'Applied after the Color numbers and before the curves and wheels.'].filter(Boolean).join('\n')
				}
			: undefined
	);
	const lutLabel = $derived(problem ? `${problem.short} · ${lutFile}` : lutSummary(clip.grade));

	async function chooseLut() {
		const to = clip.id;
		const path = await pickLutFile();
		if (!path) return;
		await write(to, (g) => ({ ...g, lut: { path, intensity: g.lut?.intensity ?? 1 } }));
		checks += 1;
	}

	function removeLut() {
		return write(clip.id, (g) => {
			if (!g.lut) return null;
			const { lut: _gone, ...rest } = g;
			return rest;
		});
	}
</script>

{#snippet curvesSection()}
<InspectorSection title="Curves" summary={curvesSummary(clip.grade)}>
	<CurveEditor {clip} locked={editor.locked} onChange={changeCurve} />
</InspectorSection>
{/snippet}

{#snippet wheelsSection()}
<InspectorSection title="Color wheels" summary={wheelsSummary(clip.grade)}>
	<div class="wheels">
		<div class="set">
			{#each WHEEL_RANGES as range (range)}
				<GradeWheel label={RANGE_LABEL[range]} wheel={wheels[range]} clipId={clip.id} locked={editor.locked} onChange={changeWheel(range)} />
			{/each}
		</div>
	</div>
	<div class="foot">
		<Btn
			size="sm"
			pending={editor.locked}
			disabled={!grade.wheels}
			title="Back to neutral wheels"
			onclick={() =>
				write(clip.id, (g) => {
					if (!g.wheels) return null;
					const { wheels: _gone, ...rest } = g;
					return rest;
				})}>Reset wheels</Btn
		>
	</div>
</InspectorSection>
{/snippet}

{#snippet lutSection()}
<InspectorSection title="LUT" summary={lutLabel}>
	<div class="lut" bind:this={lutRoot}>
		{#if grade.lut}
			<div class="top">
				<div class="file" class:bad={!!problem} use:tip={fileTip} data-selectable>
					<Icon n="file-text" s={14} />
					<span class="name">{lutFile}</span>
					{#if lutSize && !problem}<span class="size">{lutSize}</span>{/if}
				</div>
				<IconBtn size={24} title={problem ? 'Choose the file again…' : 'Choose another LUT…'} pending={editor.locked} onclick={chooseLut}>
					<Icon n="folder-open" s={14} />
				</IconBtn>
				<IconBtn size={24} title="Remove LUT" pending={editor.locked} onclick={removeLut}>
					<Icon n="trash" s={14} />
				</IconBtn>
			</div>
			<SliderRow
				label="Intensity"
				value={grade.lut.intensity}
				spec={{ min: 0, max: 1, step: 0.01, neutral: 1, ...PERCENT }}
				scope={clip.id}
				disabled={editor.locked}
				onCommit={(v, to) => write(to, (g) => (g.lut ? { ...g, lut: { ...g.lut, intensity: v } } : null))}
			/>
			{#if problem}
				<div class="problem" role="status" aria-describedby="{uid}-why" use:tip={{ text: problem.short, detail: [problem.text, problem.detail].filter(Boolean).join('\n') }}>
					<span class="mark"><Icon n="alert-triangle" s={13} /></span>
					<span class="line">{problem.line}</span>
				</div>
				<span id="{uid}-why" class="vh">{problem.text}</span>
			{/if}
		{:else}
			<div class="buttons">
				<Btn size="sm" pending={editor.locked} title="Look the clip up through a .cube 3D LUT" onclick={chooseLut}>Choose LUT…</Btn>
			</div>
			<div class="hint">Applied after the Color numbers and before the curves and wheels.</div>
		{/if}
	</div>
</InspectorSection>
{/snippet}

<!-- The sections in the order the active workspace draws them (`inspector-state.ts`: Color puts the wheels and the LUT before the curves). -->
{#each gradeSectionOrder(workspace.shown) as id (id)}
	{#if id === 'curves'}
		{@render curvesSection()}
	{:else if id === 'color-wheels'}
		{@render wheelsSection()}
	{:else}
		{@render lutSection()}
	{/if}
{/each}

<style>
	/* The box the wheels lay themselves out from (`GradeWheel`'s `@container wheels`): its width, not the window's. */
	.wheels {
		container: wheels / inline-size;
		margin: 6px 0 2px;
	}
	.set {
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 6px;
		align-items: start;
	}
	@container wheels (max-width: 287px) {
		.set {
			grid-template-columns: minmax(0, 1fr);
			row-gap: 12px;
		}
	}
	.foot {
		display: flex;
		justify-content: flex-end;
		margin-top: 8px;
	}
	/* A LUT that is set is two rows and, when it cannot be used, a third of one line: the name with its two buttons, the intensity,
	   and the warning. The hint about where it sits in the chain is the name's tooltip, so the section stays short (a 468 px Inspector
	   has the wheels above it). */
	.top {
		display: flex;
		align-items: center;
		gap: 4px;
		min-width: 0;
		margin-bottom: 2px;
	}
	.file {
		flex: 1 1 0;
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		height: var(--control-sm);
		padding: 0 8px;
		box-sizing: border-box;
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-sm);
		color: var(--text-secondary);
		font-size: var(--text-sm);
	}
	.file.bad {
		border-color: color-mix(in srgb, var(--warning) 60%, transparent);
	}
	.file :global(svg) {
		flex: none;
		color: var(--text-muted);
	}
	.name {
		flex: 1 1 auto;
		min-width: 0;
		overflow: hidden;
		overflow-wrap: anywhere;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.size {
		flex: none;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.problem {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		margin: 2px 0 0;
		padding: 3px 8px;
		font-size: var(--text-xs);
		line-height: var(--leading-snug);
		color: var(--text-primary);
		background: var(--warning-surface);
		border: var(--line-width) solid color-mix(in srgb, var(--warning) 45%, transparent);
		border-radius: var(--radius-sm);
	}
	.mark {
		flex: none;
		display: inline-flex;
		color: var(--warning);
	}
	.line {
		min-width: 0;
		overflow: hidden;
		overflow-wrap: anywhere;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.buttons {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		margin: 6px 0;
	}
	.hint {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	/* Said to a screen reader, not shown. */
	.vh {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}
</style>
