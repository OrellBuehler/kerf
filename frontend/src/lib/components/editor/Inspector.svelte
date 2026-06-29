<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { clipDuration, DEFAULT_COLOR, DEFAULT_TRANSFORM } from '$lib/types';
	import type { TransitionKind } from '$lib/types';
	import { toast } from 'svelte-sonner';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const kind = $derived(asset?.streams.some((s) => s.kind === 'video') ? 'video' : 'audio');
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	const track = $derived(
		clip ? editor.timeline.tracks.find((t) => t.clips.some((c) => c.id === clip.id)) : undefined
	);
	const tf = $derived(clip?.transform ?? DEFAULT_TRANSFORM);
	const col = $derived(clip?.color ?? DEFAULT_COLOR);
	const speed = $derived(clip?.speed ?? 1);
	const transition = $derived(clip?.transition_in ?? null);

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

<div
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
				{@render secHead('Transform')}
				{@render rangeRow('Scale', tf.scale, 0.1, 2, 0.05, (v) => `${Math.round(v * 100)}%`, (v) =>
					run(() => editor.setTransform(clip.id, { scale: v }))
				)}
				{@render rangeRow('Position X', tf.pos_x, -0.5, 0.5, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { pos_x: v }))
				)}
				{@render rangeRow('Position Y', tf.pos_y, -0.5, 0.5, 0.01, (v) => v.toFixed(2), (v) =>
					run(() => editor.setTransform(clip.id, { pos_y: v }))
				)}
				{@render rangeRow('Rotation', tf.rotation, -180, 180, 1, (v) => `${Math.round(v)}°`, (v) =>
					run(() => editor.setTransform(clip.id, { rotation: v }))
				)}
				{@render rangeRow('Opacity', tf.opacity, 0, 1, 0.05, (v) => `${Math.round(v * 100)}%`, (v) =>
					run(() => editor.setTransform(clip.id, { opacity: v }))
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
	</div>
</div>
