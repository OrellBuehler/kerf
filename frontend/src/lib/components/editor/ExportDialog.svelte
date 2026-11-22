<script lang="ts">
	import { onDestroy } from 'svelte';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import SliderRow from './SliderRow.svelte';
	import SectionHead from './SectionHead.svelte';
	import ScrollArea from './ScrollArea.svelte';
	import FormRow from './FormRow.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import CheckRow from './CheckRow.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import ExportOutcomes from './ExportOutcomes.svelte';
	import DeliverTo from './DeliverTo.svelte';
	import Readiness from './Readiness.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import {
		clipStabilizationStatus,
		confirmAction,
		exportSimulated,
		inTauri,
		pathsExist,
		pickExportPath,
		hwEncoders,
		knownHwEncoders
	} from '#lib/api';
	import { unsteadyNote } from '#lib/stabilize';
	import { overwriteCopy } from '#lib/confirm-copy';
	import { exportTargets, filesToReplace } from '#lib/export-overwrite';
	import { progressLine } from '#lib/export-progress';
	import { writtenNotice } from '#lib/export-written';
	import { toast } from '#lib/notifications.svelte';
	import { frameLine, hasPicture } from '#lib/delivery-formats';
	import { hasAudioStream, shapesWritten } from '#lib/export-plan';
	import { failed, headline, isStop, reasonLines, succeeded } from '#lib/export-outcomes';
	import { copyText } from '#lib/clipboard';
	import { folderOption } from '#lib/folder-option';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { frameKey, restoreDraft } from '#lib/export-draft';
	import { FORM_GAP, FORM_LABEL_W, FORM_SLIDER_LABEL_W, FORM_TRACK_AT } from '#lib/form-rows';
	import { judgeable } from '#lib/platform-verdict';
	import type { RowSpec } from '#lib/slider-row';
	import type { Container, ExportOptions, ExportReport, Fit, RateControl, SubtitleFormat } from '#lib/types';
	import { exportedLine, failure } from '#lib/failure-copy';
	import {
		initialExport,
		presetsByGroup,
		CONTAINERS,
		VIDEO_CODECS,
		AUDIO_CODECS,
		PRORES_PROFILES,
		RATE_CONTROLS,
		AUDIO_BITRATES,
		SAMPLE_RATES,
		SCALERS,
		GIF_DITHERS,
		FITS,
		RESOLUTIONS,
		frameRateOptions,
		applyPreset,
		carriesChapters,
		carriesSubtitles,
		presetsFor,
		containerInfo,
		isHwCodec,
		LOUDNESS_TARGETS,
		loudnessChoice,
		reconcileContainer,
		videoCodecDefaults,
		validateExport,
		buildSummary,
		buildCommandPreview,
		withLoudness,
		type LoudnessChoice
	} from '#lib/export-presets';

	let { onClose }: { onClose: () => void } = $props();

	const hasCaptions = $derived((editor.timeline.overlays ?? []).some((o) => o.generated));
	const chapterCount = $derived(editor.markers.filter((m) => m.kind === 'chapter').length);
	const sidecar = (f: SubtitleFormat) => (opts.caption_sidecars ?? []).includes(f);
	// Options for captions the cut does not have are off, and stay off in what is exported.
	$effect(() => {
		if (!hasCaptions && (opts.embed_captions || opts.skip_burned_captions || (opts.caption_sidecars ?? []).length)) {
			opts = { ...opts, embed_captions: false, skip_burned_captions: false, caption_sidecars: [] };
		}
	});
	function toggleSidecar(f: SubtitleFormat, on: boolean) {
		const rest = (opts.caption_sidecars ?? []).filter((x) => x !== f);
		patch({ caption_sidecars: on ? [...rest, f] : rest });
	}

	// The export opens on the frame the project is cut for. A 9:16 project whose
	// dialog defaulted to Web 1080p opened already landscape — and the readiness
	// panel below then warned about the shape the user had just chosen. When a
	// preset renders exactly the project frame, start from it; otherwise start
	// from the default preset with its resolution cleared, so "Project frame" is
	// what renders.
	// What the dialog was last closed on comes back, unless the project has been re-cut for another
	// frame since: the options are not lost to a Cancel, an Escape or a stray click.
	const frame = frameKey(editor.timeline.format);
	const remembered = restoreDraft(ui.exportDraft, frame);
	const initial = remembered ? { id: remembered.preset, opts: remembered.opts } : initialExport(editor.timeline.format);

	let opts = $state<ExportOptions>(initial.opts);
	let outputPath = $state('');
	/** The file the native save dialog returned — it asked about replacing it already. */
	let pickedPath = $state<string | null>(null);
	let activePreset = $state(initial.id);
	let customRes = $state(remembered?.customRes ?? false);
	onDestroy(() => {
		ui.exportDraft = { opts: $state.snapshot(opts), preset: activePreset, customRes, frame };
	});
	// The render outlives this dialog (see `editor.exportRun`), so reopening it
	// mid-render shows the progress and a Stop rather than a dead Export button.
	const rendering = $derived(editor.exportRun !== null);
	const line = $derived(progressLine(editor.exportRun?.progress));
	const cancelling = $derived(editor.exportRun?.cancelling ?? false);
	let useRange = $state(false);

	// What this dialog's exports left behind. A run ends in `ui.exportOutcomes` whichever dialog (or none) is open;
	// this one shows what arrived since it opened — the files, or why not — until it is put away, so the payoff of
	// an export is where the user is looking, and stays.
	const openedAt = Date.now();
	let putAway = $state(0);
	const latest = $derived(ui.exportOutcomes[0]);
	const fresh = $derived(latest && latest.at >= openedAt && latest.id !== putAway ? latest : null);
	const result = $derived(fresh?.ok ? fresh : null);
	const refusal = $derived(fresh && !fresh.ok ? fresh : null);
	const earlier = $derived(ui.exportOutcomes.filter((o) => o !== fresh));

	// GPU encoders the backend verified usable on this machine; merged into the
	// codec choices once known (empty in the browser harness).
	let hwCodecs = $state<string[]>(knownHwEncoders());
	const presets = $derived(presetsFor(hwCodecs));
	$effect(() => {
		hwEncoders()
			.then((list) => (hwCodecs = list))
			.catch(() => {});
	});

	/** The timeline's in/out marks when both are set and ordered, else null. */
	const marks = $derived(
		ui.markIn !== null && ui.markOut !== null && ui.markOut > ui.markIn
			? { start: ui.markIn, end: ui.markOut }
			: null
	);

	const fmtTime = (s: number): string => timeDisplay.fmt(s);

	const assets = $derived(editor.assets);
	const hasVideo = $derived(hasPicture(editor.timeline));
	const hasAudio = $derived(hasAudioStream(editor.timeline, assets));
	const info = $derived(containerInfo(opts.container));
	const vc = $derived(opts.video_codec ? VIDEO_CODECS[opts.video_codec] : undefined);
	const qualityOptions = $derived(vc?.crf ? [
		{ value: String(Math.min(vc.crf[1], vc.crf[2] + 6)), label: 'Smaller file' },
		{ value: String(vc.crf[2]), label: 'Balanced' },
		{ value: String(Math.max(vc.crf[0], vc.crf[2] - 4)), label: 'Higher quality' }
	] : []);
	const qualityValue = $derived(opts.rate_control === 'crf' && qualityOptions.some(q => Number(q.value) === (opts.crf ?? vc?.crf?.[2]))
		? String(opts.crf ?? vc?.crf?.[2]) : 'custom');

	const ac = $derived(opts.audio_codec ? AUDIO_CODECS[opts.audio_codec] : undefined);
	const loudness = $derived(loudnessChoice(opts));
	const loudnessHint = $derived(LOUDNESS_TARGETS.find((t) => t.id === loudness)?.hint ?? '');
	const issues = $derived(validateExport(opts, hasVideo, hasAudio));
	const summary = $derived(buildSummary(opts, hasVideo, hasAudio));
	const command = $derived(buildCommandPreview(opts, hasVideo, hasAudio, outputPath || undefined));
	const showVideo = $derived(!info.audioOnly && hasVideo);
	/** The stabilized clips that will export as shot (never measured, or measuring failed): said, not refused. */
	let unsteady = $state<string | null>(null);
	$effect(() => {
		void editor.timeline;
		let live = true;
		clipStabilizationStatus()
			.then((statuses) => {
				if (live) unsteady = unsteadyNote(statuses);
			})
			.catch(() => {
				if (live) unsteady = null;
			});
		return () => {
			live = false;
		};
	});
	const judged = $derived(judgeable(opts, editor.timeline));
	// Shapes only mean something for a picture: an audio-only render ignores
	// whatever the Deliver panel had ticked.
	const variantFormats = $derived(shapesWritten(opts, editor.timeline, ui.deliverShapes));
	const showAudio = $derived(!info.videoOnly && hasAudio);
	const canExport = $derived(!editor.busy && !rendering && issues.length === 0 && (hasVideo || hasAudio));

	/** A box for typing in: the kit's select height and ink, in the mono face for numbers and paths. */
	const FIELD =
		'box-sizing:border-box;height:var(--control-sm);min-width:0;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:var(--text-sm);padding:0 8px';
	/** The width a select takes in the control column: every one starts at the same x and ends at the same x. */
	const SELECT = 'width:100%;max-width:260px';

	function msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
	function patch(p: Partial<ExportOptions>) {
		opts = { ...opts, ...p };
		activePreset = 'custom';
	}
	function choosePreset(id: string) {
		opts = applyPreset(id, hwCodecs);
		activePreset = id;
		customRes = false;
	}
	function swapExt(path: string, ext: string): string {
		return path.replace(/\.[^./\\]+$/, '') + '.' + ext;
	}
	function changeContainer(c: Container) {
		opts = reconcileContainer({ ...opts, container: c, embed_captions: opts.embed_captions && carriesSubtitles(c) });
		activePreset = 'custom';
		customRes = false;
		if (outputPath) outputPath = swapExt(outputPath, containerInfo(c).ext);
	}
	function setVideoCodec(id: string) {
		const p = videoCodecDefaults(id);
		// Hardware encoders have no two-pass mode; land on CRF instead.
		if (isHwCodec(id) && opts.rate_control === 'two_pass') p.rate_control = 'crf';
		patch(p);
	}
	function setRate(r: RateControl) {
		const p: Partial<ExportOptions> = { rate_control: r };
		if (r === 'crf' && opts.crf == null && vc?.crf) p.crf = vc.crf[2];
		if ((r === 'bitrate' || r === 'two_pass') && !opts.video_bitrate) p.video_bitrate = '8M';
		patch(p);
	}
	// The project's delivery frame, when it has one — what an unset export
	// resolution now resolves to.
	const projectFrame = $derived.by(() => {
		const d = editor.timeline.format;
		return d ? `${d.width}×${d.height}` : null;
	});

	// ...and its frame rate: what an unset one resolves to.
	const projectRate = $derived(editor.timeline.format?.fps ?? null);

	function resValue(): string {
		if (customRes) return 'custom';
		return opts.resolution ? `${opts.resolution[0]}x${opts.resolution[1]}` : 'source';
	}
	function setRes(v: string) {
		if (v === 'custom') {
			customRes = true;
			if (!opts.resolution) patch({ resolution: [1920, 1080] });
			else activePreset = 'custom';
		} else if (v === 'source') {
			customRes = false;
			patch({ resolution: null });
		} else {
			customRes = false;
			const [w, h] = v.split('x').map(Number);
			patch({ resolution: [w, h] });
		}
	}
	async function browse() {
		const p = await pickExportPath(info.ext);
		if (p) {
			outputPath = p;
			pickedPath = p;
		}
	}
	async function doExport() {
		if (!inTauri() && !exportSimulated()) {
			toast.info('Export renders with FFmpeg in the desktop app.');
			return;
		}
		if (!outputPath) {
			await browse();
			if (!outputPath) return;
		}
		// The files this will write that are already there — the save dialog asked about the one it picked, not
		// about a container switched since, nor about the per-shape siblings of a multi-format export.
		const targets = exportTargets(outputPath, variantFormats);
		const replace = filesToReplace(targets, await pathsExist(targets).catch(() => []), pickedPath);
		if (replace.length > 0) {
			const copy = overwriteCopy(replace);
			if (!(await confirmAction(copy.message, copy.title, copy))) return;
		}
		// A new run starts clean: what the last one left on this dialog is put away.
		putAway = latest?.id ?? 0;
		try {
			await render(useRange && marks ? { ...opts, range: marks } : opts);
		} catch (e) {
			const m = msg(e);
			// Caption files already beside the output: name them and ask before replacing them.
			if (m.includes('already exists; allow overwriting')) {
				const replaceCaptions = await confirmAction(`${m.replace(/; allow overwriting.*$/, '')}. Replace it?`, 'Replace caption files?', {
					ok: 'Replace',
					cancel: 'Cancel'
				});
				if (!replaceCaptions) return;
				try {
					await render({ ...(useRange && marks ? { ...opts, range: marks } : opts), overwrite_sidecars: true });
				} catch (again) {
					exportFailed(again);
				}
				return;
			}
			exportFailed(e);
		}
	}
	function exportFailed(e: unknown) {
		// A user-requested stop is not a failure — keep the dialog open quietly.
		// A stopped single pass of several files says it removed them all.
		if (isStop(e)) {
			toast.info('E' + msg(e).slice(1));
			return;
		}
		// The reason is kept where the user can read it again (the dialog, the Deliver panel), not only in a toast.
		const outcome = ui.noteExport((id, at) => failed(e, outputPath || null, at, id));
		// A reason can be the tail of ffmpeg's own output: the toast says the first line and the log keeps the rest.
		const { first, rest } = reasonLines(outcome.ok ? '' : outcome.reason);
		toast.error(failure('Export failed', first || e), rest ? { detail: rest } : undefined);
	}
	async function render(finalOpts: ExportOptions) {
		const reports = variantFormats.length
			? await editor.exportVariants(outputPath, variantFormats, ui.deliverSmartCrop, finalOpts)
			: [await editor.export(outputPath, finalOpts)];
		finished(reports);
	}
	/** The notice an export ends on: the files, and what the written files measure — their loudness and true
	 *  peak, a warning when a true peak is over the target's ceiling. A path in a toast is not much use on its
	 *  own, so it offers the folder; the toast outlasts the default four seconds, and the files are kept besides
	 *  (`ui.exportOutcomes`, shown here and in the Deliver panel) and in the notification log, which can show the folder again. */
	function finished(reports: readonly ExportReport[]) {
		ui.noteExport((id, at) => succeeded(reports, at, id));
		const first = reports[0]?.path;
		const { over, description } = writtenNotice(reports);
		const shown = {
			duration: FILE_TOAST_MS,
			...(description ? { description } : {}),
			...(first ? folderOption(first) : {})
		};
		const text = exportedLine(reports.map((r) => r.path));
		if (over) toast.warning(text, shown);
		else toast.success(text, shown);
	}
	/** How long the toast for a written file stays: long enough to find the button after looking away. */
	const FILE_TOAST_MS = 10_000;
	const stop = () => editor.stopExport();
