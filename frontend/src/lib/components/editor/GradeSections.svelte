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
	import Icon from './Icon.svelte';
	import CurveEditor from './CurveEditor.svelte';
	import GradeWheel from './GradeWheel.svelte';
	import { editor } from '#lib/state.svelte';
	import { report } from '#lib/ops';
	import { inspectLut, pickLutFile } from '#lib/api';
	import { clipById } from '#lib/link-groups';
	import { PERCENT } from '#lib/slider-row';
	import { gradeWriter } from '#lib/grade-write';
	import { sameWheel } from '#lib/grade-wheel';
	import { lutFacts, lutProblem } from '#lib/lut-problem';
	import { tip } from '#lib/tip';
	import { workspace } from '#lib/workspace.svelte';
	import { gradeSectionOrder } from '#lib/inspector-state';
	import {
		IDENTITY_POINTS,
		NEUTRAL_WHEELS,
		WHEEL_RANGES,
		curvesSummary,
		lutName,
		lutSummary,
		normalizeCurve,
		normalizeGrade,
		normalizeWheel,
		withChannel,
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

	const sameCurve = (a: readonly CurvePoint[], b: readonly CurvePoint[]) => JSON.stringify(normalizeCurve(a)) === JSON.stringify(normalizeCurve(b));

	function changeCurve(clipId: string, channel: CurveChannel, change: (points: CurvePoint[]) => CurvePoint[] | null) {
		return write(clipId, (g) => {
			const was = g.curves?.[channel] ?? [];
			const next = change(was.length ? was : IDENTITY_POINTS);
			if (!next || sameCurve(next, was)) return null;
			return { ...g, curves: withChannel(g.curves, channel, normalizeCurve(next)) };
		});
	}

	// ---- wheels ----------------------------------------------------------------

	const RANGE_LABEL: Record<WheelRange, string> = { shadows: 'Shadows', midtones: 'Midtones', highlights: 'Highlights' };
	const wheels = $derived(grade.wheels ?? NEUTRAL_WHEELS);

	const changeWheel = (range: WheelRange) => (clipId: string, change: (wheel: Wheel) => Wheel) =>
		write(clipId, (g) => {
			const all = g.wheels ?? NEUTRAL_WHEELS;
			const next = normalizeWheel(change(all[range]));
			return sameWheel(all[range], next) ? null : { ...g, wheels: { ...all, [range]: next } };
		});

	// ---- LUT -------------------------------------------------------------------

	let info = $state<{ path: string; info: LutInfo | null; error: string | null } | null>(null);
	const lutPath = $derived(grade.lut?.path);
	$effect(() => {
		const path = lutPath;
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
	const lutFile = $derived(lutPath ? lutName(lutPath) : '');
	const problem = $derived(info && info.path === lutPath && info.error ? lutProblem(info.error, lutFile) : null);
	const facts = $derived(info && info.path === lutPath && info.info ? lutFacts(info.info) : '');
	const lutLabel = $derived(problem ? `${problem.short} · ${lutFile}` : lutSummary(clip.grade));

	async function chooseLut() {
		const to = clip.id;
		const path = await pickLutFile();
		if (!path) return;
		await write(to, (g) => ({ ...g, lut: { path, intensity: g.lut?.intensity ?? 1 } }));
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
	{#if grade.lut}
		<div class="file" use:tip={grade.lut.path} data-selectable>
			<Icon n="file-text" s={14} />
			<span class="name">{lutFile}</span>
		</div>
		{#if problem}
			<div class="problem" role="status" use:tip={problem.detail ? { text: problem.short, detail: problem.detail } : problem.short}>
				<span class="mark"><Icon n="alert-triangle" s={14} /></span>
				<span>{problem.text}</span>
			</div>
		{:else if facts}
			<div class="facts">{facts}</div>
		{/if}
		<SliderRow
			label="Intensity"
			value={grade.lut.intensity}
			spec={{ min: 0, max: 1, step: 0.01, neutral: 1, ...PERCENT }}
			scope={clip.id}
			disabled={editor.locked}
			onCommit={(v, to) => write(to, (g) => (g.lut ? { ...g, lut: { ...g.lut, intensity: v } } : null))}
		/>
	{/if}
	<div class="buttons">
		<Btn size="sm" pending={editor.locked} title="Look the clip up through a .cube 3D LUT" onclick={chooseLut}>
			{grade.lut ? (problem ? 'Choose the file again…' : 'Choose another…') : 'Choose LUT…'}
		</Btn>
		{#if grade.lut}
			<Btn
				size="sm"
				pending={editor.locked}
				title="Take the LUT off this clip"
				onclick={() =>
					write(clip.id, (g) => {
						if (!g.lut) return null;
						const { lut: _gone, ...rest } = g;
						return rest;
					})}>Remove LUT</Btn
			>
		{/if}
	</div>
	<div class="hint">Applied after the Color numbers and before the curves and wheels.</div>
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
	.file {
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
	.file :global(svg) {
		flex: none;
		color: var(--text-muted);
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.facts {
		margin: 4px 0 2px;
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.problem {
		display: flex;
		gap: 8px;
		margin: 6px 0;
		padding: 8px 10px;
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
		margin-top: 1px;
		color: var(--warning);
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
</style>
