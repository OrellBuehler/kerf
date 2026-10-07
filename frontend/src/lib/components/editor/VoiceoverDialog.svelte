<script lang="ts">
	// Write a script, pick a voice, and Kerf speaks it onto the VO track — captions
	// and all. The first use downloads the voice model, so the dialog says so up
	// front and then names each stage as it runs.
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { cancelVoiceover, onVoiceoverProgress, voiceoverStatus } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { CAPTION_LOOKS } from '$lib/style-presets';
	import { fmtDuration } from '$lib/media-info';
	import type { CaptionStyle, VoiceoverProgress, VoiceoverStatus } from '$lib/types';
	import {
		MAX_SPEED,
		MIN_SPEED,
		approxMB,
		clampSpeed,
		defaultCaptionStyle,
		estimateSeconds,
		fmtSpeed,
		isVoiceoverCancelled,
		loadPrefs,
		savePrefs,
		stageLabel,
		wordCount,
		type VoiceoverPrefill
	} from '$lib/voiceover';

	let { onClose, prefill = null }: { onClose: () => void; prefill?: VoiceoverPrefill | null } = $props();

	const prefs = loadPrefs();
	// The dialog opens once with what it is given; a later change is not its business.
	const start = untrack(() => prefill);

	let text = $state(start?.text ?? '');
	let voice = $state(start?.voice ?? prefs.voice);
	let speed = $state(clampSpeed(start?.speed ?? prefs.speed));
	let place = $state<'playhead' | 'append'>('playhead');
	let caption = $state(prefs.caption);
	/** What the user picked; until they do, the project's frame decides. */
	let styleChoice = $state<CaptionStyle | null>(prefs.captionStyle);
	const captionStyle = $derived(styleChoice ?? defaultCaptionStyle(editor.timeline.format));

	let status = $state<VoiceoverStatus | null>(null);
	let statusError = $state<string | null>(null);
	let generating = $state(false);
	let cancelling = $state(false);
	let progress = $state<VoiceoverProgress | null>(null);

	let dialogEl = $state<HTMLDivElement | null>(null);
	let textEl = $state<HTMLTextAreaElement | null>(null);
	$effect(() => {
		(textEl ?? dialogEl)?.focus();
	});

	$effect(() => {
		voiceoverStatus().then(
			(s) => {
				status = s;
				// A remembered voice the backend no longer lists would leave the select blank.
				if (!s.voices.some((v) => v.id === voice)) voice = s.default_voice;
			},
			(e) => (statusError = e instanceof Error ? e.message : String(e))
		);
	});

	const words = $derived(wordCount(text));
	const estimate = $derived(estimateSeconds(text, speed));
	const american = $derived(status?.voices.filter((v) => v.accent === 'us') ?? []);
	const british = $derived(status?.voices.filter((v) => v.accent === 'gb') ?? []);
	const unavailable = $derived(status ? !status.available : statusError !== null);
	const canGenerate = $derived(!!status?.available && words > 0 && !generating);
	const pct = $derived(progress?.fraction == null ? null : Math.round(progress.fraction * 100));

	function voiceLabel(v: { name: string; gender: string; downloaded: boolean }): string {
		const fetched = status?.ready && !v.downloaded ? ' · not downloaded' : '';
		return `${v.name} · ${v.gender}${fetched}`;
	}

	async function generate() {
		if (!canGenerate) return;
		savePrefs({ voice, speed, caption, captionStyle: styleChoice });
		generating = true;
		cancelling = false;
		progress = null;
		const unlisten = await onVoiceoverProgress((p) => {
			progress = p;
		});
		try {
			await editor.generateVoiceover({
				text: text.trim(),
				voice,
				speed,
				timelineStart: place === 'playhead' ? ui.time : undefined,
				captions: caption ? { style: captionStyle } : undefined
			});
			toast.success(caption ? 'Voiceover added and captioned' : 'Voiceover added');
			onClose();
		} catch (e) {
			// A stop is the user's own doing — back to the form, no notice.
			if (!isVoiceoverCancelled(e)) toast.error(e instanceof Error ? e.message : String(e));
		} finally {
			unlisten();
			generating = false;
			cancelling = false;
			progress = null;
		}
	}

	async function stop() {
		cancelling = true;
		await cancelVoiceover();
	}

	const chip = (active: boolean) =>
		`padding:4px 9px;font-size:12px;cursor:pointer;border-radius:var(--radius-sm);border:1px solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
			active ? 'var(--text-primary)' : 'var(--text-secondary)'
		}`;
	const selectCss =
		'background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:13px;min-height:32px;padding:5px 7px;min-width:220px;max-width:100%';
	const labelCss = 'font-size:12px;color:var(--text-muted)';
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	bind:this={dialogEl}
	role="dialog"
	aria-modal="true"
	aria-label="Voiceover"
	tabindex="-1"
	onclick={() => !generating && onClose()}
	onkeydown={(e) => {
		if (e.key === 'Escape' && !generating) onClose();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:color-mix(in srgb,var(--scrim) 55%,transparent);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:560px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:1px solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg);overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:1px solid var(--border-default)"
		>
			<Icon n="mic" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">Voiceover</span>
			{#if !generating}
				<Btn variant="ghost" size="sm" onclick={onClose}>✕</Btn>
			{/if}
		</div>

		<fieldset
			disabled={generating}
			style="flex:1;min-height:0;min-width:0;margin:0;border:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px"
		>
			{#if unavailable}
				<div
					style="font-size:12px;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:8px 10px"
				>
					⚠ {status?.reason ?? statusError ?? 'Voiceover is not available.'}
				</div>
			{:else if status && !status.ready}
				<div
					style="font-size:12px;color:var(--text-secondary);background:var(--surface-inset);border-radius:var(--radius-sm);padding:8px 10px"
				>
					The first voiceover downloads the voice model ({approxMB(status.approx_download_bytes)}).
				</div>
			{/if}

			<div style="display:flex;flex-direction:column;gap:5px">
				<span style={labelCss}>Script</span>
				<textarea
					bind:this={textEl}
					bind:value={text}
					rows="7"
					placeholder="Write what the voice should say. A blank line makes a longer pause."
					onkeydown={(e) => {
						if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
							e.preventDefault();
							void generate();
						}
					}}
					style="resize:vertical;min-height:110px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-sans);font-size:13px;line-height:1.5;padding:8px 10px"
				></textarea>
				<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">
					{words} {words === 1 ? 'word' : 'words'}{#if words > 0}
						&nbsp;·&nbsp;about {fmtDuration(estimate)} (estimate){/if}
				</span>
			</div>

			<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 0">
				<span style={labelCss}>Voice</span>
				<select bind:value={voice} disabled={!status} style={selectCss}>
					{#if status}
						<optgroup label="American">
							{#each american as v (v.id)}
								<option value={v.id}>{voiceLabel(v)}</option>
							{/each}
						</optgroup>
						<optgroup label="British">
							{#each british as v (v.id)}
								<option value={v.id}>{voiceLabel(v)}</option>
							{/each}
						</optgroup>
					{/if}
				</select>
			</label>

			<label style="display:flex;align-items:center;gap:10px;padding:4px 0">
				<span style="{labelCss};width:90px;flex:none">Speed</span>
				<input
					type="range"
					min={MIN_SPEED}
					max={MAX_SPEED}
					step="0.05"
					value={speed}
					oninput={(e) => (speed = parseFloat(e.currentTarget.value))}
					ondblclick={() => (speed = 1)}
					title="Double-click to reset"
					style="flex:1;accent-color:var(--kerf-500)"
				/>
				<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:54px;text-align:right"
					>{fmtSpeed(speed)}</span
				>
			</label>

			<div style="display:flex;align-items:center;gap:5px;padding:4px 0">
				<span style="{labelCss};width:90px;flex:none">Place</span>
				<button type="button" style={chip(place === 'playhead')} onclick={() => (place = 'playhead')}>
					At playhead ({fmtDuration(ui.time)})
				</button>
				<button type="button" style={chip(place === 'append')} onclick={() => (place = 'append')}>
					Append to VO track
				</button>
			</div>

			<div style="display:flex;align-items:center;gap:5px;padding:4px 0">
				<label style="display:flex;align-items:center;gap:8px;width:90px;flex:none;cursor:pointer">
					<input type="checkbox" bind:checked={caption} style="accent-color:var(--kerf-500);width:15px;height:15px" />
					<span style={labelCss}>Caption it</span>
				</label>
				{#if caption}
					{#each CAPTION_LOOKS as c (c.id)}
						<button type="button" style={chip(captionStyle === c.id)} title={c.hint} onclick={() => (styleChoice = c.id)}>
							{c.label}
						</button>
					{/each}
				{:else}
					<span style="font-size:12px;color:var(--text-disabled)">No captions are added.</span>
				{/if}
			</div>
		</fieldset>

		<div style="flex:none;border-top:1px solid var(--border-default);padding:12px 16px">
			{#if generating}
				<div style="display:flex;align-items:center;gap:10px">
					<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:6px">
						<div style="display:flex;align-items:baseline;gap:8px;font-size:12px">
							<span style="color:var(--text-primary)">{progress ? stageLabel(progress.stage) : 'Starting…'}</span>
							<span
								style="flex:1;min-width:0;font-family:var(--font-mono);font-size:11px;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
								>{progress?.detail ?? ''}</span
							>
							{#if pct !== null}
								<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{pct}%</span>
							{/if}
						</div>
						<div style="height:6px;border-radius:3px;background:var(--surface-inset);overflow:hidden">
							<div
								style="height:100%;width:{pct ?? 30}%;background:var(--kerf-500);transition:width var(--dur-fast) linear"
							></div>
						</div>
					</div>
					<Btn variant="destructive" size="md" disabled={cancelling} onclick={stop}>
						{cancelling ? 'Stopping…' : 'Cancel'}
					</Btn>
				</div>
			{:else}
				<div style="display:flex;align-items:center;gap:8px">
					<div style="flex:1"></div>
					<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
					<Btn variant="primary" size="md" icon="mic" disabled={!canGenerate} onclick={generate}>Generate</Btn>
				</div>
			{/if}
		</div>
	</div>
</div>
