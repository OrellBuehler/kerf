<script lang="ts">
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { inTauri, pickExportPath } from '$lib/api';
	import { toast } from 'svelte-sonner';
	import type { Container, ExportOptions, RateControl } from '$lib/types';
	import {
		PRESETS,
		CONTAINERS,
		VIDEO_CODECS,
		AUDIO_CODECS,
		PRORES_PROFILES,
		RATE_CONTROLS,
		AUDIO_BITRATES,
		SAMPLE_RATES,
		SCALERS,
		GIF_DITHERS,
		RESOLUTIONS,
		FRAME_RATES,
		applyPreset,
		containerInfo,
		reconcileContainer,
		videoCodecDefaults,
		validateExport,
		buildSummary,
		buildCommandPreview
	} from '$lib/export-presets';

	let { onClose }: { onClose: () => void } = $props();

	let opts = $state<ExportOptions>(applyPreset('web_1080p'));
	let outputPath = $state('');
	let activePreset = $state('web_1080p');
	let customRes = $state(false);
	let rendering = $state(false);
	let showAdvanced = $state(false);
	let showCommand = $state(false);

	// Focus the dialog on open so its Escape handler (and the focus trap) actually
	// receive keys — otherwise focus stays on whatever triggered Export.
	let dialogEl = $state<HTMLDivElement | null>(null);
	$effect(() => {
		dialogEl?.focus();
	});

	const assets = $derived(editor.assets);
	const hasVideo = $derived(editor.timeline.tracks.some((t) => t.kind === 'video' && t.clips.length > 0));
	const hasAudio = $derived(
		editor.timeline.tracks.some((t) =>
			t.clips.some((c) => assets.find((a) => a.id === c.asset_id)?.streams.some((s) => s.kind === 'audio'))
		)
	);
	const info = $derived(containerInfo(opts.container));
	const vc = $derived(opts.video_codec ? VIDEO_CODECS[opts.video_codec] : undefined);
	const ac = $derived(opts.audio_codec ? AUDIO_CODECS[opts.audio_codec] : undefined);
	const issues = $derived(validateExport(opts, hasVideo, hasAudio));
	const summary = $derived(buildSummary(opts, hasVideo, hasAudio));
	const command = $derived(buildCommandPreview(opts, hasVideo, hasAudio, outputPath || undefined));
	const showVideo = $derived(!info.audioOnly && hasVideo);
	const showAudio = $derived(!info.videoOnly && hasAudio);
	const canExport = $derived(!editor.busy && !rendering && issues.length === 0 && (hasVideo || hasAudio));

	const selectCss =
		'background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:12px;padding:5px 7px;min-width:140px';
	const inputCss =
		'background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:5px 7px';

	function msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
	function patch(p: Partial<ExportOptions>) {
		opts = { ...opts, ...p };
		activePreset = 'custom';
	}
	function choosePreset(id: string) {
		opts = applyPreset(id);
		activePreset = id;
		customRes = false;
	}
	function swapExt(path: string, ext: string): string {
		return path.replace(/\.[^./\\]+$/, '') + '.' + ext;
	}
	function changeContainer(c: Container) {
		opts = reconcileContainer({ ...opts, container: c });
		activePreset = 'custom';
		customRes = false;
		if (outputPath) outputPath = swapExt(outputPath, containerInfo(c).ext);
	}
	function setVideoCodec(id: string) {
		patch(videoCodecDefaults(id));
	}
	function setRate(r: RateControl) {
		const p: Partial<ExportOptions> = { rate_control: r };
		if (r === 'crf' && opts.crf == null && vc?.crf) p.crf = vc.crf[2];
		if ((r === 'bitrate' || r === 'two_pass') && !opts.video_bitrate) p.video_bitrate = '8M';
		patch(p);
	}
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
		if (p) outputPath = p;
	}
	async function doExport() {
		if (!inTauri()) {
			toast.info('Export renders with FFmpeg in the desktop app.');
			return;
		}
		if (!outputPath) {
			await browse();
			if (!outputPath) return;
		}
		rendering = true;
		try {
			const out = await editor.export(outputPath, opts);
			toast.success(`Exported → ${out}`);
			onClose();
		} catch (e) {
			toast.error(msg(e));
		} finally {
			rendering = false;
		}
	}
</script>

