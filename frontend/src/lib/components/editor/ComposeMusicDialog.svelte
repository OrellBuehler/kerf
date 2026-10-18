<script lang="ts">
	// Write a piece of music and Kerf plays it onto the M track: a style, a tempo and key, and a list of
	// sections with a chord per bar. The first piece downloads the SoundFont it is played on, so the dialog
	// says so up front and then names each stage as it runs — the voiceover dialog's arrangement. "Fit to
	// the picture" nudges the tempo and writes hits so the cuts land on beats; the dialog then shows what
	// that did. Opened with a score it edits that piece again, and with a clip it swaps the clip to the new render.
	import { untrack } from 'svelte';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Chip from './Chip.svelte';
	import CheckRow from './CheckRow.svelte';
	import Icon from './Icon.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import SliderRow from './SliderRow.svelte';
	import { editor, PreviewReadOnly } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { cancelMusic, musicStatus, onMusicProgress } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { failure } from '$lib/failure-copy';
	import { report } from '$lib/ops';
	import { timeDisplay } from '$lib/time-display.svelte';
	import { PREVIEW_READ_ONLY_REASON } from '$lib/proposal-ui';
	import { PERCENT } from '$lib/slider-row';
	import { clipDuration } from '$lib/types';
	import type { ComposeResult, MusicProgress, MusicStatus } from '$lib/types';
	import { DEFAULT_ENERGY, DEFAULT_MAX_TEMPO_CHANGE, MAX_BPM, MAX_SECTIONS, MIN_BPM, MUSIC_STYLES, SECTION_KINDS } from '$lib/music-score';
	import {
		MAX_TEMPO_LIMIT,
		addSection,
		alignAvailable,
		alignChoices,
		alignProblem,
		alignRows,
		alignTrackId,
		composeRequest,
		defaultForm,
		describeComposed,
		firstIssue,
		fmtClock,
		fmtLength,
		formFromScore,
		formIssues,
		formLength,
		hitsLine,
		isMusicCancelled,
		moveSection,
		musicBed,
		musicStageLabel,
		musicTrackEnd,
		removeSection,
		replaceNote,
		rerollSeed,
		revealAt,
		sectionStarts,
		setStyle,
		soundfontNote,
		type ComposeForm,
		type ComposePrefill
	} from '$lib/compose-form';
	import { tip } from '$lib/tip';

	let { onClose, prefill = null }: { onClose: () => void; prefill?: ComposePrefill | null } = $props();

	// The dialog opens once with what it is given; a later change is not its business.
	const start = untrack(() => prefill);
	const replaceId = start?.replaceClipId ?? null;
	let form = $state<ComposeForm>(start ? formFromScore(start.score) : defaultForm());
	let place = $state<'playhead' | 'append'>('append');

	let status = $state<MusicStatus | null>(null);
	let statusError = $state<string | null>(null);
	let generating = $state(false);
	let cancelling = $state(false);
	let progress = $state<MusicProgress | null>(null);
	/** What fitting to the picture did, once a piece with an alignment has landed. */
	let result = $state<ComposeResult | null>(null);

	/** The clip being re-composed, while it is still on the timeline. */
	const target = $derived.by(() => {
		if (!replaceId) return null;
		for (const track of editor.timeline.tracks) {
			const clip = track.clips.find((c) => c.id === replaceId);
			if (clip) return { track, clip };
		}
		return null;
	});

	/** What re-composing replaces: this clip and the other segments of its piece on the track. */
	const bed = $derived(target ? musicBed(target.track, target.clip) : []);
	const bedStart = $derived(Math.min(...bed.map((c) => c.timeline_start), Infinity));

	// The clip going away under the dialog (an undo, an agent's edit) ends it — unless a run is under way,
	// which the backend answers for itself, or the result is on screen.
	$effect(() => {
		if (!generating && !result && replaceId && !target) onClose();
	});

	$effect(() => {
		musicStatus().then(
			(s) => (status = s),
			(e) => (statusError = e instanceof Error ? e.message : String(e))
		);
	});

	const issues = $derived(formIssues(form));
	const length = $derived(formLength(form));
	const starts = $derived(sectionStarts(form));
	const choices = $derived(alignChoices(editor.timeline));
	const alignOk = $derived(alignAvailable(choices));
	const alignBlocked = $derived(alignProblem(choices, form));
	const cutTracks = $derived(choices.tracks.filter((t) => t.cuts > 0));
	const trackId = $derived(alignTrackId(choices, form));
	const mEnd = $derived(musicTrackEnd(editor.timeline));
	const note = $derived(status ? soundfontNote(status) : null);
	const pct = $derived(progress?.fraction == null ? null : Math.round(progress.fraction * 100));

	/** Why Compose is off, in a line (also its tooltip); null when it can go. */
	const blocked = $derived.by(() => {
		if (editor.previewingStaged) return PREVIEW_READ_ONLY_REASON;
		if (editor.busy && !generating) return 'Another edit is running';
		return firstIssue(issues) ?? alignBlocked;
	});
	const canCompose = $derived(blocked === null && !generating);

	const label = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

	async function compose() {
		if (!canCompose) return;
		generating = true;
		cancelling = false;
		progress = null;
		// Progress is a courtesy: a listener that cannot be attached must not stop the piece being made.
		const unlisten = await onMusicProgress((p) => {
			progress = p;
		}).catch(() => () => {});
		try {
			const done = await editor.composeMusic(
				composeRequest(form, choices, { replaceClipId: replaceId, at: place === 'playhead' ? ui.time : undefined })
			);
			toast.success(describeComposed(done), { action: { label: 'Undo', onClick: () => void editor.undo() } });
			// Show the piece where it landed, unless the playhead is already inside it.
			const at = revealAt(ui.time, done.clip.timeline_start, clipDuration(done.clip));
			if (at !== null) ui.seek(at, { snap: 'up' });
			if (done.align) result = done;
			else onClose();
		} catch (e) {
			// A stop is the user's own doing — back to the form, no notice.
			if (isMusicCancelled(e)) return;
			if (e instanceof PreviewReadOnly) report(e);
			else toast.error(failure('Composing failed', e));
		} finally {
			unlisten();
			generating = false;
			cancelling = false;
			progress = null;
		}
	}

	async function stop() {
		cancelling = true;
		await cancelMusic();
	}

	const FIELD =
		'box-sizing:border-box;height:var(--control-sm);min-width:0;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm)';
	const numCss = `${FIELD};font-family:var(--font-mono);padding:0 7px;text-align:right`;
	const textCss = `${FIELD};padding:0 8px`;
	const bad = (message: string | null) => (message ? ';border-color:var(--danger)' : '');
	const labelCss = 'font-size:12px;color:var(--text-muted)';
	const boxCss =
		'font-size:12px;line-height:1.5;color:var(--text-secondary);background:var(--surface-inset);border-radius:var(--radius-sm);padding:8px 10px';
	const errCss = 'font-size:11px;line-height:1.4;color:var(--red-400)';
	const GRID = 'display:grid;grid-template-columns:36px 108px 56px minmax(0,1fr) 62px 72px;column-gap:6px;align-items:center';
