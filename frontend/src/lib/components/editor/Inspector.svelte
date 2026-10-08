<script lang="ts">
	import Icon from './Icon.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import { VIDEO_THUMB_BG } from './data';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import TitlesControls from './TitlesControls.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { GENERATED_TITLE_FILL, MIN_TITLE, TITLE_SIZE_MAX, TITLE_SIZE_MIN, sampleOverlay } from '$lib/titles';
	import type { TextOverlay } from '$lib/types';
	import { clipDuration, DEFAULT_COLOR, DEFAULT_MASK, DEFAULT_REFRAME, DEFAULT_TRANSFORM } from '$lib/types';
	import { COLOR_LOOKS, activeLook } from '$lib/style-presets';
	import { MAX_GAIN } from '$lib/mixer';
	import { AUDIO_FX, VIDEO_FX } from '$lib/effect-presets';
	import { addTextHere, dropCaptions, makeCaptions } from '$lib/title-actions';
	import { needsCrop } from '$lib/smart-crop';
	import { DEFAULT_TRANSITION_SECONDS, TRANSITION_GROUPS } from '$lib/transitions';
	import type { Mask, Projection, Reframe, Transform, TransitionKind } from '$lib/types';
	import { toast } from '$lib/notifications.svelte';
	import { settings } from '$lib/settings.svelte';
	import { linkBadge } from '$lib/link-ui';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const kind = $derived(asset?.streams.some((s) => s.kind === 'video') ? 'video' : 'audio');
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	/** A picture whose sound was detached plays none of its own: the audio clip linked to it
	 *  does, and that is where the level, the fades of the *sound* and the effects belong. */
	const soundDetached = $derived(clip?.source_audio === false);
	const soundLivesOn = $derived(
		clip && soundDetached
			? (linkBadge(editor.timeline, clip, (id) => editor.assetName(id))?.partners.filter((p) => p.kind === 'audio').map((p) => p.track) ?? [])
			: []
	);
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
	// ---- 360 reframe --------------------------------------------------------
	const reframe = $derived(clip?.reframe ?? null);
	const reframeKeys = $derived(reframe?.keyframes ?? []);
	const sourceProjection = $derived(asset?.streams.find((s) => s.projection)?.projection ?? null);
	// Detection is deliberately conservative — a `Spherical Mapping` tag or an
	// Insta360 file packed as two squares, never a bare 2:1 guess. Footage that
	// carries neither signal (a stripped remux, a per-lens .insv, an unflagged
	// equirect export) would otherwise be unreachable from the GUI, so the panel
	// always offers the same `input` override the MCP `set_reframe` tool has.
	let manualProjection = $state<Projection>('equirect');
	/** Angular lerp along the shortest arc, matching the engine so the sliders
	 *  agree with what renders. The plain `lerp` above would read a 170° → -170°
	 *  pan as a 340° swing backwards. */
	function lerpAngle(points: [number, number][], at: number): number | undefined {
		if (points.length === 0) return undefined;
		const wrap = (d: number) => ((((d + 180) % 360) + 360) % 360) - 180;
		let prev = wrap(points[0][1]);
		const unwrapped: [number, number][] = [[points[0][0], prev]];
		for (const [t, v] of points.slice(1)) {
			prev += wrap(v - prev);
			unwrapped.push([t, prev]);
		}
		const v = lerp(unwrapped, at);
		return v === undefined ? undefined : wrap(v);
	}
	// Like the Transform panel: while animated the sliders show the camera
	// sampled at the playhead, and editing a channel keyframes it there.
	const cam = $derived.by(() => {
		const base = { ...DEFAULT_REFRAME, ...(reframe ?? {}) };
		if (clip && reframeKeys.length) {
			const lt = Math.max(0, ui.time - clip.timeline_start);
			const ks = [...reframeKeys].sort((a, b) => a.time - b.time);
			base.yaw = lerpAngle(ks.map((k) => [k.time, k.yaw]), lt) ?? base.yaw;
			base.pitch = lerp(ks.map((k) => [k.time, k.pitch]), lt) ?? base.pitch;
			base.roll = lerpAngle(ks.map((k) => [k.time, k.roll]), lt) ?? base.roll;
			base.fov = lerp(ks.map((k) => [k.time, k.fov]), lt) ?? base.fov;
		}
		return base;
	});
	/** Route a camera edit: a keyframe at the playhead when animated, else the
	 *  static pose. `lens_fov` / `input` / `output` are not animatable. */
	function setCam(patch: Partial<Reframe>) {
		const c = clip;
		if (!c) return;
		const animatable = Object.keys(patch).every((k) => CAM_KEYS.has(k));
		if (reframeKeys.length && animatable) {
			const time = Math.max(0, ui.time - c.timeline_start);
			void run(() =>
				editor.addReframeKeyframe(c.id, Math.round(time * 1000) / 1000, patch as Record<string, number>)
			);
		} else {
			void run(() => editor.setReframe(c.id, patch));
		}
	}
	const CAM_KEYS = new Set(['yaw', 'pitch', 'roll', 'fov']);

	const col = $derived(clip?.color ?? DEFAULT_COLOR);
	const speed = $derived(clip?.speed ?? 1);
	const transition = $derived(clip?.transition_in ?? null);
	const mask = $derived(clip?.mask ?? null);
	/** Patch the clip's mask, starting from the default when there is none. */
	const patchMask = (patch: Partial<Mask>) =>
		run(() => editor.setMask(clip!.id, { ...DEFAULT_MASK, ...(mask ?? {}), ...patch }));
	const effects = $derived(clip?.effects ?? []);
	const audioFx = $derived(clip?.audio ?? []);
	const keyframes = $derived(clip?.keyframes ?? []);
	const overlays = $derived(editor.overlays);
	const overlay = $derived(editor.selectedOverlay);

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
	function removeOverlayKeyframe(o: TextOverlay, i: number) {
		const left = (o.keyframes ?? []).filter((_, j) => j !== i).map((k) => ({ ...k }));
		void run(() => editor.setOverlayKeyframes(o.id, left));
	}
	const hasCaptions = $derived(overlays.some((o) => o.generated));

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

	// Smart crop only has a decision to make when the shot and the delivery frame
	// are different shapes — a 16:9 interview headed for a 9:16 Reel loses most
	// of its width, and which half survives is the whole question. When they
	// already match there is nothing to choose, so say so instead of offering a
	// button that would refuse.
	const deliveryAspect = $derived.by(() => {
		const fmt = editor.timeline.format;
		if (fmt?.width && fmt?.height) return fmt.width / fmt.height;
		const first = editor.assets
			.flatMap((a) => a.streams)
			.find((st) => st.kind === 'video' && st.width && st.height);
		return first?.width && first?.height ? first.width / first.height : 16 / 9;
	});
	const shotAspect = $derived.by(() => {
		const v = asset?.streams.find((st) => st.kind === 'video');
		return v?.width && v?.height ? { w: v.width, h: v.height } : null;
	});
	const reframable = $derived(
		!!shotAspect && !clip?.reframe && needsCrop(shotAspect.w, shotAspect.h, deliveryAspect)
	);
	const hasCrop = $derived(
		tf.crop_left > 0 || tf.crop_right > 0 || tf.crop_top > 0 || tf.crop_bottom > 0
	);

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
			{ label: 'Add text overlay', icon: 'captions', action: addTextHere },
			{ label: hasCaptions ? 'Regenerate captions' : 'Generate captions', icon: 'captions', action: makeCaptions }
		);
		if (hasCaptions) {
			items.push({ label: 'Clear captions', icon: 'trash', action: dropCaptions });
		}
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
		'width:90px;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:5px 7px;text-align:right';
	const selectCss =
		'background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:12px;padding:5px 7px';
	const fxNum =
		'width:58px;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:4px 5px;text-align:right';
	const fxTxt =
		'width:70px;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:12px;padding:4px 5px';
	const xBtn =
		'margin-left:auto;background:transparent;border:none;color:var(--text-muted);cursor:pointer;font-size:16px;line-height:1;min-width:28px;min-height:28px;padding:2px 5px';
	const count = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;
	const chip = (active: boolean) =>
		`padding:4px 9px;font-size:12px;cursor:pointer;border-radius:var(--radius-sm);border:var(--line-width) solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
			active ? 'var(--text-primary)' : 'var(--text-secondary)'
		}`;
