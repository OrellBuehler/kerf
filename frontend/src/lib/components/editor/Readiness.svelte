<script lang="ts">
	// The platform check: whether the file about to be written will do where it is going. It is
	// judged at the frame and length a render will actually produce — which are not always the
	// project's: a 9:16 project exported at 1920×1080 is a landscape file, and a marked In → out
	// is only that long — and re-judged as the cut changes, because the Deliver panel stays docked
	// beside a timeline that is being edited. What it says, and when it says nothing, is
	// `platform-verdict.ts`; the caller decides whether the export is one it speaks about at all.
	//
	// Renders nothing until the backend has answered, heading included.
	import Icon from './Icon.svelte';
	import SectionHead from './SectionHead.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { platformCheck } from '$lib/api';
	import { VARIANT_PRESETS } from '$lib/delivery-formats';
	import { platformVerdict, type Verdict, type VerdictLevel } from '$lib/platform-verdict';

	/** The frame the render will produce, `null` for the project's own; `range` the span it writes. */
	let {
		frame = null,
		range = null,
		heading = 'Platform check'
	}: { frame?: [number, number] | null; range?: { start: number; end: number } | null; heading?: string } = $props();

	let verdict = $state<Verdict | null>(null);
	let seq = 0;
	$effect(() => {
		void editor.timeline;
		const f: [number, number] | null = frame ? [frame[0], frame[1]] : null;
		const r = range ? { start: range.start, end: range.end } : null;
		const ticked = VARIANT_PRESETS.filter((p) => ui.deliverShapes.includes(p.id));
		const format = editor.timeline.format;
		const own = f === null || (format != null && format.width === f[0] && format.height === f[1]);
		const mine = ++seq;
		const answers = ticked.length
			? Promise.all(
					ticked.map(async (p) => ({
						label: p.label,
						report: await platformCheck([p.format!.width, p.format!.height], r)
					}))
				).then((shapes) => ({ file: null, project: null, shapes }))
			: Promise.all([platformCheck(f, r), own || !format ? null : platformCheck(null, r)]).then(([file, project]) => ({
					file,
					project,
					shapes: []
				}));
		answers
			.then((input) => {
				if (mine === seq) verdict = platformVerdict(input);
			})
			.catch(() => {
				if (mine === seq) verdict = null;
			});
	});

	const ICON: Record<VerdictLevel, string> = { ok: 'check', note: 'info', warning: 'alert-triangle', error: 'x' };
	const COLOR: Record<VerdictLevel, string> = {
		ok: 'var(--success)',
		note: 'var(--text-muted)',
		warning: 'var(--warning)',
		error: 'var(--danger)'
	};
</script>

{#if verdict}
	<SectionHead label={heading} />
	<div
		style="padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);display:flex;flex-direction:column;gap:8px"
	>
		{#each verdict.groups as group, g (g)}
			<div style="display:flex;align-items:flex-start;gap:8px">
				{#if group.shape}
					<span
						style="flex:none;width:36px;font-family:var(--font-mono);font-size:12px;line-height:1.45;color:var(--text-secondary)"
						>{group.shape}</span
					>
				{/if}
				<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:5px">
					{#each group.lines as line, l (l)}
						<div style="display:flex;align-items:flex-start;gap:6px;font-size:12px;line-height:1.45">
							<span style="flex:none;margin-top:1px"><Icon n={ICON[line.level]} s={13} color={COLOR[line.level]} /></span>
							<span style="color:{line.level === 'ok' ? 'var(--success)' : 'var(--text-secondary)'}">{line.text}</span>
						</div>
					{/each}
				</div>
			</div>
		{/each}
		{#if verdict.tips.length}
			<details>
				<summary style="cursor:pointer;font-size:12px;color:var(--text-muted)"
					>{verdict.tips.length} tip{verdict.tips.length === 1 ? '' : 's'}</summary
				>
				{#each verdict.tips as tip (tip)}
					<div
						style="display:flex;align-items:flex-start;gap:6px;margin-top:6px;font-size:12px;line-height:1.45;color:var(--text-muted)"
					>
						<span style="flex:none;margin-top:1px"><Icon n="lightbulb" s={13} color="var(--text-muted)" /></span>
						<span>{tip}</span>
					</div>
				{/each}
			</details>
		{/if}
	</div>
{/if}
