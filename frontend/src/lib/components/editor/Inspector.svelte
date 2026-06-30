<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { clipDuration, DEFAULT_COLOR, DEFAULT_TRANSFORM } from '$lib/types';
	import type { AudioEffect, Transform, TransitionKind, VideoEffect } from '$lib/types';
	import { toast } from 'svelte-sonner';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const kind = $derived(asset?.streams.some((s) => s.kind === 'video') ? 'video' : 'audio');
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	const track = $derived(
		clip ? editor.timeline.tracks.find((t) => t.clips.some((c) => c.id === clip.id)) : undefined
	);
	// While the clip is animated the Transform panel shows the *sampled* pose at
	// the playhead (so the sliders track the motion) and editing a channel adds a
	// keyframe there; otherwise it edits the static transform.
	function lerp(points: [number, number][], at: number): number | undefined {
		if (points.length === 0) return undefined;
		if (points.length === 1) return points[0][1];
		if (at <= points[0][0]) return points[0][1];
		for (let i = 0; i < points.length - 1; i++) {
			const [t0, v0] = points[i];
			const [t1, v1] = points[i + 1];
			if (at < t1) return t1 <= t0 ? v0 : v0 + ((v1 - v0) * (at - t0)) / (t1 - t0);
		}
		return points[points.length - 1][1];
	}
	const tf = $derived.by(() => {
		const base = { ...DEFAULT_TRANSFORM, ...(clip?.transform ?? {}) };
		if (clip && keyframes.length) {
			const lt = Math.max(0, ui.time - clip.timeline_start);
			const ks = [...keyframes].sort((a, b) => a.time - b.time);
			base.scale = lerp(ks.map((k) => [k.time, k.scale]), lt) ?? base.scale;
			base.pos_x = lerp(ks.map((k) => [k.time, k.pos_x]), lt) ?? base.pos_x;
			base.pos_y = lerp(ks.map((k) => [k.time, k.pos_y]), lt) ?? base.pos_y;
			base.rotation = lerp(ks.map((k) => [k.time, k.rotation]), lt) ?? base.rotation;
			base.opacity = lerp(ks.map((k) => [k.time, k.opacity]), lt) ?? base.opacity;
		}
		return base;
	});
	const ANIM_KEYS = new Set(['scale', 'pos_x', 'pos_y', 'rotation', 'opacity']);
	/** Route a transform edit: a keyframe at the playhead when animated, else the
	 *  static transform. Crop (not animatable) always edits the static transform. */
	function setTf(patch: Partial<Transform>) {
		const c = clip;
		if (!c) return;
		const animatable = Object.keys(patch).every((k) => ANIM_KEYS.has(k));
		if (keyframes.length && animatable) {
			const time = Math.max(0, ui.time - c.timeline_start);
			void run(() => editor.addKeyframe(c.id, Math.round(time * 1000) / 1000, patch as Record<string, number>));
		} else {
			void run(() => editor.setTransform(c.id, patch));
		}
	}
	const col = $derived(clip?.color ?? DEFAULT_COLOR);
	const speed = $derived(clip?.speed ?? 1);
	const transition = $derived(clip?.transition_in ?? null);
	const effects = $derived(clip?.effects ?? []);
	const audioFx = $derived(clip?.audio ?? []);
	const keyframes = $derived(clip?.keyframes ?? []);
	const overlays = $derived(editor.overlays);

	const VIDEO_FX: Record<string, VideoEffect> = {
		blur: { type: 'blur', sigma: 6 },
		sharpen: { type: 'sharpen', amount: 1 },
		grayscale: { type: 'grayscale' },
		invert: { type: 'invert' },
		vignette: { type: 'vignette' },
		chroma_key: { type: 'chroma_key', color: 'green', similarity: 0.15, blend: 0.1 }
	};
	const AUDIO_FX: Record<string, AudioEffect> = {
		highpass: { type: 'highpass', hz: 80 },
		lowpass: { type: 'lowpass', hz: 12000 },
		equalizer: { type: 'equalizer', hz: 3000, width: 1000, gain_db: 3 },
		compressor: { type: 'compressor', threshold_db: -18, ratio: 3, attack_ms: 20, release_ms: 250, makeup_db: 6 },
		gate: { type: 'gate', threshold_db: -45 }
	};

	function addVideoFx(kindKey: string) {
		const c = clip;
		if (!c || !kindKey) return;
		void run(() => editor.setVideoEffects(c.id, [...effects, structuredClone(VIDEO_FX[kindKey])]));
	}
	function setVideoFxParam(i: number, key: string, value: unknown) {
		const c = clip;
		if (!c) return;
		void run(() => editor.setVideoEffects(c.id, effects.map((e, j) => (j === i ? { ...e, [key]: value } : e))));
	}
	function removeVideoFx(i: number) {
		const c = clip;
		if (!c) return;
		void run(() => editor.setVideoEffects(c.id, effects.filter((_, j) => j !== i)));
	}
	function addAudioFx(kindKey: string) {
		const c = clip;
		if (!c || !kindKey) return;
		void run(() => editor.setAudioEffects(c.id, [...audioFx, structuredClone(AUDIO_FX[kindKey])]));
	}
	function setAudioFxParam(i: number, key: string, value: unknown) {
		const c = clip;
		if (!c) return;
		void run(() => editor.setAudioEffects(c.id, audioFx.map((e, j) => (j === i ? { ...e, [key]: value } : e))));
	}
	function removeAudioFx(i: number) {
		const c = clip;
		if (!c) return;
		void run(() => editor.setAudioEffects(c.id, audioFx.filter((_, j) => j !== i)));
	}
	function addKeyframeHere() {
		const c = clip;
		if (!c) return;
		const time = Math.max(0, ui.time - c.timeline_start);
		void run(() => editor.addKeyframe(c.id, Math.round(time * 1000) / 1000));
	}
	function removeKeyframe(i: number) {
		const c = clip;
		if (!c) return;
		void run(() => editor.setKeyframes(c.id, keyframes.filter((_, j) => j !== i)));
	}
	function addOverlayHere() {
		const at = Math.max(0, ui.time);
		void run(async () => {
			await editor.addOverlay('Text', at, at + 3);
			const created = editor.overlays[editor.overlays.length - 1];
			if (created) editor.selectedOverlayId = created.id;
		});
	}
	function makeCaptions() {
		const id = clip?.asset_id ?? editor.selectedAssetId;
		if (!id) {
			toast.error('Select a clip or asset with a transcript first');
			return;
		}
		void run(() => editor.captionsFromTranscript(id));
	}

	// While a slider is being dragged, show its live value (keyed by row label)
	// without committing to the backend on every input event — commit happens on
	// release (`onchange`). Otherwise the readout would sit frozen until release.
	let liveDrag = $state<{ label: string; value: number } | null>(null);
	const shown = (label: string, value: number) =>
		liveDrag?.label === label ? liveDrag.value : value;

	function tc(s: number): string {
		const t = Math.max(0, s);
		const m = Math.floor(t / 60);
		const sec = Math.floor(t % 60);
		const cs = Math.floor((t % 1) * 100);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}.${cs
			.toString()
			.padStart(2, '0')}`;
	}

	async function run(op: () => Promise<unknown>) {
		try {
			await op();
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	// Right-click anywhere on the panel that isn't an editable field (those keep
	// the native menu for copy / paste). Adapts to whether a clip is selected.
	function onInspectorContextMenu(e: MouseEvent) {
		const t = e.target as Element | null;
		if (t?.closest('input, textarea, select, [contenteditable="true"], [data-selectable]')) return;
		const c = clip;
		const items: MenuItem[] = [];
		if (c) {
			if (kind === 'video') {
				items.push(
					{ label: 'Reset transform', icon: 'rotate-ccw', action: () => run(() => editor.setTransform(c.id, DEFAULT_TRANSFORM)) },
					{ label: 'Reset color', icon: 'rotate-ccw', action: () => run(() => editor.setColor(c.id, DEFAULT_COLOR)) },
					{ type: 'separator' }
				);
			}
			items.push(
				{ label: 'Remove clip', icon: 'trash', danger: true, action: () => runUndoable('Clip removed', () => editor.remove(c.id)) },
				{ label: 'Ripple delete', icon: 'trash', danger: true, action: () => runUndoable('Clip ripple-deleted', () => editor.rippleDelete(c.id)) },
				{ type: 'separator' }
			);
		}
		items.push(
			{ label: 'Add text overlay', icon: 'captions', action: addOverlayHere },
			{ label: 'Generate captions', icon: 'captions', action: makeCaptions }
		);
		contextMenu.show(e, items);
	}

	/** Run a destructive op, then surface an Undo affordance (the edit is in the
	 *  history, so Undo restores it). */
	async function runUndoable(message: string, op: () => Promise<unknown>) {
		try {
			await op();
			toast(message, { action: { label: 'Undo', onClick: () => void editor.undo() } });
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	const inputCss =
		'width:90px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:5px 7px;text-align:right';
	const selectCss =
		'background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:12px;padding:5px 7px';
	const fxNum =
		'width:58px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:11px;padding:4px 5px;text-align:right';
	const fxTxt =
		'width:70px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:11px;padding:4px 5px';
	const xBtn =
		'margin-left:auto;background:transparent;border:none;color:var(--text-muted);cursor:pointer;font-size:16px;line-height:1;padding:0 2px';
</script>

{#snippet secHead(label: string)}
	<div style="display:flex;align-items:center;gap:8px;margin:14px 0 9px">
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>{label}</span
		>
		<div style="flex:1;height:1px;background:var(--border-subtle)"></div>
	</div>
{/snippet}

{#snippet readRow(label: string, value: string)}
	<div style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<span data-selectable style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)">{value}</span>
	</div>
{/snippet}

{#snippet numRow(label: string, value: number, step: number, onCommit: (v: number) => void)}
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<input
			type="number"
			{value}
			{step}
			min="0"
			disabled={editor.busy}
			onchange={(e) => {
				const v = parseFloat(e.currentTarget.value);
				if (Number.isFinite(v)) onCommit(v);
			}}
			style={inputCss}
		/>
	</label>
{/snippet}

{#snippet rangeRow(
	label: string,
	value: number,
	min: number,
	max: number,
	step: number,
	format: (v: number) => string,
	onCommit: (v: number) => void
)}
	<label style="display:flex;align-items:center;gap:10px;padding:3px 0">
		<span style="font-size:12px;color:var(--text-muted);width:64px;flex:none">{label}</span>
		<input
			type="range"
			{min}
			{max}
			{step}
			{value}
			disabled={editor.busy}
			oninput={(e) => {
				const v = parseFloat(e.currentTarget.value);
				if (Number.isFinite(v)) liveDrag = { label, value: v };
			}}
			onchange={(e) => {
				const v = parseFloat(e.currentTarget.value);
				liveDrag = null;
				if (Number.isFinite(v)) onCommit(v);
			}}
			style="flex:1;accent-color:var(--kerf-500)"
		/>
		<span
			style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:46px;text-align:right"
			>{format(shown(label, value))}</span
		>
	</label>
{/snippet}

{#snippet fxBlock(
	title: string,
	items: Array<Record<string, unknown>>,
	presets: Record<string, unknown>,
	onAdd: (k: string) => void,
	onParam: (i: number, key: string, value: unknown) => void,
	onRemove: (i: number) => void
)}
	{@render secHead(title)}
	{#each items as e, i (i)}
		<div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap;padding:3px 0">
			<span
				style="font-size:12px;color:var(--text-secondary);text-transform:capitalize;width:70px;flex:none"
				>{String(e.type).replace('_', ' ')}</span
			>
			{#each Object.entries(e).filter(([k]) => k !== 'type') as [k, v] (k)}
				{#if typeof v === 'string'}
					<input
						type="text"
						value={v}
						title={k}
						disabled={editor.busy}
						onchange={(ev) => onParam(i, k, ev.currentTarget.value)}
						style={fxTxt}
					/>
				{:else}
					<input
						type="number"
						value={v as number}
						title={k}
						step="0.1"
						disabled={editor.busy}
						onchange={(ev) => {
							const n = parseFloat(ev.currentTarget.value);
							if (Number.isFinite(n)) onParam(i, k, n);
						}}
						style={fxNum}
					/>
				{/if}
			{/each}
			<button onclick={() => onRemove(i)} disabled={editor.busy} title="Remove" style={xBtn}>×</button>
		</div>
	{/each}
	<select
		disabled={editor.busy}
		onchange={(ev) => {
			const v = ev.currentTarget.value;
			ev.currentTarget.value = '';
			if (v) onAdd(v);
		}}
		style={selectCss + ';width:100%;margin-top:4px'}
	>
		<option value="">+ Add effect…</option>
		{#each Object.keys(presets) as k (k)}
			<option value={k}>{k.replace('_', ' ')}</option>
		{/each}
	</select>
{/snippet}

{#snippet overlaysSection()}
	{@render secHead('Text overlays')}
	<div style="display:flex;gap:7px;margin-bottom:6px">
		<Btn size="sm" variant="ghost" style="flex:1" disabled={editor.busy} onclick={addOverlayHere}>+ Text</Btn>
		<Btn size="sm" variant="ghost" disabled={editor.busy} onclick={makeCaptions}>Captions</Btn>
	</div>
	{#if overlays.length === 0}
		<div style="font-size:11px;color:var(--text-muted);line-height:1.4">
			No titles or captions yet. Add text, or generate captions from an analyzed asset's transcript.
		</div>
	{/if}
	{#each overlays as o (o.id)}
		<div style="display:flex;align-items:center;gap:6px;padding:3px 0">
			<button
				onclick={() => (editor.selectedOverlayId = editor.selectedOverlayId === o.id ? null : o.id)}
				style="flex:1;min-width:0;display:flex;align-items:center;gap:8px;background:transparent;border:none;cursor:pointer;text-align:left;color:var(--text-secondary);padding:0"
			>
				<span style="flex:1;min-width:0;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					>{o.text || '(empty)'}</span
				>
				<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{tc(o.start)}</span>
			</button>
			<button onclick={() => run(() => editor.removeOverlay(o.id))} disabled={editor.busy} title="Remove" style={xBtn}
				>×</button
			>
		</div>
		{#if editor.selectedOverlayId === o.id}
			<div style="padding:2px 0 8px;border-left:2px solid var(--border-subtle);margin-left:2px;padding-left:10px">
				<label style="display:flex;align-items:center;gap:8px;padding:3px 0">
					<span style="font-size:12px;color:var(--text-muted);width:46px;flex:none">Text</span>
					<input
						type="text"
						value={o.text}
						disabled={editor.busy}
						onchange={(e) => run(() => editor.updateOverlay(o.id, { text: e.currentTarget.value }))}
						style={inputCss + ';flex:1;width:auto;text-align:left'}
					/>
				</label>
				{@render numRow('Start', o.start, 0.1, (v) => run(() => editor.updateOverlay(o.id, { start: Math.max(0, v) })))}
				{@render numRow('End', o.end, 0.1, (v) => run(() => editor.updateOverlay(o.id, { end: v })))}
				{@render rangeRow('Pos X', o.pos_x, 0, 1, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.updateOverlay(o.id, { pos_x: v }))
				)}
				{@render rangeRow('Pos Y', o.pos_y, 0, 1, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.updateOverlay(o.id, { pos_y: v }))
				)}
				{@render rangeRow('Size', o.size, 0.02, 0.2, 0.005, (v) => `${Math.round(v * 100)}%`, (v) =>
					run(() => editor.updateOverlay(o.id, { size: v }))
				)}
				<label style="display:flex;align-items:center;gap:8px;padding:3px 0">
					<span style="font-size:12px;color:var(--text-muted);width:46px;flex:none">Color</span>
					<input
						type="text"
						value={o.color}
						disabled={editor.busy}
						onchange={(e) => run(() => editor.updateOverlay(o.id, { color: e.currentTarget.value }))}
						style={fxTxt}
					/>
					<span style="font-size:12px;color:var(--text-muted)">Box</span>
					<input
						type="text"
						value={o.bg ?? ''}
						placeholder="none"
						disabled={editor.busy}
						onchange={(e) => run(() => editor.updateOverlay(o.id, { bg: e.currentTarget.value }))}
						style={fxTxt}
					/>
				</label>
				<label style="display:flex;align-items:center;gap:8px;padding:3px 0">
					<span style="font-size:12px;color:var(--text-muted);width:46px;flex:none">Font</span>
					<select
						value={o.font ?? ''}
						disabled={editor.busy}
						onchange={(e) => run(() => editor.updateOverlay(o.id, { font: e.currentTarget.value }))}
						style={selectCss + ';flex:1'}
					>
						<option value="">Default</option>
						{#each ui.availableFonts as f (f)}
							<option value={f}>{f}</option>
						{/each}
					</select>
				</label>
				<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
					<span style="font-size:12px;color:var(--text-muted)">Bold</span>
					<input
						type="checkbox"
						checked={o.bold}
						disabled={editor.busy}
						onchange={(e) => run(() => editor.updateOverlay(o.id, { bold: e.currentTarget.checked }))}
						style="accent-color:var(--kerf-500);width:15px;height:15px"
					/>
				</label>
			</div>
		{/if}
	{/each}
{/snippet}

<div
	role="presentation"
	oncontextmenu={onInspectorContextMenu}
	style="width:var(--inspector-w);flex:none;background:var(--surface-panel);border-left:1px solid var(--border-default);display:flex;flex-direction:column;overflow:hidden"
>
	<div
		style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-default)"
	>
		<Icon n="sliders-horizontal" s={14} color="var(--text-secondary)" />
		<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary)">Inspector</span>
	</div>

	<div style="flex:1;overflow-y:auto;padding:12px">
		{#if clip}
			<div style="display:flex;gap:9px;align-items:center">
				<div
					style="width:40px;height:28px;border-radius:3px;flex:none;background:{kind === 'audio'
						? 'var(--track-audio)'
						: 'linear-gradient(135deg,#22303f,#33424f)'};display:grid;place-items:center;color:rgba(255,255,255,.8)"
				>
					<Icon n={kind === 'audio' ? 'audio-waveform' : 'video'} s={14} />
				</div>
				<div style="flex:1;min-width:0">
					<div
						style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
						title={asset?.name ?? 'clip'}
					>
						{asset?.name ?? 'clip'}
					</div>
					<div style="margin-top:3px">
						<Badge tone="neutral">{track?.name ?? (kind === 'audio' ? 'audio' : 'video')}</Badge>
					</div>
				</div>
			</div>

			{@render secHead('Position')}
			{@render readRow('Start', tc(clip.timeline_start))}
			{@render readRow('Duration', tc(clipDuration(clip)))}

			{@render secHead('Trim (source)')}
			{@render readRow('Source range', `${tc(clip.source_in)} – ${tc(clip.source_out)}`)}
			{@render numRow('In', clip.source_in, 0.1, (v) =>
				run(() => editor.trim(clip.id, v, undefined))
			)}
			{@render numRow('Out', clip.source_out, 0.1, (v) =>
				run(() => editor.trim(clip.id, undefined, v))
			)}

			{#if hasAudio}
				{@render secHead('Volume')}
				<label style="display:flex;align-items:center;gap:10px;padding:3px 0">
					<input
						type="range"
						min="0"
						max="2"
						step="0.05"
						value={clip.volume}
						disabled={editor.busy}
						oninput={(e) => {
							const v = parseFloat(e.currentTarget.value);
							if (Number.isFinite(v)) liveDrag = { label: 'Volume', value: v };
						}}
						onchange={(e) => {
							const v = parseFloat(e.currentTarget.value);
							liveDrag = null;
							if (Number.isFinite(v)) void run(() => editor.setVolume(clip.id, v));
						}}
						style="flex:1;accent-color:var(--kerf-500)"
					/>
					<span
						style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:46px;text-align:right"
						>{Math.round(shown('Volume', clip.volume) * 100)}%</span
					>
				</label>
			{/if}

			{@render secHead('Fades')}
			{@render numRow('Fade in', clip.fade_in, 0.1, (v) =>
				run(() => editor.setFade(clip.id, Math.max(0, v), undefined))
			)}
			{@render numRow('Fade out', clip.fade_out, 0.1, (v) =>
				run(() => editor.setFade(clip.id, undefined, Math.max(0, v)))
			)}

			{@render secHead('Speed')}
			{@render rangeRow('Rate', Math.abs(speed), 0.25, 4, 0.25, (v) => `${v.toFixed(2)}×`, (v) =>
				run(() => editor.setSpeed(clip.id, speed < 0 ? -v : v))
			)}
			<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
				<span style="font-size:12px;color:var(--text-muted)">Reverse</span>
				<input
					type="checkbox"
					checked={speed < 0}
					disabled={editor.busy}
					onchange={() => run(() => editor.setSpeed(clip.id, -speed))}
					style="accent-color:var(--kerf-500);width:15px;height:15px"
				/>
			</label>

			{#if kind === 'video'}
				{@render secHead(keyframes.length ? 'Transform · keyframing @ playhead' : 'Transform')}
				{@render rangeRow('Scale', tf.scale, 0.1, 2, 0.05, (v) => `${Math.round(v * 100)}%`, (v) =>
					setTf({ scale: v })
				)}
				{@render rangeRow('Position X', tf.pos_x, -0.5, 0.5, 0.01, (v) => v.toFixed(2), (v) =>
					setTf({ pos_x: v })
				)}
				{@render rangeRow('Position Y', tf.pos_y, -0.5, 0.5, 0.01, (v) => v.toFixed(2), (v) =>
					setTf({ pos_y: v })
				)}
				{@render rangeRow('Rotation', tf.rotation, -180, 180, 1, (v) => `${Math.round(v)}°`, (v) =>
					setTf({ rotation: v })
				)}
				{@render rangeRow('Opacity', tf.opacity, 0, 1, 0.05, (v) => `${Math.round(v * 100)}%`, (v) =>
					setTf({ opacity: v })
				)}
				{@render rangeRow('Crop L', tf.crop_left, 0, 0.9, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { crop_left: v }))
				)}
				{@render rangeRow('Crop R', tf.crop_right, 0, 0.9, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { crop_right: v }))
				)}
				{@render rangeRow('Crop T', tf.crop_top, 0, 0.9, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { crop_top: v }))
				)}
				{@render rangeRow('Crop B', tf.crop_bottom, 0, 0.9, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { crop_bottom: v }))
				)}

				{@render secHead('Color')}
				{@render rangeRow('Brightness', col.brightness, -1, 1, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { brightness: v }))
				)}
				{@render rangeRow('Contrast', col.contrast, 0, 4, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { contrast: v }))
				)}
				{@render rangeRow('Saturation', col.saturation, 0, 3, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { saturation: v }))
				)}
				{@render rangeRow('Gamma', col.gamma, 0.1, 3, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { gamma: v }))
				)}
			{/if}

			{@render secHead('Transition (in)')}
			<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
				<span style="font-size:12px;color:var(--text-muted)">Type</span>
				<select
					value={transition?.kind ?? ''}
					disabled={editor.busy}
					onchange={(e) => {
						const k = e.currentTarget.value as '' | TransitionKind;
						if (!k) void run(() => editor.setTransition(clip.id, null));
						else void run(() => editor.setTransition(clip.id, { kind: k, duration: transition?.duration ?? 0.5 }));
					}}
					style={selectCss}
				>
					<option value="">None</option>
					<option value="crossfade">Crossfade</option>
					<option value="dip_to_black">Dip to black</option>
				</select>
			</label>
			{#if transition}
				{@render numRow('Duration', transition.duration, 0.1, (v) =>
					run(() => editor.setTransition(clip.id, { kind: transition.kind, duration: Math.max(0.05, v) }))
				)}
			{/if}

			{#if hasAudio}
					{@render fxBlock('Audio effects', audioFx, AUDIO_FX, addAudioFx, setAudioFxParam, removeAudioFx)}
				{/if}

				{#if kind === 'video'}
					{@render fxBlock('Video effects', effects, VIDEO_FX, addVideoFx, setVideoFxParam, removeVideoFx)}

					{@render secHead('Animation')}
					<div style="display:flex;gap:7px;margin-bottom:4px">
						<Btn size="sm" variant="ghost" style="flex:1" disabled={editor.busy} onclick={addKeyframeHere}
							>+ Keyframe @ playhead</Btn
						>
						{#if keyframes.length}
							<Btn
								size="sm"
								variant="ghost"
								disabled={editor.busy}
								onclick={() => run(() => editor.clearKeyframes(clip.id))}>Clear</Btn
							>
						{/if}
					</div>
					{#if keyframes.length}
						{#each keyframes as k, i (i)}
							<div
								style="display:flex;align-items:center;gap:8px;padding:2px 0;font-family:var(--font-mono);font-size:11px;color:var(--text-secondary)"
							>
								<span style="color:var(--text-muted);width:46px;flex:none">{k.time.toFixed(2)}s</span>
								<span style="flex:1;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
									>{Math.round(k.scale * 100)}% · ({k.pos_x.toFixed(2)},{k.pos_y.toFixed(2)}) · {Math.round(
										k.rotation
									)}° · {Math.round(k.opacity * 100)}%</span
								>
								<button onclick={() => removeKeyframe(i)} disabled={editor.busy} title="Remove" style={xBtn}>×</button>
							</div>
						{/each}
						<div style="font-size:11px;color:var(--text-muted);margin-top:4px;line-height:1.4">
							Move the playhead and adjust Transform above to keyframe scale / position / rotation / opacity over time.
						</div>
					{/if}
				{/if}

				<div style="margin-top:18px;display:flex;flex-direction:column;gap:7px">
				<Btn
					variant="destructive"
					size="sm"
					icon="trash"
					iconSize={13}
					style="width:100%"
					disabled={editor.busy}
					onclick={() => runUndoable('Clip removed', () => editor.remove(clip.id))}>Remove clip</Btn
				>
				<Btn
					variant="ghost"
					size="sm"
					style="width:100%"
					disabled={editor.busy}
					onclick={() => runUndoable('Clip ripple-deleted', () => editor.rippleDelete(clip.id))}
					>Ripple delete · close gap</Btn
				>
			</div>
		{:else}
			<div
				style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:40px 16px;color:var(--text-disabled);text-align:center"
			>
				<Icon n="sliders-horizontal" s={22} />
				<span style="font-size:12px">Select a clip to inspect it</span>
			</div>
		{/if}

		{@render overlaysSection()}
	</div>
</div>
