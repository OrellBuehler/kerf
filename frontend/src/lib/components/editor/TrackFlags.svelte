<script lang="ts">
	// The flags of a track's header: duck (audio), mute / hide, solo, lock and remove. Each is an `IconBtn` named for its
	// track ("Mute A1"), so it has the hover, the pressed look and the focus ring of every other icon button — and while an
	// edit is in flight, or a proposal is on screen, it is `pending` (aria-disabled, the press swallowed), not natively
	// disabled, which would drop the keyboard focus. The words are `track-flags.ts`'s.
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { editor } from '#lib/state.svelte';
	import { report } from '#lib/ops';
	import { PREVIEW_READ_ONLY_REASON } from '#lib/proposal-ui';
	import { trackFlagCopy } from '#lib/track-flags';
	import type { Track } from '#lib/types';

	let { track: t, onremove }: { track: Track; onremove: (t: Track) => void } = $props();

	const copy = $derived(trackFlagCopy(t));
	const pending = $derived(editor.locked);
	/** The tooltip says why while the proposal is on screen; the name stays the control's. */
	const tipOf = (text: string) => (editor.previewingStaged ? PREVIEW_READ_ONLY_REASON : text);
	const word = 'font-family:var(--font-mono);font-size:12px;font-weight:600;line-height:1';
</script>

{#if t.kind === 'audio'}
	<IconBtn
		size={26}
		active={!!t.duck}
		{pending}
	data-tone="quiet"
		title={tipOf(copy.duck.tip)}
		aria-label={copy.duck.name}
		style="width:auto;min-width:26px;padding:0 5px;{word}"
		onclick={() => void editor.setTrackDuck(t.id, !t.duck).catch(report)}>DUCK</IconBtn
	>
{/if}
<IconBtn
	size={26}
	active={!!t.muted}
	{pending}
	data-tone="quiet"
	title={tipOf(copy.mute.tip)}
	aria-label={copy.mute.name}
	style={t.muted ? 'color:var(--red-500)' : ''}
	onclick={() => void editor.setTrackMuted(t.id, !t.muted).catch(report)}
	><Icon n={t.kind === 'video' ? (t.muted ? 'eye-off' : 'eye') : t.muted ? 'volume-x' : 'volume-2'} s={12} /></IconBtn
>
<IconBtn
	size={26}
	active={!!t.solo}
	{pending}
	data-tone="quiet"
	title={tipOf(copy.solo.tip)}
	aria-label={copy.solo.name}
	style={word}
	onclick={() => void editor.setTrackSolo(t.id, !t.solo).catch(report)}>S</IconBtn
>
<IconBtn
	size={26}
	active={!!t.locked}
	{pending}
	data-tone="quiet"
	title={tipOf(copy.lock.tip)}
	aria-label={copy.lock.name}
	data-on-ink="primary"
	onclick={() => void editor.setTrackLocked(t.id, !t.locked).catch(report)}
	><Icon n={t.locked ? 'lock' : 'lock-open'} s={12} /></IconBtn
>
<IconBtn size={26} {pending} title={tipOf(copy.remove.tip)} aria-label={copy.remove.name} data-tone="quiet" onclick={() => onremove(t)}
	><Icon n="x" s={12} /></IconBtn
>