</script>

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
				// Put the field back to what the clip has first: an emptied, negative
				// or clamped entry then doesn't sit there showing something that
				// was ignored, while an accepted one re-renders over this.
				e.currentTarget.value = String(value);
				if (Number.isFinite(v) && v >= 0) onCommit(v);
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
		<span style="font-size:12px;color:var(--text-secondary);width:72px;flex:none">{label}</span>
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
			style="flex:1"
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
	<InspectorSection title={title} summary={count(items.length, 'effect')}>
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
	</InspectorSection>

{/snippet}

{#snippet overlayEditor(o: TextOverlay)}
	{@const pose = sampleOverlay(o, ui.time)}
	{@const keys = o.keyframes ?? []}
	<div style="display:flex;gap:9px;align-items:center">
		<div
			style="width:40px;height:28px;border-radius:3px;flex:none;background:{o.generated
				? GENERATED_TITLE_FILL
				: 'var(--track-text)'};border:1px solid var(--track-text-edge);display:grid;place-items:center;color:var(--text-on-video)"
		>
			<Icon n="captions" s={14} />
		</div>
		<div style="flex:1;min-width:0">
			<div
				style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				title={o.text}
			>
				{o.text || '(empty)'}
			</div>
			<div style="margin-top:3px"><Badge tone="neutral">{o.generated ? 'Caption' : 'Title'} · titles lane</Badge></div>
		</div>
	</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:8px">
		On its own lane, independent of the clips beneath it. Drag it in the preview to move it, or a corner to resize.
	</div>

	<InspectorSection title="Text" summary={`${tc(o.start)} – ${tc(o.end)}`} open>
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
		{@render numRow('Start', o.start, 0.1, (v) =>
			run(() => editor.retimeOverlay(o.id, Math.min(v, Math.max(0, o.end - MIN_TITLE)), o.end))
		)}
		{@render numRow('End', o.end, 0.1, (v) =>
			run(() => editor.retimeOverlay(o.id, o.start, Math.max(v, o.start + MIN_TITLE)))
		)}
		{@render rangeRow('Pos X', pose.x, 0, 1, 0.01, (v) => v.toFixed(2), (v) =>
			run(() => editor.moveOverlay(o.id, v, pose.y, ui.time))
		)}
		{@render rangeRow('Pos Y', pose.y, 0, 1, 0.01, (v) => v.toFixed(2), (v) =>
			run(() => editor.moveOverlay(o.id, pose.x, v, ui.time))
		)}
		{@render rangeRow('Size', o.size, TITLE_SIZE_MIN, Math.max(0.2, TITLE_SIZE_MAX, o.size), 0.005, (v) => `${Math.round(v * 100)}%`, (v) =>
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
				style="accent-color:var(--kerf-500);width:18px;height:18px"
			/>
		</label>
		</InspectorSection>

	<InspectorSection title="Animation" summary={count(keys.length, 'keyframe')}>
		{#if keys.length}
			{#each keys as k, i (i)}
				<div
					style="display:flex;align-items:center;gap:8px;padding:2px 0;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
				>
					<span style="color:var(--text-muted);width:46px;flex:none">{k.time.toFixed(2)}s</span>
					<span style="flex:1;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
						>({k.pos_x.toFixed(2)},{k.pos_y.toFixed(2)}) · {Math.round(k.opacity * 100)}%</span
					>
					<button onclick={() => removeOverlayKeyframe(o, i)} disabled={editor.busy} title="Remove" style={xBtn}>×</button>
				</div>
			{/each}
			<Btn
				size="sm"
				variant="ghost"
				style="margin-top:4px"
				disabled={editor.busy}
				onclick={() => run(() => editor.setOverlayKeyframes(o.id, []))}>Clear keyframes</Btn
			>
			<div style="font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4">
				This title is animated: moving it in the preview or with Pos X / Pos Y keyframes the position at the playhead.
			</div>
		{:else}
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">
				Still title. Fade presets add opacity keyframes; once there are keyframes, moving the title at the playhead adds position keyframes.
			</div>
		{/if}
	</InspectorSection>

	<div style="margin-top:18px">
		<Btn
			variant="destructive"
			size="sm"
			icon="trash"
			iconSize={13}
			style="width:100%"
			disabled={editor.busy}
			onclick={() => run(() => editor.removeOverlay(o.id))}>Remove {o.generated ? 'caption' : 'title'}</Btn
		>
	</div>
{/snippet}

{#snippet overlaysSection()}
	<InspectorSection title="Titles lane" summary={count((editor.timeline.overlays ?? []).length, 'item')} open>
		<TitlesControls />
	</InspectorSection>
{/snippet}

<div
	role="presentation"
	oncontextmenu={onInspectorContextMenu}
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>

	<div style="flex:1;overflow-y:auto;padding:12px">
		{#if overlay}
			{@render overlayEditor(overlay)}
		{:else if clip}
			{#if editor.selectedClips.length > 1}
				<!-- Several clips are selected but an edit here acts on one. Say which,
				     rather than let a fader look like it moves them all. -->
				<div
					style="margin-bottom:10px;padding:6px 9px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:var(--surface-inset);font-size:11px;line-height:1.4;color:var(--text-secondary)"
				>
					<strong style="color:var(--kerf-300);font-weight:600">{editor.selectedClips.length} clips selected</strong>
					— the settings below edit {asset?.name ?? 'the highlighted clip'} only. Drag any selected clip to move
					them all{settings.shortcut('edit.delete') ? `; ${settings.shortcut('edit.delete')} removes them all` : ''}.
				</div>
			{/if}
			<div style="display:flex;gap:9px;align-items:center">
				<div
					style="width:40px;height:28px;border-radius:3px;flex:none;background:{kind === 'audio'
						? 'var(--track-audio)'
						: VIDEO_THUMB_BG};display:grid;place-items:center;color:color-mix(in srgb,var(--text-on-video) 80%,transparent)"
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

			<InspectorSection title="Timing" summary={`${tc(clip.timeline_start)} · ${tc(clipDuration(clip))}`} open>
			{@render readRow('Start', tc(clip.timeline_start))}
			{@render readRow('Duration', tc(clipDuration(clip)))}

			<div style="font-size:12px;color:var(--text-muted);margin:8px 0">Trim source</div>
			{@render readRow('Source range', `${tc(clip.source_in)} – ${tc(clip.source_out)}`)}
			{@render numRow('In', clip.source_in, 0.1, (v) =>
				run(() => editor.trim(clip.id, v, undefined))
			)}
			{@render numRow('Out', clip.source_out, 0.1, (v) =>
				run(() => editor.trim(clip.id, undefined, v))
			)}
			</InspectorSection>

			{#if hasAudio && soundDetached}
				<InspectorSection title="Volume" summary="Sound detached" open>
					<p data-sound-detached-note style="font-size:12px;line-height:1.45;color:var(--text-muted);margin:2px 0 0">
						{#if soundLivesOn.length > 0}
							This picture's sound plays from its linked audio clip on {soundLivesOn.join(', ')} — set the level, the
							sound's own effects and the track fader there. The picture's fades below apply to the picture only.
						{:else}
							This picture's sound was detached and its audio clip is gone, so it plays silent. Reattach audio from the
							clip menu to hear it again.
						{/if}
					</p>
				</InspectorSection>
			{:else if hasAudio}
				<InspectorSection title="Volume" summary={`${Math.round(clip.volume * 100)}%`} open>
				<label style="display:flex;align-items:center;gap:10px;padding:3px 0">
					<input
						type="range"
						min="0"
						max={MAX_GAIN}
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
						style="flex:1"
					/>
					<span
						style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:46px;text-align:right"
						>{Math.round(shown('Volume', clip.volume) * 100)}%</span
					>
				</label>
				</InspectorSection>

			{/if}

			<InspectorSection title="Fades" summary={`${clip.fade_in}s in · ${clip.fade_out}s out`}>
			{@render numRow('Fade in', clip.fade_in, 0.1, (v) =>
				run(() => editor.setFade(clip.id, Math.min(v, clipDuration(clip)), undefined))
			)}
			{@render numRow('Fade out', clip.fade_out, 0.1, (v) =>
				run(() => editor.setFade(clip.id, undefined, Math.min(v, clipDuration(clip))))
			)}
			</InspectorSection>

			<InspectorSection title="Speed" summary={`${Math.abs(speed).toFixed(2)}×${speed < 0 ? ' · reverse' : ''}`}>
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
					style="accent-color:var(--kerf-500);width:18px;height:18px"
				/>
			</label>
			</InspectorSection>

			{#if kind === 'video'}
				<InspectorSection title="360 reframe" summary={reframe ? (reframeKeys.length ? `Animated · ${count(reframeKeys.length, 'key')}` : 'On') : 'Off'}>
				{#if reframe}
					{@render rangeRow('Yaw', cam.yaw, -180, 180, 1, (v) => `${Math.round(v)}°`, (v) =>
						setCam({ yaw: v })
					)}
					{@render rangeRow('Pitch', cam.pitch, -90, 90, 1, (v) => `${Math.round(v)}°`, (v) =>
						setCam({ pitch: v })
					)}
					{@render rangeRow('Roll', cam.roll, -180, 180, 1, (v) => `${Math.round(v)}°`, (v) =>
						setCam({ roll: v })
					)}
					{@render rangeRow('FOV', cam.fov, 20, 180, 1, (v) => `${Math.round(v)}°`, (v) =>
						setCam({ fov: v })
					)}
					{#if reframe.input === 'dual_fisheye'}
						{@render rangeRow('Lens FOV', cam.lens_fov, 170, 220, 1, (v) => `${Math.round(v)}°`, (v) =>
							run(() => editor.setReframe(clip.id, { lens_fov: v }))
						)}
						<p style="font-size:12px;color:var(--text-muted);margin:4px 0 0;line-height:1.4">
							Approximate stitch — the seam is a hard blend, not Insta360's optical-flow
							one. Tune Lens FOV to move it, or use a Studio equirect export for a clean
							join.
						</p>
					{/if}
					<div style="display:flex;gap:6px;padding:6px 0 0">
						<Btn
							size="sm"
							disabled={editor.busy}
							onclick={() =>
								run(() =>
									editor.addReframeKeyframe(
										clip.id,
										Math.round(Math.max(0, ui.time - clip.timeline_start) * 1000) / 1000
									)
								)}>+ Camera key</Btn
						>
						{#if reframeKeys.length}
							<Btn
								size="sm"
								disabled={editor.busy}
								onclick={() => run(() => editor.setReframeKeyframes(clip.id, []))}
								>Clear {reframeKeys.length} key{reframeKeys.length === 1 ? '' : 's'}</Btn
							>
						{/if}
						<button style={xBtn} title="Stop reframing" disabled={editor.busy}
							onclick={() => run(() => editor.clearReframe(clip.id))}>×</button
						>
					</div>
				{:else if sourceProjection}
					<p style="font-size:12px;color:var(--text-muted);margin:0 0 6px;line-height:1.4">
						360 source ({sourceProjection === 'dual_fisheye' ? 'dual fisheye' : sourceProjection}),
						shown raw.
					</p>
					<Btn
						size="sm"
						disabled={editor.busy}
						onclick={() => run(() => editor.setReframe(clip.id, { input: sourceProjection! }))}
						>Reframe to flat</Btn
					>
				{:else}
					<p style="font-size:12px;color:var(--text-muted);margin:0 0 6px;line-height:1.4">
						Not detected as 360. If this really is spherical footage, pick how the source is
						packed — the whole asset is marked, so every clip cut from it reframes.
					</p>
					<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
						<span style="font-size:12px;color:var(--text-muted)">Source is</span>
						<select
							value={manualProjection}
							disabled={editor.busy}
							onchange={(e) => (manualProjection = e.currentTarget.value as Projection)}
							style={selectCss}
						>
							<option value="equirect">Equirectangular</option>
							<option value="dual_fisheye">Dual fisheye</option>
							<option value="fisheye">Fisheye</option>
						</select>
					</label>
					<Btn
						size="sm"
						disabled={editor.busy}
						onclick={() =>
							run(async () => {
								await editor.setAssetProjection(clip.asset_id, manualProjection);
								await editor.setReframe(clip.id, { input: manualProjection });
							})}>Mark as 360 &amp; reframe</Btn
					>
				{/if}
				</InspectorSection>

			{/if}

			{#if kind === 'video'}
				<InspectorSection title="Transform" summary={keyframes.length ? `Animated · ${count(keyframes.length, 'key')}` : `${Math.round(tf.scale * 100)}% · ${Math.round(tf.rotation)}°`}>
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
				</InspectorSection>

				<InspectorSection title="Framing" summary={hasCrop ? 'Cropped' : 'Full frame'}>
				<div style="display:flex;gap:5px;flex-wrap:wrap;align-items:center;margin-bottom:6px">
					<button
						style={chip(false)}
						disabled={editor.busy || !reframable}
						title="Sample where this shot's content is and crop to the delivery frame around it"
						onclick={() =>
							runUndoable('Framed for the delivery frame', () => editor.smartCrop(clip.id))}
						>Smart crop</button
					>
					<button
						style={chip(false)}
						disabled={editor.busy || !hasCrop}
						title="Clear the crop"
						onclick={() =>
							run(() =>
								editor.setTransform(clip.id, {
									crop_left: 0,
									crop_right: 0,
									crop_top: 0,
									crop_bottom: 0
								})
							)}>Reset crop</button
					>
				</div>
				{#if !reframable}
					<div style="font:var(--type-caption);color:var(--text-muted);margin:-2px 0 6px">
						{clip.reframe
							? 'A 360 clip is framed by its virtual camera, above.'
							: 'This shot already matches the delivery frame.'}
					</div>
				{/if}
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
				</InspectorSection>

				<InspectorSection title="Color" summary={activeLook(col)?.label ?? 'Custom'}>
				<div style="display:flex;gap:5px;flex-wrap:wrap;margin-bottom:6px">
					{#each COLOR_LOOKS as look (look.id)}
						<button
							style={chip(activeLook(col)?.id === look.id)}
							disabled={editor.busy}
							onclick={() => run(() => editor.setColor(clip.id, look.color))}>{look.label}</button
						>
					{/each}
					<button
						style={chip(false)}
						disabled={editor.busy}
						title="Reset color"
						onclick={() => run(() => editor.setColor(clip.id, DEFAULT_COLOR))}>Reset</button
					>
				</div>
				{@render rangeRow('Brightness', col.brightness, -1, 1, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { brightness: v }))
				)}
				{@render rangeRow('Contrast', col.contrast, 0, 4, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { contrast: v }))
				)}
				{@render rangeRow('Saturation', col.saturation, 0, 3, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { saturation: v }))
				)}
				{@render rangeRow('Warmth', col.temperature ?? 0, -1, 1, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { temperature: v }))
				)}
				{@render rangeRow('Gamma', col.gamma, 0.1, 3, 0.05, (v) => v.toFixed(2), (v) =>
					run(() => editor.setColor(clip.id, { gamma: v }))
				)}
				</InspectorSection>

			{/if}

			<InspectorSection title="Mask" summary={mask ? (mask.shape === 'rect' ? 'Rectangle' : 'Ellipse') : 'None'}>
			<div style="display:flex;gap:5px;flex-wrap:wrap;align-items:center;margin-bottom:6px">
				<button
					style={chip(!mask)}
					disabled={editor.busy}
					title="No mask — the whole frame"
					onclick={() => run(() => editor.setMask(clip.id, null))}>None</button
				>
				<button
					style={chip(mask?.shape === 'rect')}
					disabled={editor.busy}
					title="Cut this clip to a rectangle"
					onclick={() => patchMask({ shape: 'rect' })}>Rectangle</button
				>
				<button
					style={chip(mask?.shape === 'ellipse')}
					disabled={editor.busy}
					title="Cut this clip to an ellipse"
					onclick={() => patchMask({ shape: 'ellipse' })}>Ellipse</button
				>
			</div>
			{#if mask}
				{@render rangeRow('Centre X', mask.x, 0, 1, 0.01, (v) => v.toFixed(2), (v) => patchMask({ x: v }))}
				{@render rangeRow('Centre Y', mask.y, 0, 1, 0.01, (v) => v.toFixed(2), (v) => patchMask({ y: v }))}
				{@render rangeRow('Width', mask.width, 0.02, 1.5, 0.01, (v) => v.toFixed(2), (v) =>
					patchMask({ width: v })
				)}
				{@render rangeRow('Height', mask.height, 0.02, 1.5, 0.01, (v) => v.toFixed(2), (v) =>
					patchMask({ height: v })
				)}
				{@render rangeRow('Feather', mask.feather, 0, 1, 0.01, (v) => v.toFixed(2), (v) =>
					patchMask({ feather: v })
				)}
				<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
					<span style="font-size:12px;color:var(--text-muted)">Invert</span>
					<input
						type="checkbox"
						checked={!!mask.inverted}
						disabled={editor.busy}
						onchange={(e) => patchMask({ inverted: e.currentTarget.checked })}
					/>
				</label>
				<div style="font:var(--type-caption);color:var(--text-muted);margin:2px 0 6px">
					Outside the shape this clip is transparent, so a lower track shows through. To blur a
					face: duplicate the shot onto the track above, blur the copy, mask the copy.
				</div>
			{/if}
			</InspectorSection>

			<InspectorSection title="Transition (in)" summary={transition ? `${transition.kind.replaceAll('_', ' ')} · ${transition.duration}s` : 'None'}>
			<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
				<span style="font-size:12px;color:var(--text-muted)">Type</span>
				<select
					value={transition?.kind ?? ''}
					disabled={editor.busy}
					onchange={(e) => {
						const k = e.currentTarget.value as '' | TransitionKind;
						if (!k) void run(() => editor.setTransition(clip.id, null));
						else void run(() => editor.setTransition(clip.id, { kind: k, duration: transition?.duration ?? DEFAULT_TRANSITION_SECONDS }));
					}}
					style={selectCss}
				>
					<option value="">None</option>
					{#each TRANSITION_GROUPS as g (g.label)}
						<optgroup label="{g.label} — {g.hint}">
							{#each g.options as o (o.id)}
								<option value={o.id}>{o.label}</option>
							{/each}
						</optgroup>
					{/each}
				</select>
			</label>
			{#if transition}
				{@render numRow('Duration', transition.duration, 0.1, (v) =>
					run(() => editor.setTransition(clip.id, { kind: transition.kind, duration: Math.max(0.05, v) }))
				)}
			{/if}
			</InspectorSection>

			{#if hasAudio && !soundDetached}
					{@render fxBlock('Audio effects', audioFx, AUDIO_FX, addAudioFx, setAudioFxParam, removeAudioFx)}
				{/if}

				{#if kind === 'video'}
					{@render fxBlock('Video effects', effects, VIDEO_FX, addVideoFx, setVideoFxParam, removeVideoFx)}

					<InspectorSection title="Animation" summary={count(keyframes.length, 'keyframe')}>
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
								style="display:flex;align-items:center;gap:8px;padding:2px 0;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
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
						<div style="font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4">
							Move the playhead and adjust Transform above to keyframe scale / position / rotation / opacity over time.
						</div>
					{/if}
					</InspectorSection>

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
				<span style="font-size:12px">Select a clip or a title to inspect it</span>
			</div>
		{/if}

		{@render overlaysSection()}
	</div>
</div>