</script>

{#snippet secHead(label: string)}
	<SectionHead {label} />
{/snippet}

{#snippet selectRow(label: string, value: string, items: { value: string; label: string }[], onChange: (v: string) => void, o: { disabled?: boolean; hint?: string; groups?: { group: string; items: { value: string; label: string }[] }[] } = {})}
	<FormRow {label} hint={o.hint}>
		{#snippet control(id, describedby)}
			<select {id} aria-describedby={describedby} class="k-select" style={SELECT} {value} disabled={o.disabled} onchange={(e) => onChange(e.currentTarget.value)}>
				{#each items as it (it.value)}
					<option value={it.value}>{it.label}</option>
				{/each}
				{#each o.groups ?? [] as g (g.group)}
					<optgroup label={g.group}>
						{#each g.items as it (it.value)}
							<option value={it.value}>{it.label}</option>
						{/each}
					</optgroup>
				{/each}
			</select>
		{/snippet}
	</FormRow>
{/snippet}

{#snippet toggleRow(label: string, checked: boolean, onChange: (v: boolean) => void, hint?: string)}
	<div style="display:grid;grid-template-columns:{FORM_LABEL_W}px minmax(0,1fr);column-gap:{FORM_GAP}px;padding:2px 0">
		<div style="grid-column:2;min-width:0">
			<CheckRow {checked} onchange={(e) => onChange(e.currentTarget.checked)}>{label}</CheckRow>
			{#if hint}
				<div style="padding-left:24px;font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)">{hint}</div>
			{/if}
		</div>
	</div>
{/snippet}

{#snippet sliderRow(label: string, value: number, spec: RowSpec, onChange: (v: number) => void, hint?: string)}
	<SliderRow {label} {value} {spec} labelWidth={FORM_SLIDER_LABEL_W} reserve={false} onCommit={onChange} />
	{#if hint}
		<div style="padding:0 0 4px {FORM_TRACK_AT}px;font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)">{hint}</div>
	{/if}
{/snippet}

{#snippet textRow(label: string, value: string, placeholder: string, onChange: (v: string) => void, hint?: string)}
	<FormRow {label} {hint}>
		{#snippet control(id, describedby)}
			<input
				{id}
				aria-describedby={describedby}
				type="text"
				{value}
				{placeholder}
				onchange={(e) => onChange(e.currentTarget.value)}
				style="{FIELD};width:100%;max-width:260px"
			/>
		{/snippet}
	</FormRow>
{/snippet}

<!-- A click on the scrim does not close this one: it holds a preset, shapes and encoder settings. -->
<Modal title="Export" icon="upload" width={600} dismissOnScrim={false} rescue {onClose}>
	{#if result}
		<ScrollArea padding="14px 16px">
			<div data-testid="export-result" aria-live="polite" style="display:flex;flex-direction:column;gap:10px">
				<ExportOutcomes outcomes={[result]} />
				<div style="font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)">
					The Deliver panel keeps this, and the notification log, until you quit.
				</div>
			</div>
		</ScrollArea>
	{:else}
	<ScrollArea padding="12px 16px">
		{@render selectRow(
			'Preset',
			activePreset,
			activePreset === 'custom' ? [{ value: 'custom', label: 'Custom settings' }] : [],
			choosePreset,
			{
				hint: presets.find((p) => p.id === activePreset)?.description ?? 'Your own settings — pick a preset to start over from one.',
				groups: presetsByGroup(presets).map((g) => ({ group: g.group, items: g.presets.map((p) => ({ value: p.id, label: p.label })) }))
			}
		)}
		<!-- destination -->
		{@render secHead('Destination')}
		<FormRow label="Save to">
			{#snippet control(id)}
				<div style="display:flex;align-items:center;gap:8px;width:100%">
					<input
						{id}
						type="text"
						readonly
						value={outputPath}
						placeholder="Choose a file…"
						style="{FIELD};flex:1;color:var(--text-secondary)"
					/>
					<Btn variant="secondary" size="sm" icon="folder-open" onclick={browse}>Browse…</Btn>
				</div>
			{/snippet}
		</FormRow>
		{#if marks}
			{@render selectRow(
				'Range',
				useRange ? 'range' : 'full',
				[
					{ value: 'full', label: 'Full timeline' },
					{ value: 'range', label: `In → out (${fmtTime(marks.start)}–${fmtTime(marks.end)})` }
				],
				(v) => (useRange = v === 'range')
			)}
		{/if}

		<!-- one cut, every platform -->
		{#if showVideo}
			{@render secHead('Shapes to write')}
			<DeliverTo {outputPath} ext={info.ext} />
		{/if}

		{#if showVideo}
			{@render secHead('Picture')}
			<FormRow
				label="Project frame"
				hint="Set in the Deliver workspace or with the timeline’s Frame button. The rows below change this export only."
			>
				{#snippet control(_id, describedby)}
					<span aria-describedby={describedby} style="font-family:var(--font-mono);font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-primary)"
						>{frameLine(editor.timeline.format)}</span
					>
				{/snippet}
			</FormRow>
			{#if variantFormats.length}
				<FormRow label="Size">
					{#snippet control()}
						<span style="font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-muted)">Each shape brings its own size and fit.</span>
					{/snippet}
				</FormRow>
			{:else}
				{@render selectRow(
					'Resolution',
					resValue(),
					[
						...RESOLUTIONS.map((r) => ({
							value: r.value ? `${r.value[0]}x${r.value[1]}` : 'source',
							// With a project frame set, "Source" is that frame — the export and
							// the preview must not be able to disagree about the deliverable.
							label: r.value ? r.label : projectFrame ? `Project frame (${projectFrame})` : r.label
						})),
						{ value: 'custom', label: 'Custom…' }
					],
					setRes,
					{ hint: !opts.resolution && projectFrame ? 'Renders the project frame. Pick a size to override it for this export only.' : undefined }
				)}
				{#if customRes}
					<FormRow label="Custom size">
						{#snippet control()}
							<div style="display:flex;align-items:center;gap:8px">
								<input
									type="number"
									min="2"
									step="2"
									aria-label="Width"
									value={opts.resolution?.[0] ?? 1920}
									onchange={(e) => patch({ resolution: [parseInt(e.currentTarget.value) || 1920, opts.resolution?.[1] ?? 1080] })}
									style="{FIELD};width:84px;text-align:right"
								/>
								<span style="color:var(--text-muted)">×</span>
								<input
									type="number"
									min="2"
									step="2"
									aria-label="Height"
									value={opts.resolution?.[1] ?? 1080}
									onchange={(e) => patch({ resolution: [opts.resolution?.[0] ?? 1920, parseInt(e.currentTarget.value) || 1080] })}
									style="{FIELD};width:84px;text-align:right"
								/>
							</div>
						{/snippet}
					</FormRow>
				{/if}
				{#if opts.resolution}
					{@render selectRow(
						'Fit',
						opts.fit ?? 'contain',
						FITS.map((f) => ({ value: f.id, label: f.label })),
						(v) => patch({ fit: v as Fit }),
						{ hint: FITS.find((f) => f.id === (opts.fit ?? 'contain'))?.hint }
					)}
				{/if}
			{/if}
			{@render selectRow(
				'Frame rate',
				opts.fps ? String(opts.fps) : 'source',
				frameRateOptions(opts.fps, projectRate).map((f) => ({ value: f.value ? String(f.value) : 'source', label: f.label })),
				(v) => patch({ fps: v === 'source' ? null : parseFloat(v) }),
				{ hint: !opts.fps && projectRate ? 'Renders at the project frame’s rate. Pick a rate to override it for this export only.' : undefined }
			)}
			{#if qualityOptions.length}
				{@render selectRow(
					'Quality',
					qualityValue,
					[
						...(qualityValue === 'custom' ? [{ value: 'custom', label: 'Custom · see Advanced' }] : []),
						...qualityOptions
					],
					(v) => {
						if (v !== 'custom') patch({ rate_control: 'crf', crf: Number(v) });
					},
					{ hint: 'Higher quality keeps more detail and makes a larger file.' }
				)}
			{:else}
				<FormRow label="Quality">
					{#snippet control()}
						<span style="font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-muted)"
							>{opts.video_codec === 'gif'
								? 'Follows the picture size and the palette settings in Advanced.'
								: 'This preset is a mastering format. Its profile is in Advanced.'}</span
						>
					{/snippet}
				</FormRow>
			{/if}
		{/if}
		{#if showAudio}
			{@render secHead('Sound')}
			{@render toggleRow('Include audio', !!opts.include_audio, (v) => patch({ include_audio: v }))}
			{#if opts.include_audio}
				{@render selectRow(
					'Loudness',
					loudness,
					LOUDNESS_TARGETS.map((t) => ({ value: t.id, label: t.label })),
					(v) => patch(withLoudness(opts, v as LoudnessChoice)),
					{ hint: loudnessHint }
				)}
				{#if loudness !== 'off'}
					{@render toggleRow(
						'Keep the dynamics',
						!!opts.loudness_static,
						(v) => patch({ loudness_static: v }),
						opts.loudness_static
							? 'Measures the mix first, then moves it by one gain and holds the true peak with a limiter — quiet and loud passages keep their distance. The export takes a little longer.'
							: 'Off, the level is evened out as it plays (loudnorm), which narrows the gap between quiet and loud passages.'
					)}
				{/if}
			{/if}
		{/if}

		<!-- the platform check: a video export's only (not audio, a GIF or an empty cut), per file when several are written -->
		{#if judged}
			<Readiness frame={opts.resolution ?? null} range={useRange ? marks : null} />
		{/if}
		{#if showVideo && unsteady}
			<p role="status" style="margin:8px 0 0;font-size:var(--text-sm);line-height:1.5;color:var(--warning)">{unsteady}</p>
		{/if}

		<div style="margin-top:12px">
			<InspectorSection title="Advanced encoding" summary={summary}>
				{@render selectRow(
					'Container',
					opts.container,
					CONTAINERS.map((c) => ({ value: c.id, label: `${c.label} (.${c.ext})` })),
					(v) => changeContainer(v as Container)
				)}
				<!-- video -->
				{#if showVideo}
					{@render secHead('Video')}
					{@render selectRow(
						'Codec',
						opts.video_codec ?? '',
						[...info.video, ...info.hwVideo.filter((id) => hwCodecs.includes(id))].map((id) => ({
							value: id,
							label: VIDEO_CODECS[id]?.label ?? id
						})),
						setVideoCodec
					)}

					{#if opts.video_codec === 'prores_ks'}
						{@render selectRow(
							'Profile',
							String(opts.prores_profile ?? 3),
							PRORES_PROFILES.map((p) => ({ value: String(p.value), label: p.label })),
							(v) => patch({ prores_profile: Number(v) })
						)}
					{:else if opts.video_codec === 'gif'}
						<FormRow label="Rate control">
							{#snippet control()}
								<span style="font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-muted)">Quality is set by the palette and the resolution / fps above.</span>
							{/snippet}
						</FormRow>
					{:else}
						<FormRow
							label="Rate control"
							top
							hint={opts.rate_control === 'two_pass'
								? 'Two passes ≈ double the render time.'
								: opts.rate_control === 'lossless'
									? 'Mathematically lossless — very large files.'
									: undefined}
						>
							{#snippet control()}
								<ChoiceChips
									label="Rate control"
									value={opts.rate_control ?? 'crf'}
									options={RATE_CONTROLS.filter((rc) => rc.id !== 'two_pass' || !isHwCodec(opts.video_codec)).map((rc) => ({ value: rc.id, label: rc.label }))}
									onpick={setRate}
								/>
							{/snippet}
						</FormRow>

						{#if opts.rate_control === 'crf' && vc?.crf}
							{@render sliderRow(
								'Quality (CRF)',
								opts.crf ?? vc.crf[2],
								{ min: vc.crf[0], max: vc.crf[1], step: 1, digits: 0, neutral: vc.crf[2] },
								(v) => patch({ crf: Math.round(v) }),
								'Lower = higher quality and a larger file.'
							)}
						{:else if opts.rate_control === 'bitrate' || opts.rate_control === 'two_pass'}
							{@render textRow('Target bitrate', opts.video_bitrate ?? '', 'e.g. 8M', (v) => patch({ video_bitrate: v }))}
						{/if}

						{#if vc?.presets}
							{@render selectRow(
								vc.presetKind === 'cpuused' ? 'Speed (cpu-used)' : vc.presetKind === 'svtav1' ? 'Speed (preset)' : 'Speed preset',
								opts.preset ?? vc.presets[0],
								vc.presets.map((p) => ({ value: p, label: p })),
								(v) => patch({ preset: v })
							)}
						{/if}
					{/if}

					{#if opts.video_codec !== 'gif'}
						{#if vc && opts.video_codec !== 'prores_ks'}
							{#if vc.tunes.length}
								{@render selectRow(
									'Tune',
									opts.tune ?? '',
									[{ value: '', label: 'None' }, ...vc.tunes.map((t) => ({ value: t, label: t }))],
									(v) => patch({ tune: v || null })
								)}
							{/if}
							{#if vc.profiles.length}
								{@render selectRow(
									'Profile',
									opts.profile_v ?? '',
									[{ value: '', label: 'Auto' }, ...vc.profiles.map((p) => ({ value: p, label: p }))],
									(v) => patch({ profile_v: v || null })
								)}
							{/if}
						{/if}
						{#if vc?.pixFmts.length}
							{@render selectRow(
								'Pixel format',
								opts.pix_fmt ?? vc.pixFmts[0],
								vc.pixFmts.map((p) => ({ value: p, label: p })),
								(v) => patch({ pix_fmt: v })
							)}
						{/if}
						{@render selectRow(
							'Scaler',
							opts.scaler ?? '',
							[{ value: '', label: 'Default (bicubic)' }, ...SCALERS.map((s) => ({ value: s, label: s }))],
							(v) => patch({ scaler: v || null })
						)}
						{@render toggleRow('GPU decode (hwaccel)', !!opts.hwaccel && opts.hwaccel !== 'none', (v) =>
							patch({ hwaccel: v ? 'auto' : null })
						)}
					{/if}
				{/if}

				<!-- audio -->
				{#if showAudio}
					{@render secHead('Audio')}
					{#if opts.include_audio}
						{@render selectRow(
							'Codec',
							opts.audio_codec ?? '',
							info.audio.map((id) => ({ value: id, label: AUDIO_CODECS[id]?.label ?? id })),
							(v) => patch({ audio_codec: v })
						)}
						{#if ac?.lossy}
							{@render selectRow(
								'Bitrate',
								opts.audio_bitrate ?? '192k',
								AUDIO_BITRATES.map((b) => ({ value: b, label: b })),
								(v) => patch({ audio_bitrate: v })
							)}
						{:else if opts.audio_codec === 'flac'}
							{@render sliderRow('Compression', opts.flac_compression ?? 5, { min: 0, max: 12, step: 1, digits: 0, neutral: 5 }, (v) =>
								patch({ flac_compression: Math.round(v) })
							)}
						{:else}
							<FormRow label="Bitrate">
								{#snippet control()}
									<span style="font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-muted)">Uncompressed / lossless — no bitrate.</span>
								{/snippet}
							</FormRow>
						{/if}
						{@render selectRow(
							'Sample rate',
							opts.audio_codec === 'libopus' ? '48000' : opts.audio_sample_rate ? String(opts.audio_sample_rate) : 'source',
							[{ value: 'source', label: 'Source' }, ...SAMPLE_RATES.map((r) => ({ value: String(r), label: `${r / 1000} kHz` }))],
							(v) => patch({ audio_sample_rate: v === 'source' ? null : parseInt(v) }),
							{ disabled: opts.audio_codec === 'libopus' }
						)}
						{@render selectRow(
							'Channels',
							opts.audio_channels ? String(opts.audio_channels) : 'source',
							[
								{ value: 'source', label: 'Source' },
								{ value: '1', label: 'Mono' },
								{ value: '2', label: 'Stereo' }
							],
							(v) => patch({ audio_channels: v === 'source' ? null : parseInt(v) })
						)}
					{/if}
				{/if}

				<!-- container options -->
				{@render secHead('Container')}
				{#if opts.container === 'gif'}
					{@render selectRow(
						'Dither',
						opts.gif_dither ?? 'bayer',
						GIF_DITHERS.map((d) => ({ value: d, label: d })),
						(v) => patch({ gif_dither: v })
					)}
					{@render toggleRow('Loop forever', opts.gif_loop, (v) => patch({ gif_loop: v }))}
				{/if}
				{#if info.faststart}
					{@render toggleRow('Fast start (web streaming)', opts.faststart, (v) => patch({ faststart: v }))}
				{/if}
				{@render textRow('Title', opts.metadata_title ?? '', 'optional', (v) => patch({ metadata_title: v || null }))}

				{#if hasCaptions || (chapterCount > 0 && carriesChapters(opts.container))}
					{@render secHead('Captions & chapters')}
					{#if hasCaptions}
						{#if carriesSubtitles(opts.container)}
							{@render toggleRow('Embed captions as a subtitle track', !!opts.embed_captions, (v) => patch({ embed_captions: v }))}
						{/if}
						{@render toggleRow(
							'Keep captions out of the picture',
							!!opts.skip_burned_captions,
							(v) => patch({ skip_burned_captions: v }),
							'Captions are drawn into the picture unless this is on. An embedded track is off until the viewer turns it on, so with both a player shows the captions once; keep them out of the picture to deliver them only as a track or a file.'
						)}
						{@render toggleRow('Write .srt beside the export', sidecar('srt'), (v) => toggleSidecar('srt', v))}
						{@render toggleRow('Write .vtt beside the export', sidecar('vtt'), (v) => toggleSidecar('vtt', v))}
					{/if}
					{#if chapterCount > 0 && carriesChapters(opts.container)}
						<FormRow label="Chapters">
							{#snippet control()}
								<span style="font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)">
									{chapterCount === 1 ? '1 chapter marker becomes' : `${chapterCount} chapter markers become`} the file's chapters.
								</span>
							{/snippet}
						</FormRow>
					{/if}
				{/if}

				<!-- command preview -->
				<InspectorSection title="ffmpeg command">
					<pre
						data-selectable
						style="margin:0;padding:8px 10px;background:var(--surface-void);border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-sm);font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-secondary);white-space:pre-wrap;word-break:break-all">{command}</pre>
				</InspectorSection>
			</InspectorSection>
		</div>

		{#if earlier.length}
			<InspectorSection title="Recent exports" summary={headline(earlier[0])}>
				<ExportOutcomes outcomes={earlier} />
			</InspectorSection>
		{/if}
	</ScrollArea>
	{/if}
	{#snippet footer()}
		{#if !result && issues.length}
			<div style="padding:8px 16px;display:flex;flex-direction:column;gap:4px;background:var(--danger-surface)">
				{#each issues as issue (issue)}
					<div style="display:flex;align-items:flex-start;gap:6px;font-size:var(--text-sm);line-height:1.45;color:var(--red-400)">
						<span style="flex:none;margin-top:2px"><Icon n="alert-triangle" s={13} color="var(--danger)" /></span>
						<span>{issue}</span>
					</div>
				{/each}
			</div>
		{/if}
		{#if refusal && !rendering}
			{@const reason = reasonLines(refusal.reason)}
			<!-- Bounded: the reason can be a dozen lines of ffmpeg's own output, and the form above must stay usable for a retry. -->
			<div role="alert" style="padding:8px 16px;display:flex;flex-direction:column;gap:6px;background:var(--danger-surface)">
				<div style="display:flex;align-items:flex-start;gap:8px">
					<span style="flex:none;margin-top:2px"><Icon n="circle-x" s={14} color="var(--danger)" /></span>
					<div style="flex:1;min-width:0;font-size:var(--text-sm);line-height:1.45;color:var(--red-400);overflow-wrap:anywhere">
						<strong style="font-weight:600">Export failed</strong>{reason.first ? ` — ${reason.first}` : ''}
					</div>
					{#if refusal.reason}
						<Btn variant="ghost" size="sm" icon="copy" onclick={() => copyText(refusal.reason, 'Reason')}>Copy</Btn>
					{/if}
					<Btn variant="ghost" size="sm" onclick={() => (putAway = refusal.id)}>Dismiss</Btn>
				</div>
				{#if reason.rest}
					<!-- A scrolling region has to take focus to be scrolled from the keyboard. -->
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<pre
						data-selectable
						tabindex="0"
						aria-label="Details of the failure"
						style="margin:0 0 0 22px;max-height:84px;overflow:auto;padding:4px 6px;border-radius:var(--radius-xs);background:var(--surface-void);font-family:var(--font-mono);font-size:var(--text-xs);line-height:1.4;color:var(--text-secondary);white-space:pre-wrap;overflow-wrap:anywhere">{reason.rest}</pre>
				{/if}
			</div>
		{/if}
		<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
			{#if rendering}
				<div style="flex:1;display:flex;align-items:center;gap:10px">
					<ProgressBar value={line.value} label="Exporting" />
					<span
						data-testid="export-progress-text"
						style="font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-muted);min-width:104px;text-align:right"
					>
						{line.text}
					</span>
				</div>
				<Btn variant="destructive" size="md" data-rescue disabled={cancelling} onclick={stop}>
					{cancelling ? 'Stopping…' : 'Stop'}
				</Btn>
			{:else if result}
				<div style="flex:1"></div>
				<Btn variant="ghost" size="md" onclick={() => (putAway = result.id)}>Export again</Btn>
				<Btn variant="primary" size="md" data-rescue onclick={onClose}>Done</Btn>
			{:else}
				<div style="flex:1"></div>
				<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
				<Btn variant="primary" size="md" icon="upload" data-rescue disabled={!canExport} onclick={doExport}>
					{variantFormats.length > 1 ? `Export ${variantFormats.length} files` : 'Export'}
				</Btn>
			{/if}
		</div>
	{/snippet}
</Modal>
