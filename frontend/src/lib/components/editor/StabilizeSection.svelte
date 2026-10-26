<script lang="ts">
	// A clip's stabilization: the switch, its strength and zoom, and where its analysis stands. Switching it on is
	// one edit; the camera is then measured in the background (the app's queue, behind exports and proxies), and
	// the clip plays as shot until that lands. The panel starts the analysis itself for a clip nothing measured
	// (`wantsAnalysis`), once per clip and window, and leaves a failure to the Analyze button. Each slider writes
	// one edit on release (`SliderRow`). The preview shows the clip through FFmpeg (the GPU preview refuses a
	// stabilized clip), so what is drawn is what the export renders; it is drawn again when an analysis lands.
	import InspectorSection from './InspectorSection.svelte';
	import SliderRow from './SliderRow.svelte';
	import CheckRow from './CheckRow.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { isCancelled, report } from '#lib/ops';
	import { analyzeClipStabilization, cancelStabilization, clipStabilizationStatus, onStabilizeProgress } from '#lib/api';
	import { PERCENT } from '#lib/slider-row';
	import {
		canStabilize,
		DEFAULT_STABILIZE,
		forgetStopped,
		noteStopped,
		stabilizeSummary,
		statusDetail,
		statusLabel,
		wantsAnalysis
	} from '#lib/stabilize';
	import { MAX_STABILIZE_ZOOM } from '#lib/types';
	import type { Asset, Clip, Stabilize, StabilizeStatus, Track } from '#lib/types';

	let { clip, track, asset }: { clip: Clip; track: Track | undefined; asset: Asset | undefined } = $props();

	const offered = $derived(canStabilize(clip, track, asset));
	const setting = $derived(clip.stabilize ?? null);
	let status = $state<StabilizeStatus | null>(null);
	const label = $derived(statusLabel(status));
	const running = $derived(status?.state === 'analyzing' || status?.state === 'queued');
	/** What the panel has asked to analyse (clip, setting and window): each once. */
	const asked = new Set<string>();

	/** The stabilization of the clip `id` as the timeline has it now (the selection may have moved since a gesture began). */
	function settingOf(id: string): Stabilize {
		for (const t of editor.timeline.tracks) {
			const c = t.clips.find((x) => x.id === id);
			if (c?.stabilize) return c.stabilize;
		}
		return DEFAULT_STABILIZE;
	}

	async function write(id: string, next: Stabilize | null) {
		try {
			await editor.setClipStabilize(id, next);
		} catch (e) {
			report(e);
		}
	}

	async function refresh(id: string) {
		try {
			const [s] = await clipStabilizationStatus([id]);
			if (id === clip.id) status = s ?? null;
		} catch {
			if (id === clip.id) status = null;
		}
	}

	async function analyze(id: string) {
		try {
			const s = await analyzeClipStabilization(id);
			// The frame on screen was drawn before the clip could be steadied: draw it again.
			if (s.state === 'ready' && status?.state !== 'ready') ui.refreshPreview();
			if (id === clip.id) status = s;
		} catch (e) {
			// A stopped analysis stays stopped: the panel does not start it again when the clip is trimmed or reselected.
			if (isCancelled(e)) noteStopped(id);
			else report(e);
			await refresh(id);
		}
	}

	/** The Analyze button: the user asks again, a stop before included. */
	function analyzeAgain(id: string) {
		forgetStopped(id);
		void analyze(id);
	}

	// Where the clip stands, asked whenever what decides it changes: the clip, its setting, its window.
	$effect(() => {
		const id = clip.id;
		const key = `${id}|${JSON.stringify(clip.stabilize ?? null)}|${clip.asset_id}|${clip.source_in}|${clip.source_out}`;
		if (!offered) {
			status = null;
			return;
		}
		let live = true;
		clipStabilizationStatus([id])
			.then(([s]) => {
				if (!live) return;
				status = s ?? null;
				if (s && wantsAnalysis(s, id) && !asked.has(key)) {
					asked.add(key);
					void analyze(id);
				}
			})
			.catch(() => {
				if (live) status = null;
			});
		return () => {
			live = false;
		};
	});

	// The analysis of this clip as it moves (the app's events; the browser harness has none).
	$effect(() => {
		const id = clip.id;
		let gone = false;
		let unlisten: (() => void) | undefined;
		void onStabilizeProgress((p) => {
			if (p.clip_id !== id) return;
			// An analysis that landed (an agent's too) changes the picture of a cut that did not change.
			if (p.state === 'ready' && status?.state !== 'ready') ui.refreshPreview();
			status = p;
		}).then((u) => {
			if (gone) u();
			else unlisten = u;
		});
		return () => {
			gone = true;
			unlisten?.();
		};
	});
</script>

{#if offered}
	<InspectorSection title="Stabilize" summary={stabilizeSummary(clip, status)}>
		<CheckRow
			checked={!!setting}
			pending={editor.locked}
			onchange={(e) => write(clip.id, e.currentTarget.checked ? settingOf(clip.id) : null)}
		>
			Take out the camera's shake
		</CheckRow>
		{#if setting}
			<SliderRow
				label="Strength"
				value={setting.strength}
				spec={{ min: 0, max: 1, step: 0.05, neutral: DEFAULT_STABILIZE.strength, ...PERCENT }}
				scope={clip.id}
				disabled={editor.locked}
				onCommit={(v, id) => write(id, { ...settingOf(id), strength: v })}
			/>
			<CheckRow
				checked={setting.zoom == null}
				pending={editor.locked}
				onchange={(e) => write(clip.id, { ...settingOf(clip.id), zoom: e.currentTarget.checked ? null : 5 })}
			>
				Zoom just enough to hide the edges
			</CheckRow>
			{#if setting.zoom != null}
				<SliderRow
					label="Zoom"
					value={setting.zoom}
					spec={{ min: 0, max: MAX_STABILIZE_ZOOM, step: 0.5, neutral: 5, digits: 1, unit: '%' }}
					scope={clip.id}
					disabled={editor.locked}
					onCommit={(v, id) => write(id, { ...settingOf(id), zoom: v })}
				/>
			{/if}
			<div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin:6px 0 4px">
				<Badge tone={label.tone} dot={running}>{label.text}</Badge>
				{#if running}
					<Btn size="sm" variant="secondary" title={'Stop measuring the camera'} onclick={() => {
						noteStopped(clip.id);
						void cancelStabilization();
					}}>Stop</Btn>
				{:else if status?.state === 'missing' || status?.state === 'failed'}
					<Btn size="sm" variant="secondary" pending={editor.locked} title={"Measure this clip's camera"} onclick={() => analyzeAgain(clip.id)}
						>Analyze</Btn
					>
				{/if}
			</div>
			{#if statusDetail(status)}
				<div style="font:var(--type-caption);color:var(--text-muted);margin:2px 0 6px">{statusDetail(status)}</div>
			{/if}
		{/if}
	</InspectorSection>
{/if}
