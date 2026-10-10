<script lang="ts">
	// Fit a music clip to the picture. The engine plans an arrangement of whole phrases on bar lines
	// (`plan_music_fit`) — intro and ending kept, phrases repeated or dropped between — and this
	// shows it before it is made: target against achieved length, the splices it takes and what is
	// left over. Apply writes it as clips on the original file (`fit_music`), one revision.
	import { tick } from 'svelte';
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { trapFocus } from '$lib/modal';
	import { editor } from '$lib/state.svelte';
	import { planMusicFit } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { formatTime } from '$lib/diff';
	import { describeFit, describeRemainder, fitVerdict, musicSummary, parseTarget } from '$lib/music-ui';
	import type { MusicFit } from '$lib/types';

	let { clipId, onClose }: { clipId: string; onClose: () => void } = $props();

	const clip = $derived(editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === clipId));
	const music = $derived(clip ? editor.analysisFor(clip.asset_id)?.music : undefined);

	/** Fit to the picture (the engine's default) or to a length typed here. */
	let custom = $state(false);
	let customText = $state('');
	let fadeOut = $state(true);
	let lengthEl = $state<HTMLInputElement | null>(null);

	let fit = $state<MusicFit | null>(null);
	/** Why there is no plan: a backend refusal, or a length that is not a number. */
	let problem = $state<string | null>(null);
	let applying = $state(false);

	// The clip going away under the dialog (an undo, an agent's edit) ends it.
	$effect(() => {
		if (!clip) onClose();
	});

	// Plan on open and whenever the question changes — newest answer wins, since a plan that
	// takes a moment must not land on top of the one asked for after it. A refusal is shown here
	// *and* noticed once, so the log keeps what the dialog only shows while it is open.
	let seq = 0;
	let noticed: string | null = null;
	$effect(() => {
		const id = clipId;
		const asked = custom ? parseTarget(customText) : undefined;
		void editor.timeline; // the cut changed under the dialog: plan again
		if (!clip) return; // gone — an applied fit replaces it — and the effect above closes the dialog
		if (asked === null) {
			fit = null;
			problem = 'Enter a length in seconds';
			return;
		}
		const mine = ++seq;
		planMusicFit(id, asked).then(
			(plan) => {
				if (mine !== seq) return;
				fit = plan;
				problem = null;
				noticed = null;
			},
			(e) => {
				if (mine !== seq) return;
				fit = null;
				problem = e instanceof Error ? e.message : String(e);
				if (problem !== noticed) toast.error(problem);
				noticed = problem;
			}
		);
	});

	function chooseCustom() {
		if (custom) return;
		// Start from the length it is fitted to now, so the box reads as an adjustment of it.
		if (fit && customText === '') customText = String(Math.round(fit.target * 10) / 10);
		custom = true;
		// Ready to type over it; the field only exists once `custom` has rendered.
		void tick().then(() => lengthEl?.select());
	}

	const verdict = $derived(fit ? fitVerdict(fit) : null);
	/** Fading only means something for an arrangement that runs past its target. */
	const overruns = $derived(verdict === 'over');
	const canApply = $derived(!!fit && !applying && !editor.busy);

	async function apply() {
		if (!canApply) return;
		applying = true;
		try {
			const report = await editor.fitMusic(clipId, custom ? parseTarget(customText) : null, fadeOut);
			toast.success(describeFit(report), { action: { label: 'Undo', onClick: () => void editor.undo() } });
			onClose();
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		} finally {
			applying = false;
		}
	}

	const chip = (active: boolean) =>
		`padding:4px 9px;font-size:12px;cursor:pointer;border-radius:var(--radius-sm);border:var(--line-width) solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
			active ? 'var(--text-primary)' : 'var(--text-secondary)'
		}`;
	const labelCss = 'font-size:12px;color:var(--text-muted)';
	const inputCss =
		'background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:13px;min-height:30px;padding:4px 7px;width:90px;text-align:right';
	const valueCss = 'font-family:var(--font-mono);font-size:12px;color:var(--text-primary)';
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	use:trapFocus
	role="dialog"
	aria-modal="true"
	aria-label="Fit music to video"
	tabindex="-1"
	onclick={() => !applying && onClose()}
	onkeydown={(e) => {
		if (e.key === 'Escape' && !applying) onClose();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:color-mix(in srgb,var(--scrim) 55%,transparent);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:460px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg);overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:var(--line-width) solid var(--border-default)"
		>
			<Icon n="music" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">Fit music to video</span>
			<Btn variant="ghost" size="sm" aria-label="Close" onclick={onClose}>✕</Btn>
		</div>

		<fieldset
			disabled={applying}
			style="flex:1;min-height:0;min-width:0;margin:0;border:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px"
		>
			<div style="display:flex;flex-direction:column;gap:2px">
				<span
					style="font-size:13px;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					title={clip ? editor.assetName(clip.asset_id) : ''}>{clip ? editor.assetName(clip.asset_id) : ''}</span
				>
				{#if music}
					<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{musicSummary(music)}</span>
				{/if}
			</div>

			<div style="display:flex;align-items:center;gap:5px;padding:4px 0">
				<span style="{labelCss};width:70px;flex:none">Length</span>
				<button type="button" style={chip(!custom)} aria-pressed={!custom} onclick={() => (custom = false)}>
					To the picture
				</button>
				<button type="button" style={chip(custom)} aria-pressed={custom} onclick={chooseCustom}>Custom</button>
				{#if custom}
					<input
						bind:this={lengthEl}
						type="text"
						inputmode="decimal"
						aria-label="Length in seconds"
						bind:value={customText}
						onkeydown={(e) => {
							if (e.key === 'Enter') {
								e.preventDefault();
								void apply();
							}
						}}
						style={inputCss}
					/>
					<span style={labelCss}>s</span>
				{/if}
			</div>

			<div
				aria-live="polite"
				style="display:grid;grid-template-columns:1fr auto;gap:5px 12px;padding:10px 12px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);min-height:150px;align-content:start"
			>
				{#if fit}
					<span style={labelCss}>Target</span>
					<span style={valueCss}>{formatTime(fit.target)}</span>
					<span style={labelCss}>Fitted</span>
					<span style={valueCss}>{formatTime(fit.duration)}</span>
					<span style={labelCss}>Splices</span>
					<span style={valueCss}>{fit.splices}</span>
					<span style={labelCss}>Bars between intro and ending</span>
					<span style={valueCss}>{fit.bars}</span>
					<span
						data-verdict={verdict}
						style="grid-column:1 / -1;margin-top:3px;font-size:12px;line-height:1.4;color:{verdict === 'exact'
							? 'var(--success)'
							: 'var(--warning)'}">{describeRemainder(fit, fadeOut)}</span
					>
				{:else if problem}
					<span style="grid-column:1 / -1;font-size:12px;line-height:1.4;color:var(--text-muted)">{problem}</span>
				{:else}
					<span style="grid-column:1 / -1;font-size:12px;color:var(--text-disabled)">Planning…</span>
				{/if}
			</div>

			<label
				style="display:flex;align-items:center;gap:8px;padding:2px 0;cursor:{overruns ? 'pointer' : 'default'};opacity:{overruns
					? 1
					: 0.6}"
				title={overruns
					? 'Cut the music at the target and fade it out over its last two seconds.'
					: 'Only matters when the arrangement runs past the target.'}
			>
				<input
					type="checkbox"
					bind:checked={fadeOut}
					disabled={!overruns}
					style="accent-color:var(--kerf-500);width:15px;height:15px"
				/>
				<span style="font-size:12px;color:var(--text-secondary)">Fade out at the end</span>
				{#if fit && !overruns}
					<span style="font-size:11px;color:var(--text-disabled)">— nothing runs over</span>
				{/if}
			</label>

			<p style="margin:0;font-size:11px;line-height:1.5;color:var(--text-muted)">
				Keeps the intro and the ending and repeats or drops whole phrases between, splicing on bar lines with a 10 ms
				crossfade. The clip is replaced by clips on the original file; undo puts it back.
			</p>
		</fieldset>

		<div style="flex:none;border-top:var(--line-width) solid var(--border-default);padding:12px 16px">
			<div style="display:flex;align-items:center;gap:8px">
				<div style="flex:1"></div>
				<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
				<Btn variant="primary" size="md" icon="music" disabled={!canApply} onclick={apply}>Fit music</Btn>
			</div>
		</div>
	</div>
</div>