{#snippet secHead(label: string)}
	<div style="display:flex;align-items:center;gap:8px;margin:16px 0 9px">
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>{label}</span
		>
		<div style="flex:1;height:1px;background:var(--border-subtle)"></div>
	</div>
{/snippet}

{#snippet selectRow(label: string, value: string, items: { value: string; label: string }[], onChange: (v: string) => void, disabled = false)}
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<select {value} {disabled} onchange={(e) => onChange(e.currentTarget.value)} style={selectCss}>
			{#each items as it (it.value)}
				<option value={it.value}>{it.label}</option>
			{/each}
		</select>
	</label>
{/snippet}

{#snippet toggleRow(label: string, checked: boolean, onChange: (v: boolean) => void)}
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<input
			type="checkbox"
			{checked}
			onchange={(e) => onChange(e.currentTarget.checked)}
			style="accent-color:var(--kerf-500);width:15px;height:15px"
		/>
	</label>
{/snippet}

{#snippet sliderRow(label: string, value: number, min: number, max: number, step: number, display: string, onChange: (v: number) => void)}
	<label style="display:flex;align-items:center;gap:10px;padding:4px 0">
		<span style="font-size:12px;color:var(--text-muted);width:90px;flex:none">{label}</span>
		<input
			type="range"
			{min}
			{max}
			{step}
			{value}
			oninput={(e) => onChange(parseFloat(e.currentTarget.value))}
			style="flex:1;accent-color:var(--kerf-500)"
		/>
		<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:54px;text-align:right">{display}</span>
	</label>
{/snippet}

{#snippet textRow(label: string, value: string, placeholder: string, onChange: (v: string) => void)}
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<input
			type="text"
			{value}
			{placeholder}
			onchange={(e) => onChange(e.currentTarget.value)}
			style="{inputCss};width:150px;text-align:left"
		/>
	</label>
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	bind:this={dialogEl}
	role="dialog"
	aria-modal="true"
	tabindex="-1"
	onclick={onClose}
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:rgba(0,0,0,.55);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:560px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:1px solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg,0 24px 60px rgba(0,0,0,.5));overflow:hidden"
	>
		<!-- header -->
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:1px solid var(--border-default)"
		>
			<Icon n="upload" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">Export</span>
			<Btn variant="ghost" size="sm" onclick={onClose}>✕</Btn>
		</div>

		<div style="flex:1;overflow-y:auto;padding:12px 16px">
			<!-- preset chips -->
			<div style="display:flex;flex-wrap:wrap;gap:6px">
				{#each PRESETS as p (p.id)}
					<button
						title={p.description}
						onclick={() => choosePreset(p.id)}
						style="padding:5px 10px;border-radius:999px;font-size:12px;cursor:pointer;white-space:nowrap;border:1px solid {activePreset ===
						p.id
							? 'var(--kerf-500)'
							: 'var(--border-strong)'};background:{activePreset === p.id
							? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)'
							: 'var(--surface-inset)'};color:{activePreset === p.id ? 'var(--text-primary)' : 'var(--text-secondary)'}"
						>{p.label}</button
					>
				{/each}
			</div>

			<!-- summary -->
			<div
				data-selectable
					style="margin-top:12px;padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:1px solid var(--border-subtle);font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
			>
				{summary}
			</div>

			<!-- destination -->
			{@render secHead('Destination')}
			{@render selectRow(
				'Container',
				opts.container,
				CONTAINERS.map((c) => ({ value: c.id, label: `${c.label} (.${c.ext})` })),
				(v) => changeContainer(v as Container)
			)}
			<div style="display:flex;align-items:center;gap:8px;padding:4px 0">
				<input
					type="text"
					readonly
					value={outputPath}
					placeholder="Choose a file…"
					style="{inputCss};flex:1;text-align:left;color:var(--text-secondary)"
				/>
				<Btn variant="secondary" size="sm" icon="folder-open" onclick={browse}>Browse</Btn>
			</div>

			<!-- video -->
			{#if showVideo}
				{@render secHead('Video')}
				{@render selectRow(
					'Codec',
					opts.video_codec ?? '',
					info.video.map((id) => ({ value: id, label: VIDEO_CODECS[id]?.label ?? id })),
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
					<div style="font-size:12px;color:var(--text-muted);padding:4px 0">Quality is set by the palette and the resolution / fps below.</div>
				{:else}
					<!-- rate control segmented -->
					<div style="display:flex;align-items:center;gap:8px;padding:4px 0">
						<span style="font-size:12px;color:var(--text-muted);width:90px;flex:none">Rate control</span>
						<div style="display:flex;gap:4px;flex:1">
							{#each RATE_CONTROLS as rc (rc.id)}
								<button
									onclick={() => setRate(rc.id)}
									style="flex:1;padding:5px 4px;font-size:11px;cursor:pointer;border-radius:var(--radius-sm);border:1px solid {opts.rate_control ===
									rc.id
										? 'var(--kerf-500)'
										: 'var(--border-strong)'};background:{opts.rate_control === rc.id
										? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)'
										: 'var(--surface-inset)'};color:{opts.rate_control === rc.id ? 'var(--text-primary)' : 'var(--text-secondary)'}"
									>{rc.label}</button
								>
							{/each}
						</div>
					</div>

					{#if opts.rate_control === 'crf' && vc?.crf}
						{@render sliderRow('Quality (CRF)', opts.crf ?? vc.crf[2], vc.crf[0], vc.crf[1], 1, String(opts.crf ?? vc.crf[2]), (v) =>
							patch({ crf: Math.round(v) })
						)}
						<div style="font-size:11px;color:var(--text-disabled);padding:0 0 2px 100px">lower = higher quality &amp; larger file</div>
					{:else if opts.rate_control === 'bitrate' || opts.rate_control === 'two_pass'}
						{@render textRow('Target bitrate', opts.video_bitrate ?? '', 'e.g. 8M', (v) => patch({ video_bitrate: v }))}
						{#if opts.rate_control === 'two_pass'}
							<div style="font-size:11px;color:var(--text-disabled);padding:0 0 2px 0">Two passes ≈ double the render time.</div>
						{/if}
					{:else if opts.rate_control === 'lossless'}
						<div style="font-size:12px;color:var(--text-muted);padding:4px 0">Mathematically lossless — very large files.</div>
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

				<!-- advanced video -->
				<button
					onclick={() => (showAdvanced = !showAdvanced)}
					style="margin-top:8px;background:none;border:none;color:var(--text-secondary);font-size:12px;cursor:pointer;padding:2px 0"
					>{showAdvanced ? '▾' : '▸'} Advanced video</button
				>
				{#if showAdvanced && opts.video_codec !== 'gif'}
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
				{/if}

				{@render secHead('Scaling')}
				{@render selectRow(
					'Resolution',
					resValue(),
					[
						...RESOLUTIONS.map((r) => ({ value: r.value ? `${r.value[0]}x${r.value[1]}` : 'source', label: r.label })),
						{ value: 'custom', label: 'Custom…' }
					],
					setRes
				)}
				{#if customRes}
					<div style="display:flex;align-items:center;gap:8px;padding:4px 0;justify-content:flex-end">
						<input
							type="number"
							min="2"
							step="2"
							value={opts.resolution?.[0] ?? 1920}
							onchange={(e) => patch({ resolution: [parseInt(e.currentTarget.value) || 1920, opts.resolution?.[1] ?? 1080] })}
							style="{inputCss};width:90px;text-align:right"
						/>
						<span style="color:var(--text-muted)">×</span>
						<input
							type="number"
							min="2"
							step="2"
							value={opts.resolution?.[1] ?? 1080}
							onchange={(e) => patch({ resolution: [opts.resolution?.[0] ?? 1920, parseInt(e.currentTarget.value) || 1080] })}
							style="{inputCss};width:90px;text-align:right"
						/>
					</div>
				{/if}
				{@render selectRow(
					'Frame rate',
					opts.fps ? String(opts.fps) : 'source',
					FRAME_RATES.map((f) => ({ value: f.value ? String(f.value) : 'source', label: f.label })),
					(v) => patch({ fps: v === 'source' ? null : parseFloat(v) })
				)}
			{/if}

			<!-- audio -->
			{#if showAudio}
				{@render secHead('Audio')}
				{@render toggleRow('Strip audio', !opts.include_audio, (v) => patch({ include_audio: !v }))}
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
						{@render sliderRow('Compression', opts.flac_compression ?? 5, 0, 12, 1, String(opts.flac_compression ?? 5), (v) =>
							patch({ flac_compression: Math.round(v) })
						)}
					{:else}
						<div style="font-size:12px;color:var(--text-muted);padding:4px 0">Uncompressed / lossless — no bitrate.</div>
					{/if}
					{@render selectRow(
						'Sample rate',
						opts.audio_codec === 'libopus' ? '48000' : opts.audio_sample_rate ? String(opts.audio_sample_rate) : 'source',
						[{ value: 'source', label: 'Source' }, ...SAMPLE_RATES.map((r) => ({ value: String(r), label: `${r / 1000} kHz` }))],
						(v) => patch({ audio_sample_rate: v === 'source' ? null : parseInt(v) }),
						opts.audio_codec === 'libopus'
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

			<!-- command preview -->
			<button
				onclick={() => (showCommand = !showCommand)}
				style="margin-top:12px;background:none;border:none;color:var(--text-secondary);font-size:12px;cursor:pointer;padding:2px 0"
				>{showCommand ? '▾' : '▸'} Show ffmpeg command</button
			>
			{#if showCommand}
				<pre
					data-selectable
					style="margin:6px 0 0;padding:8px 10px;background:var(--surface-void);border:1px solid var(--border-subtle);border-radius:var(--radius-sm);font-family:var(--font-mono);font-size:11px;color:var(--text-secondary);white-space:pre-wrap;word-break:break-all">{command}</pre>
			{/if}
		</div>

		<!-- footer -->
		<div style="flex:none;border-top:1px solid var(--border-default)">
			{#if issues.length}
				<div style="padding:8px 16px;display:flex;flex-direction:column;gap:3px;background:color-mix(in srgb,var(--red-600) 14%,transparent)">
					{#each issues as issue (issue)}
						<div style="font-size:12px;color:var(--red-400)">⚠ {issue}</div>
					{/each}
				</div>
			{/if}
			<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
				<div style="flex:1"></div>
				<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
				<Btn variant="primary" size="md" icon={rendering ? 'loader' : 'upload'} disabled={!canExport} onclick={doExport}>
					{rendering ? 'Rendering…' : 'Export'}
				</Btn>
			</div>
		</div>
	</div>
</div>