</script>

<Modal
	title="Compose music"
	icon="music"
	width={700}
	busy={generating}
	dismissOnScrim={false}
	initial="[data-initial]"
	rescue
	{onClose}
>
	{#if result?.align}
		<div
			data-testid="compose-result"
			aria-live="polite"
			style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px"
		>
			<div style="display:flex;align-items:center;gap:8px;font-size:13px;color:var(--text-primary)">
				<Icon n="circle-check" s={16} color="var(--success)" />
				<span>{describeComposed(result)}</span>
			</div>
			<div style="font-size:12px;color:var(--text-muted)">
				{result.align.cuts === 0
					? 'No cut fell inside the music, so there was nothing to fit it to.'
					: `Fitted to ${result.align.cuts} ${result.align.cuts === 1 ? 'cut' : 'cuts'} of the picture.`}
			</div>
			<div
				style="display:grid;grid-template-columns:1fr auto;gap:5px 12px;padding:10px 12px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)"
			>
				{#each alignRows(result.align) as row (row.label)}
					<span style={labelCss}>{row.label}</span>
					<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-primary);text-align:right">{row.value}</span>
				{/each}
			</div>
			{#each result.align.notes as n (n)}
				<div style="font-size:12px;line-height:1.45;color:var(--text-secondary)">{n}</div>
			{/each}
			<div style="font-size:11px;color:var(--text-muted)">
				The clip is selected on the timeline. Undo takes it out again.
			</div>
		</div>
	{:else}
		<fieldset
			disabled={generating}
			style="flex:1;min-height:0;min-width:0;margin:0;border:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:12px"
		>
			{#if target}
				<div style={boxCss}>{replaceNote(bed.length, target.track.name, timeDisplay.fmt(bedStart))}</div>
			{/if}
			{#if note}
				<div style={boxCss}>{note}</div>
			{:else if statusError}
				<div style="{errCss};font-size:12px;background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:8px 10px">
					⚠ Couldn't check whether the SoundFont is downloaded — {statusError}
				</div>
			{/if}

			<div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap">
				<span style="{labelCss};width:70px;flex:none">Style</span>
				{#each MUSIC_STYLES as s (s.name)}
					<Chip pressed={form.style === s.name} title="{s.label} · {s.default_bpm} BPM" onclick={() => (form = setStyle(form, s.name))}>
						{s.label}
					</Chip>
				{/each}
			</div>

			<div style="display:flex;align-items:flex-start;gap:16px;flex-wrap:wrap">
				<label style="display:flex;flex-direction:column;gap:3px">
					<span style={labelCss}>Tempo</span>
					<span style="display:flex;align-items:center;gap:6px">
						<input
							data-initial
							type="number"
							min={MIN_BPM}
							max={MAX_BPM}
							step="1"
							aria-label="Tempo in BPM"
							aria-invalid={issues.bpm ? 'true' : undefined}
							bind:value={form.bpm}
							style="{numCss};width:72px{bad(issues.bpm)}"
						/>
						<span style={labelCss}>BPM</span>
					</span>
				</label>
				<label style="display:flex;flex-direction:column;gap:3px">
					<span style={labelCss}>Key</span>
					<input
						type="text"
						aria-label="Key"
						aria-invalid={issues.key ? 'true' : undefined}
						spellcheck="false"
						autocapitalize="off"
						autocomplete="off"
						placeholder="Am"
						bind:value={form.key}
						use:tip={'A tonic and an optional m: G, F#m, Bb, Ebm. Chords written as roman numerals (I, V, vi, IV) are in this key.'}
						style="{textCss};width:72px{bad(issues.key)}"
					/>
				</label>
				<div style="display:flex;flex-direction:column;gap:3px">
					<span style={labelCss}>Variation</span>
					<span style="display:flex;align-items:center;gap:4px">
						<input
							type="number"
							min="0"
							step="1"
							aria-label="Variation number"
							aria-invalid={issues.seed ? 'true' : undefined}
							bind:value={form.seed}
							style="{numCss};width:84px{bad(issues.seed)}"
						/>
						<IconBtn
							title={{
								text: 'Another take',
								detail: 'The same score and variation render the same audio. A new number changes the feel of the playing and the hook.'
							}}
							size={28}
							onclick={() => (form = rerollSeed(form))}><Icon n="refresh-cw" s={14} /></IconBtn
						>
					</span>
				</div>
			</div>
			{#each [issues.bpm, issues.key, issues.seed].filter((m) => m !== null) as m}
				<div role="alert" style={errCss}>{m}</div>
			{/each}

			<div role="group" aria-label="Sections" style="display:flex;flex-direction:column;gap:6px">
				<div style="display:flex;align-items:baseline;gap:8px">
					<span class="k-overline" style="flex:1">Sections</span>
					<span data-testid="compose-length" style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">
						{length.bars} {length.bars === 1 ? 'bar' : 'bars'}{#if length.seconds !== null}&nbsp;·&nbsp;{fmtLength(length.seconds)}{/if}
					</span>
				</div>
				<div style="{GRID};{labelCss}">
					<span>Start</span>
					<span>Kind</span>
					<span style="text-align:right">Bars</span>
					<span use:tip={'One chord per bar: names (C, Em, F#m7, Gsus4, D/F#) or roman numerals in the key (I, V, vi, IV, ii7). A shorter list repeats.'}
						>Chords, one per bar</span
					>
					<span style="text-align:right" use:tip={'0 to 1: how loud and busy the section is. Empty: by kind.'}>Energy</span>
					<span></span>
				</div>
				{#each form.sections as s, i (s.id)}
					{@const e = issues.sections[i]}
					<div data-testid="compose-section" style="{GRID};padding:1px 0">
						<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)"
							>{starts[i] === null ? '' : fmtClock(starts[i] ?? 0)}</span
						>
						<select class="k-select" aria-label="Section {i + 1} kind" bind:value={s.kind} style="width:100%">
							{#each SECTION_KINDS as k (k)}
								<option value={k}>{label(k)}</option>
							{/each}
						</select>
						<input
							type="number"
							min="1"
							max="128"
							step="1"
							aria-label="Section {i + 1} bars"
							aria-invalid={e?.bars ? 'true' : undefined}
							bind:value={s.bars}
							style="{numCss};width:100%{bad(e?.bars ?? null)}"
						/>
						<input
							type="text"
							aria-label="Section {i + 1} chords"
							aria-invalid={e?.chords ? 'true' : undefined}
							spellcheck="false"
							autocapitalize="off"
							autocomplete="off"
							placeholder="I V vi IV   or   C G Am F"
							bind:value={s.chords}
							style="{textCss};width:100%;font-family:var(--font-mono){bad(e?.chords ?? null)}"
						/>
						<input
							type="number"
							min="0"
							max="1"
							step="0.05"
							aria-label="Section {i + 1} energy"
							aria-invalid={e?.energy ? 'true' : undefined}
							placeholder={String(DEFAULT_ENERGY[s.kind])}
							bind:value={s.energy}
							style="{numCss};width:100%{bad(e?.energy ?? null)}"
						/>
						<span style="display:flex;justify-content:flex-end">
							<IconBtn
								title="Move up"
								size={24}
								disabled={i === 0}
								onclick={() => (form = moveSection(form, s.id, -1))}><Icon n="chevron-up" s={14} /></IconBtn
							>
							<IconBtn
								title="Move down"
								size={24}
								disabled={i === form.sections.length - 1}
								onclick={() => (form = moveSection(form, s.id, 1))}><Icon n="chevron-down" s={14} /></IconBtn
							>
							<IconBtn
								title="Remove section"
								size={24}
								disabled={form.sections.length === 1}
								onclick={() => (form = removeSection(form, s.id))}><Icon n="x" s={14} /></IconBtn
							>
						</span>
						{#each [e?.bars, e?.chords, e?.energy].filter((m) => m) as m}
							<div role="alert" style="{errCss};grid-column:2 / -1">{m}</div>
						{/each}
					</div>
				{/each}
				<div style="display:flex;align-items:center;gap:8px">
					<Btn
						size="sm"
						variant="secondary"
						icon="plus"
						disabled={form.sections.length >= MAX_SECTIONS}
						onclick={() => (form = addSection(form))}>Add section</Btn
					>
					{#if form.hits.length > 0}
						<span style="{labelCss};margin-left:auto" use:tip={'Moments the picture asked of the music: impacts on downbeats, a dropout and a riser before a drop. They sit at fixed times in the score.'}
							>{form.hits.length} {form.hits.length === 1 ? 'hit' : 'hits'} kept: {hitsLine(form.hits)}</span
						>
						<Btn size="sm" variant="ghost" onclick={() => (form = { ...form, hits: [] })}>Clear hits</Btn>
					{/if}
				</div>
				{#if issues.general}
					<div role="alert" style="{errCss};font-size:12px;display:flex;gap:6px;align-items:flex-start">
						<span style="flex:none;margin-top:1px"><Icon n="alert-triangle" s={13} /></span>
						<span>{issues.general}</span>
					</div>
				{/if}
			</div>

			<div style="display:flex;flex-direction:column;gap:8px;padding:10px 12px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)">
				<CheckRow
					checked={form.align}
					disabled={!alignOk && !form.align}
					onchange={(ev) => (form.align = ev.currentTarget.checked)}
					data-testid="compose-align"
				>
					Fit to the picture
				</CheckRow>
				{#if form.align}
					<div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap">
						<span style="{labelCss};width:90px;flex:none">Land on</span>
						<Chip
							pressed={form.alignTo === 'cuts'}
							title="The boundaries between the clips of a video track"
							onclick={() => (form.alignTo = 'cuts')}>Cuts</Chip
						>
						<Chip
							pressed={form.alignTo === 'markers'}
							title="Every marker on the timeline"
							onclick={() => (form.alignTo = 'markers')}>Markers</Chip
						>
						{#if form.alignTo === 'cuts' && cutTracks.length > 1}
							<select
								class="k-select"
								aria-label="Video track whose cuts to use"
								value={trackId ?? ''}
								onchange={(ev) => (form.alignTrack = ev.currentTarget.value)}
							>
								{#each cutTracks as t (t.id)}
									<option value={t.id}>{t.name} · {t.cuts} {t.cuts === 1 ? 'cut' : 'cuts'}</option>
								{/each}
							</select>
						{/if}
					</div>
					<SliderRow
						label="Tempo change"
						value={form.maxTempo}
						spec={{ min: 0, max: MAX_TEMPO_LIMIT, step: 0.01, neutral: DEFAULT_MAX_TEMPO_CHANGE, ...PERCENT }}
						labelWidth={90}
						reserve={false}
						disabled={generating}
						onCommit={(v) => (form.maxTempo = v)}
					/>
					{#if alignBlocked}
						<div role="alert" style={errCss}>{alignBlocked}</div>
					{/if}
				{/if}
				<div style="font-size:11px;line-height:1.5;color:var(--text-muted)">
					{#if !alignOk && !form.align}
						Needs a video track with a cut in it, or a marker, to land on.
					{:else}
						Nudges the tempo, by up to the amount above, so the cuts land on beats, and writes an impact on each cut that
						lands on a downbeat and a dropout and riser before a drop. You see what it did afterwards.
					{/if}
				</div>
			</div>

			{#if !target}
				<div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap">
					<span style="{labelCss};width:90px;flex:none">Place</span>
					<Chip pressed={place === 'append'} onclick={() => (place = 'append')}>
						{mEnd > 0 ? `After the music on M (${timeDisplay.fmt(mEnd)})` : 'At the start'}
					</Chip>
					<Chip pressed={place === 'playhead'} onclick={() => (place = 'playhead')}>
						At playhead ({timeDisplay.fmt(ui.time)})
					</Chip>
				</div>
			{/if}
		</fieldset>
	{/if}
	{#snippet footer()}
		<div style="padding:12px 16px">
			{#if generating}
				<div style="display:flex;align-items:center;gap:10px">
					<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:6px" aria-live="polite">
						<div style="display:flex;align-items:baseline;gap:8px;font-size:12px">
							<span style="color:var(--text-primary)">{progress ? musicStageLabel(progress.stage) : 'Starting…'}</span>
							<span
								style="flex:1;min-width:0;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
								>{progress?.detail ?? ''}</span
							>
							{#if pct !== null}
								<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{pct}%</span>
							{/if}
						</div>
						<ProgressBar value={progress?.fraction ?? null} label="Composing the music" />
					</div>
					<Btn variant="destructive" size="md" data-rescue disabled={cancelling} onclick={stop}>
						{cancelling ? 'Stopping…' : 'Cancel'}
					</Btn>
				</div>
			{:else if result}
				<div style="display:flex;align-items:center;gap:8px">
					<div style="flex:1"></div>
					<Btn variant="primary" size="md" data-rescue onclick={onClose}>Done</Btn>
				</div>
			{:else}
				<div style="display:flex;align-items:center;gap:8px">
					<div style="flex:1"></div>
					<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
					<Btn
						variant="primary"
						size="md"
						icon="music"
						data-rescue
						disabled={!canCompose}
						title={blocked ?? (target ? 'Render the piece and swap the clip to it' : 'Render the piece and put it on the timeline')}
						onclick={compose}>{target ? 'Re-compose' : 'Compose music'}</Btn
					>
				</div>
			{/if}
		</div>
	{/snippet}
</Modal>
