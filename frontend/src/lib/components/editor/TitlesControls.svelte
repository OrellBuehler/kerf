<script lang="ts">
	// Titles, lower thirds and captions: style chips that add one at the playhead,
	// the caption button (and which look it generates in), importing captions from
	// a subtitle file, and the lane's list of what is there. The content of the
	// Inspector's "Titles lane" section and of the library's Titles tab — one
	// component, so the two cannot drift.
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { CAPTION_LOOKS, TEXT_STYLES } from '$lib/style-presets';
	import { addStyledTitle, addTextHere, dropCaptions, importCaptionFile, makeCaptions, pickTitle } from '$lib/title-actions';
	import {
		CAPTION_EXTENSIONS,
		CAPTION_HINT,
		IMPORT_BASES,
		NO_SOURCE_HINT,
		baseHint,
		clearHint,
		generatedCount,
		importableAssets,
		recaptionHint,
		resolveChoice
	} from '$lib/caption-import-ui';
	import { attempt } from '$lib/ops';
	import { chip } from '$lib/chip';

	const uid = $props.id();

	const overlays = $derived(editor.overlays);
	const captionCount = $derived(generatedCount(overlays));
	const hasCaptions = $derived(captionCount > 0);

	// ---- importing captions from a subtitle file ----------------------------
	// The options row opens under the buttons; the file is picked from inside it, so
	// the timing is settled before the picker opens (the lane's menu, which has no
	// room for options, offers the same import with the timing in the label).
	let importOpen = $state(false);

	/** The assets a file can be timed to: those a clip of the cut shows. */
	const offered = $derived(importableAssets(editor.timeline, editor.assets));
	/** Until an asset is picked the selected clip's is the default. */
	const choice = $derived(
		resolveChoice(ui.captionImportBase, ui.captionImportAsset, offered, [editor.selectedClip?.asset_id])
	);
	const choiceAsset = $derived(offered.find((a) => a.id === choice.assetId));
	const lookLabel = $derived(CAPTION_LOOKS.find((c) => c.id === ui.captionStyle)?.label ?? ui.captionStyle);

	// The Inspector scrolls: keep the options (and the Choose file button under them)
	// in view when they open, and when picking a clip makes them taller.
	$effect(() => {
		if (!importOpen) return;
		void choice.base;
		document.getElementById(`${uid}-import`)?.scrollIntoView({ block: 'nearest' });
	});

	async function chooseFile() {
		if (await importCaptionFile(choice)) closeImport(true);
	}

	function closeImport(refocus: boolean) {
		importOpen = false;
		if (refocus) document.getElementById(`${uid}-toggle`)?.focus();
	}

	/** Escape folds the options away and hands focus back to the button that opened
	 *  them; the page's own Escape (clear the selection) does not also fire. */
	function onImportKey(e: KeyboardEvent) {
		if (e.key !== 'Escape') return;
		e.preventDefault();
		e.stopPropagation();
		closeImport(true);
	}

	/** Minutes:seconds.centiseconds — a title's start in the lane's list. */
	function tc(s: number): string {
		const t = Math.max(0, s);
		const m = Math.floor(t / 60);
		const sec = Math.floor(t % 60);
		const cs = Math.floor((t % 1) * 100);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}.${cs.toString().padStart(2, '0')}`;
	}

	const selectCss =
		'background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:12px;padding:5px 7px';
	const fieldLabel = 'font-size:12px;color:var(--text-muted)';

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
		<button
			style={chip(ui.captionStyle === c.id)}
			title={c.hint}
			aria-pressed={ui.captionStyle === c.id}
			onclick={() => (ui.captionStyle = c.id)}
		>
			{c.label}
		</button>
	{/each}
</div>
<div style="display:flex;flex-wrap:wrap;gap:7px;margin-bottom:6px">
	<Btn size="sm" variant="ghost" style="flex:1" disabled={editor.busy} onclick={() => void addTextHere()}>+ Text</Btn>
	<Btn
		size="sm"
		variant="ghost"
		disabled={editor.busy}
		title={hasCaptions ? recaptionHint(captionCount) : CAPTION_HINT}
		onclick={() => void makeCaptions()}
	>
		{hasCaptions ? 'Recaption' : 'Captions'}
	</Btn>
	{#if hasCaptions}
		<Btn size="sm" variant="ghost" disabled={editor.busy} title={clearHint(captionCount)} onclick={() => void dropCaptions()}>Clear</Btn>
	{/if}
	<Btn size="sm" variant="ghost" icon="mic" disabled={editor.busy} title="Speak a script onto the timeline" onclick={() => ui.openVoiceover()}
		>Voiceover…</Btn
	>
	<Btn
		id="{uid}-toggle"
		size="sm"
		variant={importOpen ? 'secondary' : 'ghost'}
		icon="file-text"
		aria-expanded={importOpen}
		aria-controls="{uid}-import"
		title="Caption the cut from a .srt or .ass subtitle file"
		onclick={() => (importOpen = !importOpen)}>Import captions…</Btn
	>
</div>
{#if importOpen}
	<!-- The Escape handler is a convenience for the fields inside; every control it
	     covers is a real button / select that Tab already reaches. -->
	<div role="presentation" onkeydown={onImportKey}>
		<div
			id="{uid}-import"
			role="group"
			aria-label="Import captions"
			style="margin-bottom:8px;padding:9px 10px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-inset);display:flex;flex-direction:column;gap:7px"
		>
			<div style="font-size:12px;font-weight:600;color:var(--text-primary)">Import captions from a subtitle file</div>
			<div style="display:flex;flex-direction:column;gap:5px">
				<span style={fieldLabel}>Timing</span>
				<div role="group" aria-label="Timing" style="display:flex;gap:5px;flex-wrap:wrap">
					{#each IMPORT_BASES as b (b.id)}
						{@const unavailable = b.id === 'source' && offered.length === 0}
						<button
							style={chip(choice.base === b.id) + (unavailable ? ';opacity:0.5;cursor:default' : '')}
							aria-pressed={choice.base === b.id}
							disabled={unavailable}
							onclick={() => (ui.captionImportBase = b.id)}
						>
							{b.label}
						</button>
					{/each}
				</div>
			</div>
			{#if choice.base === 'source'}
				<label style="display:flex;align-items:center;gap:8px">
					<span style="{fieldLabel};flex:none">Clip</span>
					<select
						value={choice.assetId ?? ''}
						onchange={(e) => (ui.captionImportAsset = e.currentTarget.value)}
						style="{selectCss};flex:1;min-width:0"
					>
						{#each offered as a (a.id)}
							<option value={a.id}>{a.label}{a.clips > 1 ? ` · ${a.clips} clips` : ''}</option>
						{/each}
					</select>
				</label>
			{/if}
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">
				{offered.length === 0 ? NO_SOURCE_HINT : baseHint(choice, choiceAsset?.label)}
			</div>
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">
				Look: <span style="color:var(--text-secondary)">{lookLabel}</span>, from Caption style above. Fonts, colors and
				positions in the file are ignored. {captionCount > 0
					? `The ${captionCount} caption${captionCount === 1 ? '' : 's'} already on the cut will be replaced.`
					: ''}
			</div>
			<div style="display:flex;align-items:center;gap:7px;flex-wrap:wrap">
				<Btn size="sm" variant="primary" icon="file-text" disabled={editor.busy} onclick={() => void chooseFile()}
					>Choose file…</Btn
				>
				<Btn size="sm" variant="ghost" onclick={() => closeImport(true)}>Cancel</Btn>
				<span style="font-size:12px;color:var(--text-muted)"
					>{CAPTION_EXTENSIONS.map((e) => `.${e}`).join(' ')}</span
				>
			</div>
		</div>
	</div>
{/if}
{#if overlays.length === 0}
	<div style="font-size:12px;color:var(--text-muted);line-height:1.4">
		No titles or captions yet. Add text, caption the whole cut from the transcripts of the clips on
		the timeline (captions land on the words that survived your edit), or import a subtitle file.
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
