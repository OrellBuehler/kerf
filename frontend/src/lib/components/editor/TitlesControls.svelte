<script lang="ts">
	// Titles, lower thirds and captions: style chips that add one at the playhead,
	// the caption button (and which look it generates in), and the lane's list of
	// what is there. The content of the Inspector's "Titles lane" section and of
	// the library's Titles tab — one component, so the two cannot drift.
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { CAPTION_LOOKS, TEXT_STYLES } from '$lib/style-presets';
	import { addStyledTitle, addTextHere, dropCaptions, makeCaptions, pickTitle } from '$lib/title-actions';
	import { attempt } from '$lib/ops';
	import { chip } from '$lib/chip';

	const overlays = $derived(editor.overlays);
	const hasCaptions = $derived(overlays.some((o) => o.generated));

	/** Minutes:seconds.centiseconds — a title's start in the lane's list. */
	function tc(s: number): string {
		const t = Math.max(0, s);
		const m = Math.floor(t / 60);
		const sec = Math.floor(t % 60);
		const cs = Math.floor((t % 1) * 100);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}.${cs.toString().padStart(2, '0')}`;
	}

	const xBtn =
		'margin-left:auto;background:transparent;border:none;color:var(--text-muted);cursor:pointer;font-size:16px;line-height:1;min-width:28px;min-height:28px;padding:2px 5px';
</script>

<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-bottom:6px">
	Titles sit on their own lane in the timeline, apart from the clips.
</div>
<div style="display:flex;gap:5px;flex-wrap:wrap;margin-bottom:6px">
	{#each TEXT_STYLES as s (s.id)}
		<button style={chip(false)} disabled={editor.busy} onclick={() => void addStyledTitle(s)}>+ {s.label}</button>
	{/each}
</div>
<div style="display:flex;align-items:center;flex-wrap:wrap;gap:5px;margin-bottom:6px">
	<span style="font-size:12px;color:var(--text-muted)">Caption style</span>
	{#each CAPTION_LOOKS as c (c.id)}
		<button style={chip(ui.captionStyle === c.id)} title={c.hint} onclick={() => (ui.captionStyle = c.id)}>
			{c.label}
		</button>
	{/each}
</div>
<div style="display:flex;flex-wrap:wrap;gap:7px;margin-bottom:6px">
	<Btn size="sm" variant="ghost" style="flex:1" disabled={editor.busy} onclick={() => void addTextHere()}>+ Text</Btn>
	<Btn size="sm" variant="ghost" disabled={editor.busy} onclick={() => void makeCaptions()}>
		{hasCaptions ? 'Recaption' : 'Captions'}
	</Btn>
	{#if hasCaptions}
		<Btn size="sm" variant="ghost" disabled={editor.busy} onclick={() => void dropCaptions()}>Clear</Btn>
	{/if}
	<Btn size="sm" variant="ghost" icon="mic" disabled={editor.busy} title="Speak a script onto the timeline" onclick={() => ui.openVoiceover()}
		>Voiceover…</Btn
	>
</div>
{#if overlays.length === 0}
	<div style="font-size:12px;color:var(--text-muted);line-height:1.4">
		No titles or captions yet. Add text, or caption the whole cut from the transcripts of the clips on
		the timeline — captions land on the words that survived your edit.
	</div>
{/if}
{#each overlays as o (o.id)}
	<div style="display:flex;align-items:center;gap:6px;padding:3px 0">
		<button
			onclick={() => pickTitle(o)}
			style="flex:1;min-width:0;display:flex;align-items:center;gap:8px;background:transparent;border:none;cursor:pointer;text-align:left;color:{editor.selectedOverlayId === o.id ? 'var(--kerf-300)' : 'var(--text-secondary)'};padding:0"
		>
			<span style="flex:1;min-width:0;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				>{o.text || '(empty)'}</span
			>
			<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted)">{tc(o.start)}</span>
		</button>
		<button onclick={() => void attempt(() => editor.removeOverlay(o.id))} disabled={editor.busy} title="Remove" aria-label="Remove" style={xBtn}
			>×</button
		>
	</div>
{/each}
