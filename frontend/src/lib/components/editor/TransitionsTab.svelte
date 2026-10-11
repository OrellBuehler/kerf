<script lang="ts">
	// The library's Transitions tab: the grouped picker from `transitions.ts`,
	// applied to the clip selected on the timeline as the transition *into* it
	// (the same `transition_in` the Inspector's Transition section edits).
	import Icon from './Icon.svelte';
	import { editor } from '$lib/state.svelte';
	import { DEFAULT_TRANSITION_SECONDS, TRANSITION_GROUPS, needsSourceHandle, transitionLabel } from '$lib/transitions';
	import { attempt } from '$lib/ops';
	import { chip } from '$lib/chip';
	import type { TransitionKind } from '$lib/types';
	import { FieldEdits } from '$lib/field-edit';
	import { tip } from '$lib/tip';

	const clip = $derived(editor.selectedClip);
	const current = $derived(clip?.transition_in ?? null);
	const reason = $derived(clip ? null : 'Select a clip on the timeline to set the transition into it.');
	const off = $derived(reason !== null || editor.busy);
	/** The clip the length field took focus on and what was typed: its `change` comes with the blur, after the click that selects another clip has moved the selection and re-rendered the field with that clip's length. */
	const length = new FieldEdits<string | null>();
	const clipById = (id: string | null) => editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);

	function pick(kind: TransitionKind) {
		const c = clip;
		if (!c) return;
		void attempt(() => editor.setTransition(c.id, { kind, duration: current?.duration ?? DEFAULT_TRANSITION_SECONDS }));
	}
</script>

<div style="flex:1;min-height:0;overflow-y:auto;padding:12px">
	{#if reason}
		<div
			role="status"
			style="display:flex;gap:7px;align-items:flex-start;padding:8px 10px;margin-bottom:12px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);font-size:12px;line-height:1.45;color:var(--text-muted)"
		>
			<span style="margin-top:1px"><Icon n="lightbulb" s={13} color="var(--text-muted)" /></span>
			<span>{reason}</span>
		</div>
	{:else}
		<div style="display:flex;align-items:center;gap:8px;margin-bottom:10px;font-size:12px;color:var(--text-secondary)">
			<span style="flex:1;min-width:0">
				Into this clip:
				<span style="color:var(--text-primary);font-weight:600"
					>{current ? transitionLabel(current.kind) : 'a hard cut'}</span
				>
			</span>
			{#if current}
				<label style="display:inline-flex;align-items:center;gap:5px;color:var(--text-muted)">
					<span>Length</span>
					<input
						type="number"
						min="0.05"
						step="0.1"
						value={current.duration}
						disabled={editor.busy}
						aria-label="Transition length in seconds"
						onfocus={(e) => length.focus(e.currentTarget, editor.selectedClipId)}
						oninput={(e) => length.input(e.currentTarget, e.currentTarget.value)}
						oncompositionend={(e) => length.input(e.currentTarget, e.currentTarget.value)}
						onchange={(e) => {
							const { scope: id, text } = length.end(e.currentTarget, { scope: editor.selectedClipId, text: e.currentTarget.value });
							const v = parseFloat(text);
							const was = clipById(id)?.transition_in;
							// Put the field back first: an emptied or negative entry would
							// otherwise sit there showing something that was ignored.
							e.currentTarget.value = String(current.duration);
							if (Number.isFinite(v) && v > 0 && id && was)
								void attempt(() => editor.setTransition(id, { kind: was.kind, duration: Math.max(0.05, v) }));
						}}
						style="width:58px;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:4px 5px;text-align:right"
					/>
					<span>s</span>
				</label>
			{/if}
		</div>
	{/if}

	<div style="display:flex;gap:5px;flex-wrap:wrap;margin-bottom:4px">
		<button
			style="{chip(!!clip && !current)};{off ? 'opacity:.5;cursor:default' : ''}"
			disabled={off}
			use:tip={reason ?? 'A hard cut — no transition into this clip'}
			onclick={() => clip && void attempt(() => editor.setTransition(clip.id, null))}>None</button
		>
	</div>

	{#each TRANSITION_GROUPS as g (g.label)}
		<div
			style="margin:14px 0 4px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
		>
			{g.label}
		</div>
		<div style="font-size:11px;color:var(--text-muted);margin-bottom:6px">{g.hint}</div>
		<div style="display:flex;gap:5px;flex-wrap:wrap">
			{#each g.options as o (o.id)}
				<button
					style="{chip(current?.kind === o.id)};{off ? 'opacity:.5;cursor:default' : ''}"
					disabled={off}
					use:tip={reason ?? o.label}
					onclick={() => pick(o.id)}>{o.label}</button
				>
			{/each}
		</div>
	{/each}

	{#if current && needsSourceHandle(current.kind)}
		<div style="font-size:11px;color:var(--text-muted);line-height:1.45;margin-top:14px">
			This transition plays both shots at once, so it borrows the unused footage after the previous clip's cut.
			A clip trimmed to the very end of its source has none to lend and cuts hard instead.
		</div>
	{/if}
</div>
