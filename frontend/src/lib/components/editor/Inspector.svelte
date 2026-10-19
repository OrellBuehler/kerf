<script lang="ts">
	import Icon from './Icon.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import { VIDEO_THUMB_BG } from './data';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Chip from './Chip.svelte';
	import EmptyState from './EmptyState.svelte';
	import CheckRow from './CheckRow.svelte';
	import SliderRow from './SliderRow.svelte';
	import GradeSections from './GradeSections.svelte';
	import TemplateSection from './TemplateSection.svelte';
	import MediaChips from './MediaChips.svelte';
	import AnalysisRun from './AnalysisRun.svelte';
	import AssetInspector from './AssetInspector.svelte';
	import { analyzeMenuItems } from '#lib/analyze-menu';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import type { MenuItem } from '#lib/context-menu.svelte';
	import { GENERATED_TITLE_FILL, MIN_TITLE, SVG_TITLE_FILL, TITLE_SIZE_MAX, TITLE_SIZE_MIN, sampleOverlay } from '#lib/titles';
	import type { SvgOverlay, SvgOverlayPatch, SvgPresetInfo, SvgPresetParams, TextOverlay } from '#lib/types';
	import { clipDuration, DEFAULT_COLOR, DEFAULT_MASK, DEFAULT_REFRAME, DEFAULT_TRANSFORM } from '#lib/types';
	import { MAX_GAIN } from '#lib/mixer';
	import { AUDIO_FX, VIDEO_FX } from '#lib/effect-presets';
	import { applySvgSource, changePreset, convertToCustom, svgPresetInfos } from '#lib/title-actions';
	import { copyKeyframesOf, pasteKeyframesOnto, report } from '#lib/ops';
	import { keyframeOffer } from '#lib/keyframe-copy';
	import { SVG_PRESETS, svgDisplayName, takesParam } from '#lib/svg-titles';
	import { untrack } from 'svelte';
	import { needsCrop } from '#lib/smart-crop';
	import { EASING_CHOICES, easingId } from '#lib/easing';
	import {
		channelOf,
		colorAnimated,
		colorAt,
		isKeyed,
		propertyAt,
		propertyKeys,
		transformAnimated,
		transformAt,
		upsertKey,
		volumeAnimated
	} from '#lib/channels';
	import { DEFAULT_TRANSITION_SECONDS, TRANSITION_GROUPS } from '#lib/transitions';
	import type { Clip, Color, Mask, Projection, Property, Reframe, Transform, TransitionKind } from '#lib/types';
	import { toast } from '#lib/notifications.svelte';
	import { settings } from '#lib/settings.svelte';
	import { linkBadge, selectionUnits } from '#lib/link-ui';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { parseColor, swatchHex, withPicked } from '#lib/title-color';
	import {
		colorSummary,
		keyLabel,
		keyNeighbours,
		svgPlacementSummary,
		svgSourceSummary,
		timingSummary
	} from '#lib/inspector-summary';
	import { fitOffer, musicSummary } from '#lib/music-ui';
	import MusicStructure from './MusicStructure.svelte';
	import { DEGREES, NUMBER_COL, PERCENT, SECONDS, TIMES, readout, type RowSpec } from '#lib/slider-row';
	import { FieldEdits } from '#lib/field-edit';
	import { tip } from '#lib/tip';
	import ComposedMusic from './ComposedMusic.svelte';
	import { scoreTitle } from '#lib/music-score';

	const clip = $derived(editor.selectedClip);
	/** What a number row is editing: the clip or title and the playhead, as they are when an edit
	 *  *begins* (`SliderRow` hands this back with the value). A typed entry is written by the click that
	 *  selects another clip, and a run of arrow keys when focus leaves, so what is selected then is not
	 *  where the edit goes. */
	interface Edit {
		clipId: string | null;
		overlayId: string | null;
		svgId: string | null;
		time: number;
	}
	const scope = $derived<Edit>({
		clipId: editor.selectedClipId,
		overlayId: editor.selectedOverlayId,
		svgId: editor.selectedSvgOverlayId,
		time: ui.time
	});
	/** A fade of the clip the edit began on, held to that clip's length. */
	function setFade(at: Edit, to: { fade_in?: number; fade_out?: number }) {
		const c = clipOf(at);
		if (!c) return;
		const len = clipDuration(c);
		return run(() =>
			editor.setFade(
				c.id,
				to.fade_in === undefined ? undefined : Math.min(to.fade_in, len),
				to.fade_out === undefined ? undefined : Math.min(to.fade_out, len)
			)
		);
	}
	/** Move a title to `x` / `y`, the other coordinate as it was sampled when the edit began. */
	function moveTitle(at: Edit, to: { x?: number; y?: number }) {
		const ov = overlayOf(at);
		if (!ov) return;
		const pose = sampleOverlay(ov, at.time);
		return run(() => editor.moveOverlay(ov.id, to.x ?? pose.x, to.y ?? pose.y, at.time));
	}
	/** What the plain fields (a number, a title's text, an effect's parameter) are editing: the scope when each took
	 *  focus and what was typed into it. Its `change` comes with the blur, after the click that selects another clip has
	 *  moved the selection and the field has been re-rendered with that clip's value. */
	const fields = new FieldEdits<Edit>();
	const noteFocus = (e: { currentTarget: HTMLElement }) => fields.focus(e.currentTarget, scope);
	const noteTyped = (e: { currentTarget: HTMLInputElement }) => fields.input(e.currentTarget, e.currentTarget.value);
	const endEdit = (el: HTMLInputElement | HTMLSelectElement) => fields.end(el, { scope, text: el.value });
	const clipOf = (at: Edit): Clip | undefined => {
		for (const t of editor.timeline.tracks) {
			const c = t.clips.find((c) => c.id === at.clipId);
			if (c) return c;
		}
		return undefined;
	};
	const overlayOf = (at: Edit): TextOverlay | undefined => editor.overlays.find((o) => o.id === at.overlayId);
	const svgOf = (at: Edit): SvgOverlay | undefined => editor.svgOverlays.find((o) => o.id === at.svgId);
	/** Write a title field to the title it took focus on (see `fields`), with what was typed into it. */
	function patchTitle(
		el: HTMLInputElement | HTMLSelectElement,
		patch: (typed: string) => Partial<Omit<TextOverlay, 'id' | 'keyframes'>>,
		shown?: (o: TextOverlay) => string
	) {
		const { scope: at, text } = endEdit(el);
		const ov = overlayOf(at);
		if (!ov) return;
		// A select changed from the keyboard while an edit is in flight or the proposal is on screen: its press is swallowed
		// but its keys are not. Nothing is written, and it goes back to what the title has.
		if (editor.locked) {
			if (shown && el.isConnected) el.value = shown(ov);
			return;
		}
		void run(() => editor.updateOverlay(ov.id, patch(text)));
	}
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const kind = $derived(asset?.streams.some((s) => s.kind === 'video') ? 'video' : 'audio');
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	/** The rate the clip's footage counts frames at — the In and Out are positions in *it*, not in the cut. */
	const sourceFps = $derived(asset?.streams.find((s) => s.kind === 'video')?.fps || editor.fps);
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
	/** Something is drawn from this clip: its footage has a picture *and* it sits on a picture track.
	 *  The sound half of a linked pair has the picture's asset but plays no picture of its own. */
	const picture = $derived(kind === 'video' && track?.kind !== 'audio');
	const linkInfo = $derived(clip ? linkBadge(editor.timeline, clip, (id) => editor.assetName(id)) : null);
	const linkedTo = $derived(linkInfo?.partners.map((p) => p.track) ?? []);
	/** Music with a fitted bar grid offers *Fit to video*: the section only exists for a clip that does. */
	const music = $derived(clip ? editor.analysisFor(clip.asset_id)?.music : undefined);
	const musicOffer = $derived(clip ? fitOffer(track, clip, editor.analysisFor(clip.asset_id)) : { show: false, reason: null });
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
	/** Seconds into the selected clip at the playhead (clip-local, what keys are in). */
	const localTime = $derived(clip ? Math.max(0, ui.time - clip.timeline_start) : 0);
	// Every keyed number as the eased polyline the export draws, whichever of the legacy bundle and
	// the numbers' own tracks drives it (`transformAt`).
	const tf = $derived(clip ? transformAt(clip, localTime) : { ...DEFAULT_TRANSFORM });
	const ANIM_KEYS = new Set(['scale', 'pos_x', 'pos_y', 'rotation', 'opacity']);
	/** Route a transform edit: a keyframe at the playhead when animated, else the
	 *  static transform. Crop (not animatable) always edits the static transform. */
	function setTf(patch: Partial<Transform>, at: Edit = scope) {
		const c = clipOf(at);
		if (!c) return;
		const animatable = Object.keys(patch).every((k) => ANIM_KEYS.has(k));
		if (c.keyframes?.length && animatable) {
			const time = Math.max(0, at.time - c.timeline_start);
			return run(() => editor.addKeyframe(c.id, Math.round(time * 1000) / 1000, patch as Record<string, number>));
		} else if (animatable && transformAnimated(c)) {
			// Numbers keyed on their own (no whole-transform keys): a key at the playhead for each
			// of those, the static transform for the rest.
			const keyed = (Object.keys(patch) as (keyof Transform)[]).filter((k) => isKeyed(c, k as Property));
			const rest = Object.fromEntries(Object.entries(patch).filter(([k]) => !keyed.includes(k as keyof Transform)));
			return run(async () => {
				for (const k of keyed) await keyAtPlayhead(c, k as Property, patch[k] as number, at.time);
				if (Object.keys(rest).length) await editor.setTransform(c.id, rest);
			});
		} else {
			return run(() => editor.setTransform(c.id, patch));
		}
	}
	/** A key for `prop` at the playhead (clip-local, to the millisecond), put in as the backend does:
	 *  re-keying a moment keeps how it leaves, a key inside a segment splits it. */
	function keyAtPlayhead(c: Clip, prop: Property, value: number, playhead: number = ui.time) {
		const time = Math.round(Math.max(0, playhead - c.timeline_start) * 1000) / 1000;
		return editor.setPropertyKeyframes(c.id, prop, upsertKey(propertyKeys(c, prop), { time, value }));
	}
	/** The ◆ beside a number: key it at the playhead at the value it has there, or — a key already
	 *  sits there — take that key out (the last one makes the number static again). */
	function toggleKey(prop: Property) {
		const c = clip;
		if (!c) return;
		const time = Math.round(localTime * 1000) / 1000;
		const keys = propertyKeys(c, prop);
		if (keys.some((k) => Math.abs(k.time - time) <= 0.02)) {
			void run(() => editor.setPropertyKeyframes(c.id, prop, keys.filter((k) => Math.abs(k.time - time) > 0.02)));
		} else {
			void run(() => keyAtPlayhead(c, prop, propertyAt(c, prop, localTime)));
		}
	}
	/** Right-click on a ◇: step to the previous or next key of that number, key it here, or take its keys out. */
	function keyMenu(e: MouseEvent, prop: Property) {
		const c = clip;
		if (!c) return;
		e.preventDefault();
		e.stopPropagation();
		const keys = propertyKeys(c, prop);
		const near = keyNeighbours(keys.map((k) => k.time), localTime);
		const go = (t: number) => ui.seek(c.timeline_start + t);
		const items: MenuItem[] = [
			{ label: 'Previous key', icon: 'skip-back', disabled: near.prev === null, reason: near.prev === null ? 'no earlier key' : undefined, action: () => go(near.prev!) },
			{ label: 'Next key', icon: 'skip-forward', disabled: near.next === null, reason: near.next === null ? 'no later key' : undefined, action: () => go(near.next!) },
			{ type: 'separator' },
			{ label: near.here ? 'Remove the key here' : 'Key at the playhead', icon: 'diamond', action: () => toggleKey(prop) }
		];
		if (keys.length > 0)
			items.push({
				label: `Remove all ${keyLabel(prop)} keys`,
				icon: 'trash',
				danger: true,
				action: () => void run(() => editor.setPropertyKeyframes(c.id, prop, []))
			});
		contextMenu.show(e, items);
	}
	/** Route a colour edit: a key at the playhead for each number that is keyed, the static
	 *  colour for the rest. */
	function setCol(patch: Partial<Color>, at: Edit = scope) {
		const c = clipOf(at);
		if (!c) return;
		if (!colorAnimated(c)) return run(() => editor.setColor(c.id, patch));
		const keyed = (Object.keys(patch) as (keyof Color)[]).filter((k) => isKeyed(c, k));
		const rest = Object.fromEntries(Object.entries(patch).filter(([k]) => !keyed.includes(k as keyof Color)));
		return run(async () => {
			for (const k of keyed) await keyAtPlayhead(c, k, patch[k] as number, at.time);
			if (Object.keys(rest).length) await editor.setColor(c.id, rest);
		});
	}
	/** The volume the slider shows and edits: the curve at the playhead while it is keyed. */
	const gain = $derived(clip ? propertyAt(clip, 'volume', localTime) : 1);
	function setGain(v: number, at: Edit = scope) {
		const c = clipOf(at);
		if (!c) return;
		return volumeAnimated(c) ? run(() => keyAtPlayhead(c, 'volume', v, at.time)) : run(() => editor.setVolume(c.id, v));
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
	function setCam(patch: Partial<Reframe>, at: Edit = scope) {
		const c = clipOf(at);
		if (!c) return;
		const animatable = Object.keys(patch).every((k) => CAM_KEYS.has(k));
		if (c.reframe?.keyframes?.length && animatable) {
			const time = Math.max(0, at.time - c.timeline_start);
			return run(() =>
				editor.addReframeKeyframe(c.id, Math.round(time * 1000) / 1000, patch as Record<string, number>)
			);
		} else {
			return run(() => editor.setReframe(c.id, patch));
		}
	}
	const CAM_KEYS = new Set(['yaw', 'pitch', 'roll', 'fov']);

	const col = $derived(clip ? colorAt(clip, localTime) : DEFAULT_COLOR);
	const speed = $derived(clip?.speed ?? 1);
	const transition = $derived(clip?.transition_in ?? null);
	const mask = $derived(clip?.mask ?? null);
	/** Patch the clip's mask, starting from the default when there is none. */
	const patchMask = (patch: Partial<Mask>, at: Edit = scope) => {
		const c = clipOf(at);
		if (c) return run(() => editor.setMask(c.id, { ...DEFAULT_MASK, ...(c.mask ?? {}), ...patch }));
	};
	const effects = $derived(clip?.effects ?? []);
	const audioFx = $derived(clip?.audio ?? []);
	const keyframes = $derived(clip?.keyframes ?? []);
	const overlays = $derived(editor.overlays);
	const overlay = $derived(editor.selectedOverlay);
	const svgTitle = $derived(editor.selectedSvgOverlay);

	// ---- animated SVG titles ---------------------------------------------------------------
	// The artwork is the engine's; this edits the title around it (timing, placement, a preset's
	// parameters) and, for custom artwork, its source. The page never draws the SVG itself.
	let presetInfos = $state<SvgPresetInfo[]>([]);
	$effect(() => {
		if (!svgTitle?.preset || presetInfos.length) return;
		svgPresetInfos().then(
			(infos) => (presetInfos = infos),
			() => {}
		);
	});
	const presetInfo = $derived(svgTitle?.preset ? presetInfos.find((i) => i.kind === svgTitle.preset?.kind) : undefined);

	/** A change to a title field written to the title it took focus on (see `fields`), with what was typed into it. A
	 *  refusal is a toast, and the field goes back to what the title has (it would otherwise keep showing what was refused). */
	async function patchSvg(
		el: HTMLInputElement | HTMLSelectElement,
		patch: (typed: string, o: SvgOverlay) => SvgOverlayPatch,
		shown: (o: SvgOverlay) => string
	) {
		const { scope: at, text } = endEdit(el);
		const o = svgOf(at);
		if (!o) return;
		if (editor.locked) {
			if (el.isConnected) el.value = shown(o);
			return;
		}
		try {
			await editor.updateSvgOverlay(o.id, patch(text, o));
		} catch (e) {
			report(e);
			const now = svgOf(at);
			if (now && el.isConnected) el.value = shown(now);
		}
	}
	/** Whether the playhead is inside the title, where the preview draws its placement box. */
	const isVisibleNow = (o: SvgOverlay) => ui.time >= o.start && ui.time <= o.end;
	/** A preset's parameters with one changed; a patch always carries all of them, since the backend fills what is left out
	 *  with defaults. */
	const presetPatch = (o: SvgOverlay, change: Partial<SvgPresetParams>): SvgOverlayPatch =>
		o.preset ? { preset_params: { ...o.preset.params, ...change } } : {};

	// The source box of a custom title: a draft that follows the title until it is edited, applied with Apply.
	let draft = $state('');
	let seeded = $state<{ id: string; text: string } | null>(null);
	let sourceEl = $state<HTMLTextAreaElement | null>(null);
	let sourceError = $state<string | null>(null);
	let sourceWarnings = $state<string[]>([]);
	let sourceRuns = $state<number | null>(null);
	let applying = $state(false);
	const draftDirty = $derived(svgTitle ? draft !== svgTitle.svg : false);

	$effect(() => {
		const id = svgTitle?.id ?? null;
		const text = svgTitle?.svg ?? '';
		untrack(() => {
			if (!id) {
				seeded = null;
				return;
			}
			// Another title, or the same one changed underneath an untouched draft (an undo, an agent): take its source.
			if (seeded === null || seeded.id !== id || draft === seeded.text) {
				draft = text;
				if (seeded?.id !== id) {
					sourceError = null;
					sourceWarnings = [];
					sourceRuns = null;
				}
			}
			seeded = { id, text };
		});
	});

	// *Custom SVG…* and the lane menu's *Edit SVG source* ask for the source box: open its section and put the caret in it.
	$effect(() => {
		const id = ui.svgSourceFocus;
		const el = sourceEl;
		if (!id || !el || svgTitle?.id !== id || svgTitle.preset) return;
		const section = el.closest('details');
		if (section) section.open = true;
		el.focus();
		el.scrollIntoView({ block: 'nearest' });
		ui.svgSourceFocus = null;
	});

	async function applySource() {
		const o = svgTitle;
		if (!o || applying) return;
		applying = true;
		sourceError = null;
		try {
			const check = await applySvgSource(o.id, draft);
			sourceWarnings = check.warnings;
			sourceRuns = check.natural_duration;
			draft = check.svg;
		} catch (e) {
			sourceError = e instanceof Error ? e.message : String(e);
			sourceWarnings = [];
		} finally {
			applying = false;
		}
	}
	function revertSource() {
		if (svgTitle) draft = svgTitle.svg;
		sourceError = null;
	}

	function addVideoFx(kindKey: string) {
		const c = clip;
		if (!c || !kindKey) return;
		void run(() => editor.setVideoEffects(c.id, [...effects, structuredClone(VIDEO_FX[kindKey])]));
	}
	/** An effect's parameter on the clip the edit began on (see `fields`), read from that clip's chain as it is now. */
	function setVideoFxParam(i: number, key: string, value: unknown, at: Edit = scope) {
		const c = clipOf(at);
		const chain = c?.effects ?? [];
		if (!c || !chain[i]) return;
		void run(() => editor.setVideoEffects(c.id, chain.map((e, j) => (j === i ? { ...e, [key]: value } : e))));
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
	function setAudioFxParam(i: number, key: string, value: unknown, at: Edit = scope) {
		const c = clipOf(at);
		const chain = c?.audio ?? [];
		if (!c || !chain[i]) return;
		void run(() => editor.setAudioEffects(c.id, chain.map((e, j) => (j === i ? { ...e, [key]: value } : e))));
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
	function setEasing(el: HTMLSelectElement, k: { time: number }, was: string) {
		// Pending, not disabled, so the keys still reach the select: nothing is written, and it goes back.
		if (editor.locked) {
			el.value = was;
			return;
		}
		const c = clip;
		const choice = EASING_CHOICES.find((e) => e.id === el.value);
		if (!c || !choice) return;
		void run(() => editor.setKeyframeEasing(c.id, k.time, choice.easing));
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
			report(e);
		}
	}

	/** A typed In / Out, asked of the port of the backend's own rules first (`trimRefusal`): footage the file does
	 *  not have, a negative start, a lane it would land on — said at once, with the fields named as the Inspector names them. */
	async function trimChecked(clipId: string, sourceIn?: number, sourceOut?: number) {
		const why = editor.trimRefusal(clipId, sourceIn, sourceOut);
		if (why) throw new Error(why);
		await editor.trim(clipId, sourceIn, sourceOut);
	}

	// Right-click anywhere on the panel that isn't an editable field (those keep
	// the native menu for copy / paste). Adapts to whether a clip is selected.
	function onInspectorContextMenu(e: MouseEvent) {
		const t = e.target as Element | null;
		if (t?.closest('input, textarea, select, [contenteditable="true"], [data-selectable]')) return;
		// What is selected is what this menu acts on; adding a title or captioning the cut is the Titles tab's,
		// the titles lane's and the Edit menu's.
		const c = clip;
		if (!c) return;
		const items: MenuItem[] = [];
		if (picture) {
			items.push(
				{ label: 'Reset transform', icon: 'rotate-ccw', action: () => run(() => editor.setTransform(c.id, DEFAULT_TRANSFORM)) },
				{ label: 'Reset color', icon: 'rotate-ccw', action: () => run(() => editor.setColor(c.id, DEFAULT_COLOR)) },
				{ type: 'separator' }
			);
		}
		items.push(
			{ label: 'Remove clip', icon: 'trash', danger: true, action: () => runUndoable('Clip removed', () => editor.remove(c.id)) },
			{ label: 'Ripple delete', icon: 'trash', danger: true, action: () => runUndoable('Clip ripple-deleted', () => editor.rippleDelete(c.id)) }
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
			report(e);
		}
	}

	/** A field that is off while an edit is in flight or the proposal is on screen. It is read-only and `aria-disabled`,
	 *  never `disabled`: a disabled field drops the keyboard focus the edit that is being written began in, and the
	 *  next key goes nowhere. (`Btn`, `Chip`, `CheckRow`, `SliderRow` do the same through their `pending` / `disabled`.) */
	const off = $derived(editor.locked ? { readonly: true, 'aria-disabled': true } : {});
	/** The same for a `<select>` and a colour box, which have no read-only: the press that would open them is swallowed
	 *  and `aria-disabled` says why. (While the proposal is on screen the whole panel is `inert`.) */
	const swallow = (e: Event) => {
		if (editor.locked) e.preventDefault();
	};

	// One height for every field in the panel (`--control-sm`), so a number, a text and a select sit on one line.
	const FIELD = 'box-sizing:border-box;height:var(--control-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm)';
	/** A number: mono, right-aligned, in the value column. */
	const inputCss = `${FIELD};width:90px;font-family:var(--font-mono);padding:0 7px;text-align:right`;
	/** Words — a title's text, an effect's string parameter — are set in the sans, not the number face. */
	const textCss = `${FIELD};padding:0 8px`;
	const fxNum = `${FIELD};width:58px;font-family:var(--font-mono);padding:0 5px;text-align:right`;
	const fxTxt = `${FIELD};width:70px;padding:0 5px`;
	/** A fade is a second or two: the slider covers the first FADE_TRAVEL seconds and a typed entry the whole clip. */
	const FADE_TRAVEL = 5;
	const fadeSpec = (c: Clip): RowSpec => ({
		min: 0,
		max: Math.min(Math.max(clipDuration(c), 0.1), FADE_TRAVEL),
		step: 0.1,
		neutral: 0,
		...SECONDS,
		limit: { max: clipDuration(c) }
	});
	/** The rows that are not sliders share the sliders' label column and end where the sliders' numbers do; a field's unit sits in the keyframe slot. */
	const ROW_GRID = `display:grid;grid-template-columns:72px minmax(0,1fr) ${NUMBER_COL}px 24px;align-items:center;column-gap:8px;padding:3px 0`;
	/** What a fade reads as in its row, so the section's summary says the same. */
	const SECONDS_SPEC: RowSpec = { min: 0, max: 0, step: 0.1, ...SECONDS };
	const count = (n: number, noun: string) => `${n} ${noun}${n === 1 ? '' : 's'}`;
</script>

{#snippet readRow(label: string, value: string)}
	<div style={ROW_GRID}>
		<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">{label}</span>
		<span data-selectable style="grid-column:2 / 4;justify-self:end;white-space:nowrap;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)">{value}</span>
	</div>
{/snippet}

{#snippet numRow(label: string, value: number, step: number, onCommit: (v: number, at: Edit) => void)}
	<label style={ROW_GRID}>
		<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">{label}</span>
		<input
			type="number"
			{value}
			{step}
			min="0"
			{...off}
			onfocus={noteFocus}
			oninput={noteTyped}
			oncompositionend={noteTyped}
			onchange={(e) => {
				const { scope: at, text } = endEdit(e.currentTarget);
				const v = parseFloat(text);
				// Put the field back to what the clip has first: an emptied, negative
				// or clamped entry then doesn't sit there showing something that
				// was ignored, while an accepted one re-renders over this.
				e.currentTarget.value = String(value);
				if (Number.isFinite(v) && v >= 0) onCommit(v, at);
			}}
			style="{inputCss};grid-column:2 / 4;justify-self:end"
		/>
		<span aria-hidden="true" style="font-size:var(--text-sm);color:var(--text-muted)">s</span>
	</label>
{/snippet}

{#snippet timeRow(label: string, value: number, fps: number, onCommit: (v: number, at: Edit) => void)}
	<!-- A time is typed as timecode or as seconds (`parseTime` takes either) and shown on a frame — the number
	     field it replaces showed 4.633333 and took only seconds. -->
	<label style={ROW_GRID}>
		<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">{label}</span>
		<input
			type="text"
			spellcheck="false"
			autocomplete="off"
			value={timeDisplay.field(value, fps)}
			readonly={editor.locked}
			aria-disabled={editor.locked || undefined}
			aria-label={`${label}, as ${timeDisplay.mode === 'seconds' ? 'seconds' : 'timecode or seconds'}`}
			onfocus={noteFocus}
			oninput={noteTyped}
			oncompositionend={noteTyped}
			onchange={(e) => {
				const { scope: at, text } = endEdit(e.currentTarget);
				const v = timeDisplay.parseFrame(text, fps);
				// Put the field back to what the clip has first: a refused entry then doesn't sit there
				// showing something that was ignored, while an accepted one re-renders over this.
				e.currentTarget.value = timeDisplay.field(value, fps);
				if (editor.locked) return;
				if (v === null) toast.error(`“${text.trim()}” is not a time — ${timeDisplay.hint()}`);
				else onCommit(v, at);
			}}
			style="{inputCss};width:112px;grid-column:2 / 4;justify-self:end"
		/>
		<span aria-hidden="true" style="font-size:var(--text-sm);color:var(--text-muted)">{timeDisplay.mode === 'seconds' ? 's' : ''}</span>
	</label>
{/snippet}

{#snippet keyToggle(prop: Property)}
	{@const c = clip}
	{@const keyed = c ? isKeyed(c, prop) : false}
	{@const here = c ? propertyKeys(c, prop).some((k) => Math.abs(k.time - localTime) <= 0.02) : false}
	<IconBtn
		size={24}
		active={here}
		pending={editor.locked}
		title={here
			? `Remove the ${keyLabel(prop)} keyframe at the playhead`
			: keyed
				? { text: `Add a ${keyLabel(prop)} keyframe at the playhead`, detail: 'Right-click to step to the previous or next key.' }
				: `Animate ${keyLabel(prop)}: key it at the playhead`}
		aria-label={`${here ? 'Remove' : 'Add'} a ${keyLabel(prop)} keyframe at the playhead`}
		onclick={(e: MouseEvent) => {
			e.preventDefault();
			toggleKey(prop);
		}}
		oncontextmenu={(e: MouseEvent) => keyMenu(e, prop)}
		style="--k-ink:{keyed ? 'var(--playhead)' : 'var(--text-muted)'};--k-on-ink:var(--playhead)"
	>
		<Icon n="diamond" s={12} style={here ? 'fill:currentColor' : ''} />
	</IconBtn>
{/snippet}

{#snippet rangeRow(
	label: string,
	value: number,
	spec: RowSpec,
	onCommit: (v: number, at: Edit) => unknown,
	animate?: Property
)}
	<SliderRow {label} {value} {spec} {scope} disabled={editor.locked} {onCommit}>
		{#snippet keyframe()}
			{#if animate}{@render keyToggle(animate)}{/if}
		{/snippet}
	</SliderRow>
{/snippet}

{#snippet fxBlock(
	title: string,
	items: Array<Record<string, unknown>>,
	presets: Record<string, unknown>,
	onAdd: (k: string) => void,
	onParam: (i: number, key: string, value: unknown, at: Edit) => void,
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
						use:tip={k}
						{...off}
						onfocus={noteFocus}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(ev) => {
							const { scope: at, text } = endEdit(ev.currentTarget);
							onParam(i, k, text, at);
						}}
						style={fxTxt}
					/>
				{:else}
					<input
						type="number"
						value={v as number}
						use:tip={k}
						step="0.1"
						{...off}
						onfocus={noteFocus}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(ev) => {
							const { scope: at, text } = endEdit(ev.currentTarget);
							const n = parseFloat(text);
							if (Number.isFinite(n)) onParam(i, k, n, at);
						}}
						style={fxNum}
					/>
				{/if}
			{/each}
			<IconBtn
				size={28}
				data-tone="quiet"
				title={`Remove ${String(e.type).replace('_', ' ')}`}
				pending={editor.locked}
				onclick={() => onRemove(i)}
				style="margin-left:auto"><Icon n="x" s={14} /></IconBtn
			>
		</div>
	{/each}
	<select
		class="k-select"
		aria-disabled={editor.locked || undefined}
		onmousedown={swallow}
		onchange={(ev) => {
			const v = ev.currentTarget.value;
			ev.currentTarget.value = '';
			if (v && !editor.locked) onAdd(v);
		}}
		style="width:100%;margin-top:4px"
	>
		<option value="">+ Add effect…</option>
		{#each Object.keys(presets) as k (k)}
			<option value={k}>{k.replace('_', ' ')}</option>
		{/each}
	</select>
	</InspectorSection>

{/snippet}

{#snippet colorField(
	label: string,
	value: string,
	empty: string,
	name: string,
	patch: (typed: string) => Partial<Omit<TextOverlay, 'id' | 'keyframes'>>,
	boxAlpha: number | null
)}
	{@const parsed = parseColor(value)}
	<div style={ROW_GRID}>
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<div style="grid-column:2 / 5;display:flex;align-items:center;gap:6px;min-width:0">
			<input
				type="color"
				class="k-swatch"
				value={swatchHex(value)}
				data-empty={parsed ? undefined : ''}
				aria-disabled={editor.locked || undefined}
				onclick={swallow}
				aria-label="{name}: pick a color"
				onfocus={noteFocus}
				onchange={(e) => patchTitle(e.currentTarget, (picked) => patch(withPicked(value, picked, boxAlpha)), () => swatchHex(value))}
			/>
			<input
				type="text"
				{value}
				placeholder={empty}
				{...off}
				aria-label={name}
				onfocus={noteFocus}
				oninput={noteTyped}
				oncompositionend={noteTyped}
				onchange={(e) => patchTitle(e.currentTarget, (typed) => patch(typed))}
				style="{textCss};flex:1;min-width:0;font-family:var(--font-mono)"
			/>
		</div>
	</div>
{/snippet}

{#snippet overlayEditor(o: TextOverlay)}
	{@const pose = sampleOverlay(o, ui.time)}
	{@const keys = o.keyframes ?? []}
	<div style="display:flex;gap:9px;align-items:center">
		<div
			style="width:40px;height:28px;border-radius:var(--radius-sm);flex:none;background:{o.generated
				? GENERATED_TITLE_FILL
				: 'var(--track-text)'};border:var(--line-width) solid var(--track-text-edge);display:grid;place-items:center;color:var(--text-on-video)"
		>
			<Icon n="captions" s={14} />
		</div>
		<div style="flex:1;min-width:0">
			<div
				style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				use:tip={o.text}
			>
				{o.text || '(empty)'}
			</div>
			<div style="margin-top:3px"><Badge tone="neutral">{o.generated ? 'Caption' : 'Title'} · titles lane</Badge></div>
		</div>
	</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:8px">
		On its own lane, independent of the clips beneath it. Drag it in the preview to move it, or a corner to resize.
	</div>

	<InspectorSection title="Text" summary={timeDisplay.range(o.start, o.end)} open>
		<label style={ROW_GRID}>
			<span style="font-size:12px;color:var(--text-muted)">Text</span>
			<input
				type="text"
				value={o.text}
				{...off}
				onfocus={noteFocus}
				oninput={noteTyped}
				oncompositionend={noteTyped}
				onchange={(e) => patchTitle(e.currentTarget, (typed) => ({ text: typed }))}
				style="{textCss};grid-column:2 / 5;min-width:0"
			/>
		</label>
		{@render timeRow('Start', o.start, editor.fps, (v, at) => {
			const ov = overlayOf(at);
			if (ov) void run(() => editor.retimeOverlay(ov.id, Math.min(v, Math.max(0, ov.end - MIN_TITLE)), ov.end));
		})}
		{@render timeRow('End', o.end, editor.fps, (v, at) => {
			const ov = overlayOf(at);
			if (ov) void run(() => editor.retimeOverlay(ov.id, ov.start, Math.max(v, ov.start + MIN_TITLE)));
		})}
		{@render rangeRow('Position X', pose.x, { min: 0, max: 1, step: 0.01 }, (v, at) => moveTitle(at, { x: v }))}
		{@render rangeRow('Position Y', pose.y, { min: 0, max: 1, step: 0.01 }, (v, at) => moveTitle(at, { y: v }))}
		{@render rangeRow('Size', o.size, { min: TITLE_SIZE_MIN, max: Math.max(0.2, TITLE_SIZE_MAX, o.size), step: 0.005, ...PERCENT, digits: 1 }, (v, at) => {
			const ov = overlayOf(at);
			if (ov) return run(() => editor.updateOverlay(ov.id, { size: v }));
		})}
		{@render colorField('Color', o.color, 'white', 'Title color', (typed) => ({ color: typed }), null)}
		{@render colorField('Box', o.bg ?? '', 'none', 'Box behind the title', (typed) => ({ bg: typed }), 0.55)}
		<label style={ROW_GRID}>
			<span style="font-size:12px;color:var(--text-muted)">Font</span>
			<select
				class="k-select"
				value={o.font ?? ''}
				aria-disabled={editor.locked || undefined}
				onmousedown={swallow}
				onfocus={noteFocus}
				onchange={(e) => patchTitle(e.currentTarget, (typed) => ({ font: typed }), (t) => t.font ?? '')}
				style="grid-column:2 / 5"
			>
				<option value="">Default</option>
				{#each ui.availableFonts as f (f)}
					<option value={f}>{f}</option>
				{/each}
			</select>
		</label>
		<CheckRow
			checked={o.bold}
			pending={editor.locked}
			onfocus={noteFocus}
			onchange={(e) => patchTitle(e.currentTarget, () => ({ bold: e.currentTarget.checked }))}
		>
			Bold
		</CheckRow>
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
					<IconBtn
						size={28}
						data-tone="quiet"
						title={`Remove the keyframe at ${k.time.toFixed(2)}s`}
						pending={editor.locked}
						onclick={() => removeOverlayKeyframe(o, i)}><Icon n="x" s={14} /></IconBtn
					>
				</div>
			{/each}
			<Btn
				size="sm"
				style="margin-top:4px"
				pending={editor.locked}
				onclick={() => run(() => editor.setOverlayKeyframes(o.id, []))}>Clear keyframes</Btn
			>
			<div style="font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4">
				This title is animated: moving it in the preview or with Position X / Position Y keyframes the position at the playhead.
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
			data-quiet
			size="sm"
			icon="trash"
			iconSize={13}
			style="width:100%"
			pending={editor.locked}
			onclick={() => run(() => editor.removeOverlay(o.id))}>Remove {o.generated ? 'caption' : 'title'}</Btn
		>
	</div>
{/snippet}

{#snippet presetColor(label: string, o: SvgOverlay, key: 'color' | 'accent')}
	{@const value = o.preset?.params[key] ?? ''}
	{@const parsed = parseColor(value)}
	<div style={ROW_GRID}>
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<div style="grid-column:2 / 5;display:flex;align-items:center;gap:6px;min-width:0">
			<input
				type="color"
				class="k-swatch"
				value={swatchHex(value)}
				data-empty={parsed ? undefined : ''}
				aria-disabled={editor.locked || undefined}
				onclick={swallow}
				aria-label="{label}: pick a color"
				onfocus={noteFocus}
				onchange={(e) =>
					patchSvg(
						e.currentTarget,
						(picked, t) => presetPatch(t, { [key]: withPicked(t.preset?.params[key], picked) }),
						(t) => t.preset?.params[key] ?? ''
					)}
			/>
			<input
				type="text"
				{value}
				{...off}
				aria-label={label}
				onfocus={noteFocus}
				oninput={noteTyped}
				oncompositionend={noteTyped}
				onchange={(e) => patchSvg(e.currentTarget, (typed, t) => presetPatch(t, { [key]: typed }), (t) => t.preset?.params[key] ?? '')}
				style="{textCss};flex:1;min-width:0;font-family:var(--font-mono)"
			/>
		</div>
	</div>
{/snippet}

{#snippet svgEditor(o: SvgOverlay)}
	<div style="display:flex;gap:9px;align-items:center">
		<div
			style="width:40px;height:28px;border-radius:var(--radius-sm);flex:none;background:{SVG_TITLE_FILL};border:var(--line-width) solid var(--track-video-edge);display:grid;place-items:center;color:var(--text-on-video)"
		>
			<Icon n="orbit" s={14} />
		</div>
		<div style="flex:1;min-width:0">
			<div
				style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				use:tip={svgDisplayName(o)}
			>
				{svgDisplayName(o)}
			</div>
			<div style="margin-top:3px"><Badge tone="neutral">Animated title · {svgSourceSummary(o)}</Badge></div>
		</div>
	</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:8px">
		On the titles lane, drawn over the clips and under the text titles. {isVisibleNow(o)
			? 'Drag its box in the preview to move it, or a corner to resize.'
			: 'Move the playhead into it to place it in the preview.'}
	</div>

	{#if o.template}<TemplateSection {o} />{/if}

	<InspectorSection title="Timing" summary={timingSummary(o.start, o.end - o.start)} open>
		<label style={ROW_GRID}>
			<span style="font-size:12px;color:var(--text-muted)">Name</span>
			<input
				type="text"
				value={o.name}
				placeholder={svgDisplayName({ name: '', preset: o.preset })}
				{...off}
				onfocus={noteFocus}
				oninput={noteTyped}
				oncompositionend={noteTyped}
				onchange={(e) => patchSvg(e.currentTarget, (typed) => ({ name: typed }), (t) => t.name)}
				style="{textCss};grid-column:2 / 5;min-width:0"
			/>
		</label>
		{@render numRow('Start', o.start, 0.1, (v, at) => {
			const t = svgOf(at);
			if (t) void run(() => editor.updateSvgOverlay(t.id, { start: Math.min(v, Math.max(0, t.end - MIN_TITLE)) }));
		})}
		{@render numRow('End', o.end, 0.1, (v, at) => {
			const t = svgOf(at);
			if (t) void run(() => editor.updateSvgOverlay(t.id, { end: Math.max(v, t.start + MIN_TITLE) }));
		})}
	</InspectorSection>

	<InspectorSection title="Placement" summary={svgPlacementSummary(o)} open>
		{@render rangeRow('X', o.x, { min: 0, max: 1, step: 0.01, neutral: 0, ...PERCENT, limit: { min: -5, max: 5 } }, (v, at) => {
			const t = svgOf(at);
			if (t) return run(() => editor.updateSvgOverlay(t.id, { x: v }));
		})}
		{@render rangeRow('Y', o.y, { min: 0, max: 1, step: 0.01, neutral: 0, ...PERCENT, limit: { min: -5, max: 5 } }, (v, at) => {
			const t = svgOf(at);
			if (t) return run(() => editor.updateSvgOverlay(t.id, { y: v }));
		})}
		{@render rangeRow('Width', o.width, { min: 0.02, max: 1, step: 0.01, neutral: 1, ...PERCENT, limit: { min: 0.01, max: 10 } }, (v, at) => {
			const t = svgOf(at);
			if (t) return run(() => editor.updateSvgOverlay(t.id, { width: v }));
		})}
		{@render rangeRow('Height', o.height, { min: 0.02, max: 1, step: 0.01, neutral: 1, ...PERCENT, limit: { min: 0.01, max: 10 } }, (v, at) => {
			const t = svgOf(at);
			if (t) return run(() => editor.updateSvgOverlay(t.id, { height: v }));
		})}
		{@render rangeRow('Opacity', o.opacity, { min: 0, max: 1, step: 0.01, neutral: 1, ...PERCENT }, (v, at) => {
			const t = svgOf(at);
			if (t) return run(() => editor.updateSvgOverlay(t.id, { opacity: v }));
		})}
		<div style="font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4">
			The artwork is fitted inside this rectangle, centred, keeping its shape.
		</div>
	</InspectorSection>

	{#if o.preset}
		{@const preset = o.preset}
		<InspectorSection title="Preset" summary={svgSourceSummary(o)} open>
			<div role="group" aria-label="Preset" style="display:flex;flex-wrap:wrap;gap:5px;margin-bottom:6px">
				{#each SVG_PRESETS as p (p.kind)}
					<Chip
						pressed={preset.kind === p.kind}
						title={p.hint}
						pending={editor.locked}
						onclick={() => void changePreset(o, p.kind)}>{p.label}</Chip
					>
				{/each}
			</div>
			<label style={ROW_GRID}>
				<span style="font-size:12px;color:var(--text-muted)">Text</span>
				<input
					type="text"
					value={preset.params.text}
					{...off}
					onfocus={noteFocus}
					oninput={noteTyped}
					oncompositionend={noteTyped}
					onchange={(e) => patchSvg(e.currentTarget, (typed, t) => presetPatch(t, { text: typed }), (t) => t.preset?.params.text ?? '')}
					style="{textCss};grid-column:2 / 5;min-width:0"
				/>
			</label>
			{#if takesParam(presetInfo, 'subtitle')}
				<label style={ROW_GRID}>
					<span style="font-size:12px;color:var(--text-muted)">Subtitle</span>
					<input
						type="text"
						value={preset.params.subtitle ?? ''}
						placeholder="none"
						{...off}
						onfocus={noteFocus}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(e) =>
							patchSvg(
								e.currentTarget,
								(typed, t) => presetPatch(t, { subtitle: typed.trim() === '' ? null : typed }),
								(t) => t.preset?.params.subtitle ?? ''
							)}
						style="{textCss};grid-column:2 / 5;min-width:0"
					/>
				</label>
			{/if}
			{#if takesParam(presetInfo, 'color')}{@render presetColor('Color', o, 'color')}{/if}
			{#if takesParam(presetInfo, 'accent')}{@render presetColor('Accent', o, 'accent')}{/if}
			{#if takesParam(presetInfo, 'font')}
				<label style={ROW_GRID}>
					<span style="font-size:12px;color:var(--text-muted)">Font</span>
					<select
						class="k-select"
						value={preset.params.font ?? ''}
						aria-disabled={editor.locked || undefined}
						onmousedown={swallow}
						onfocus={noteFocus}
						onchange={(e) =>
							patchSvg(e.currentTarget, (typed, t) => presetPatch(t, { font: typed === '' ? null : typed }), (t) => t.preset?.params.font ?? '')}
						style="grid-column:2 / 5"
					>
						<option value="">Default</option>
						{#each ui.availableFonts as f (f)}
							<option value={f}>{f}</option>
						{/each}
						{#if preset.params.font && !ui.availableFonts.includes(preset.params.font)}
							<option value={preset.params.font}>{preset.params.font}</option>
						{/if}
					</select>
				</label>
			{/if}
			<div style="font-size:12px;color:var(--text-muted);margin:6px 0;line-height:1.4">
				Changing the preset makes a new title with the same text, timing and placement. Convert to custom to write the SVG yourself.
			</div>
			<Btn size="sm" icon="code-xml" pending={editor.locked} onclick={() => void convertToCustom(o)}>Convert to custom</Btn>
		</InspectorSection>
	{:else}
		<InspectorSection title="SVG source" summary={draftDirty ? 'Edited' : o.template ? 'Template' : 'Custom'} open={!o.template}>
			<textarea
				bind:this={sourceEl}
				bind:value={draft}
				spellcheck="false"
				autocomplete="off"
				aria-label="SVG source"
				rows="12"
				style="{FIELD};display:block;width:100%;height:auto;min-height:160px;resize:vertical;padding:8px 10px;font-family:var(--font-mono);font-size:11px;line-height:1.5;white-space:pre;overflow:auto"
			></textarea>
			<div style="display:flex;align-items:center;gap:7px;flex-wrap:wrap;margin-top:6px">
				<Btn size="sm" variant="primary" disabled={!draftDirty || applying || editor.busy} onclick={() => void applySource()}>Apply</Btn>
				<Btn size="sm" variant="ghost" disabled={!draftDirty || applying} onclick={revertSource}>Revert</Btn>
				{#if draftDirty}<span style="font-size:12px;color:var(--text-muted)">Not applied yet</span>{/if}
			</div>
			{#if sourceError}
				<div
					role="alert"
					data-svg-error
					style="margin-top:6px;font-size:12px;line-height:1.4;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:6px 8px"
				>
					{sourceError}
				</div>
			{/if}
			{#if sourceWarnings.length}
				<ul
					data-svg-warnings
					style="margin:6px 0 0;padding:6px 8px 6px 22px;font-size:12px;line-height:1.4;color:var(--orange-400);background:var(--warning-surface);border-radius:var(--radius-sm)"
				>
					{#each sourceWarnings as w, i (i)}<li>{w}</li>{/each}
				</ul>
			{/if}
			{#if sourceRuns !== null}
				<div style="font-size:12px;color:var(--text-muted);margin-top:6px;line-height:1.4">
					The animation runs {sourceRuns.toFixed(1)} s; this title lasts {(o.end - o.start).toFixed(1)} s.
				</div>
			{/if}
			<div style="font-size:12px;color:var(--text-muted);margin-top:6px;line-height:1.4">
				SMIL animation (animate, set, animateTransform) is drawn; scripts, external images and CSS animations are not. Make the
				viewBox the frame. Times count from the start of the title.
			</div>
		</InspectorSection>
	{/if}

	{#if !o.template && !o.preset}<TemplateSection {o} />{/if}

	<div style="margin-top:18px">
		<Btn
			variant="destructive"
			data-quiet
			size="sm"
			icon="trash"
			iconSize={13}
			style="width:100%"
			pending={editor.locked}
			onclick={() => run(() => editor.removeSvgOverlay(o.id))}>Remove animated title</Btn
		>
	</div>
{/snippet}

<div
	role="presentation"
	oncontextmenu={onInspectorContextMenu}
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>

	<!-- While the agent's proposal is previewed the cut on screen is not the project's, so what is selected here is
	     the proposal's clip and nothing in this panel may edit it: it is `inert` (not reachable by pointer or key),
	     and says why. -->
	<div inert={editor.previewingStaged} style="flex:1;overflow-y:auto;padding:12px">
		{#if editor.previewingStaged && (overlay || svgTitle || clip)}
			<div
				data-preview-readonly
				style="margin-bottom:10px;padding:6px 9px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--agent-border);background:var(--agent-surface);font-size:11px;line-height:1.4;color:var(--agent-300)"
			>
				Previewing the agent's proposal — this is its clip, so the Inspector is read-only. Exit the preview to edit.
			</div>
		{/if}
		{#if overlay}
			{@render overlayEditor(overlay)}
		{:else if svgTitle}
			{@render svgEditor(svgTitle)}
		{:else if clip}
			{#if selectionUnits(editor.timeline, editor.selectedClipIds) > 1}
				<!-- Several clips are selected but an edit here acts on one. Say which,
				     rather than let a fader look like it moves them all. A picture and its
				     linked sound are one selection: that is what a click on either gives. -->
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
					style="width:40px;height:28px;border-radius:var(--radius-sm);flex:none;background:{picture
						? VIDEO_THUMB_BG
						: 'var(--track-audio)'};display:grid;place-items:center;color:color-mix(in srgb,var(--text-on-video) 80%,transparent)"
				>
					<Icon n={picture ? 'video' : 'audio-waveform'} s={14} />
				</div>
				<div style="flex:1;min-width:0">
					<div
						style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
						use:tip={asset?.name ?? 'clip'}
					>
						{asset?.name ?? 'clip'}
					</div>
					<div style="margin-top:3px;display:flex;align-items:center;gap:5px;flex-wrap:wrap">
						<Badge tone="neutral">{track?.name ?? (picture ? 'video' : 'audio')}</Badge>
						{#if linkedTo.length > 0}
							<span use:tip={linkInfo?.title}>
								<Badge tone="neutral"><Icon n="link" s={10} />Linked to {linkedTo.join(', ')}</Badge>
							</span>
						{/if}
					</div>
				</div>
			</div>
			{#if asset}
				<!-- Which analyses have run for this clip's media, and a way to run one: the same
				     chips as the bin, and the same Analyze menu (the work is the asset's, not the clip's). -->
				<div style="margin-top:8px;display:flex;align-items:center;gap:8px;flex-wrap:wrap">
					<MediaChips assetId={asset.id} />
					{#if ui.analyzing && ui.analyzingId === asset.id}
						<div style="flex-basis:100%"><AnalysisRun /></div>
					{:else}
						<Btn
							size="sm"
							variant="secondary"
							icon="scan-line"
							onclick={(e: MouseEvent) => {
								const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
								contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), analyzeMenuItems(asset));
							}}>Analyze</Btn
						>
					{/if}
				</div>
			{/if}

			<InspectorSection title="Timing" summary={timingSummary(clip.timeline_start, clipDuration(clip), (s) => timeDisplay.fmt(s))} open>
			{@render readRow('Start', timeDisplay.fmt(clip.timeline_start))}
			{@render readRow('Duration', timeDisplay.fmt(clipDuration(clip)))}

			<div class="k-overline" style="margin:12px 0 4px">Trim source</div>
			{@render timeRow('In', clip.source_in, sourceFps, (v, at) => {
				const c = clipOf(at);
				if (c) void run(() => trimChecked(c.id, v, undefined));
			})}
			{@render timeRow('Out', clip.source_out, sourceFps, (v, at) => {
				const c = clipOf(at);
				if (c) void run(() => trimChecked(c.id, undefined, v));
			})}
			</InspectorSection>

			{#if asset?.music_score}
				<InspectorSection title="Composed music" summary={scoreTitle(asset.music_score)} open>
				<ComposedMusic {asset} {clip} {track} />
				</InspectorSection>
			{/if}

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
				<InspectorSection
					title="Volume"
					summary={volumeAnimated(clip) ? `Animated · ${Math.round(gain * 100)}%` : `${Math.round(clip.volume * 100)}%`}
					open
				>
				{@render rangeRow('Level', gain, { min: 0, max: MAX_GAIN, step: 0.05, neutral: 1, ...PERCENT }, setGain, 'volume')}
				</InspectorSection>

			{/if}

			{#if musicOffer.show && music}
				<InspectorSection title="Music" summary={musicSummary(music)}>
				<div style="margin-bottom:6px">
					<Btn
						size="sm"
						icon="film"
						pending={editor.locked} disabled={musicOffer.reason !== null}
						title={'Make this music last as long as the picture, repeating or dropping whole phrases'}
						onclick={() => ui.openFitMusic(clip.id)}>Fit to video…</Btn
					>
				</div>
				<div style="font:var(--type-caption);color:var(--text-muted)">
					{musicOffer.reason ??
						'Repeats or drops whole phrases on bar lines, keeping the intro and the ending, until the music lasts as long as the picture.'}
				</div>
				<div style="margin-top:10px"><MusicStructure assetId={clip.asset_id} {music} /></div>
				</InspectorSection>
			{/if}

			<InspectorSection title="Fades" summary={`${readout(clip.fade_in, SECONDS_SPEC)} in · ${readout(clip.fade_out, SECONDS_SPEC)} out`}>
			{@render rangeRow('Fade in', clip.fade_in, fadeSpec(clip), (v, at) => setFade(at, { fade_in: v }))}
			{@render rangeRow('Fade out', clip.fade_out, fadeSpec(clip), (v, at) => setFade(at, { fade_out: v }))}
			</InspectorSection>

			<InspectorSection title="Speed" summary={`${Math.abs(speed).toFixed(2)}×${speed < 0 ? ' · reverse' : ''}`}>
			{@render rangeRow('Rate', Math.abs(speed), { min: 0.25, max: 4, step: 0.25, neutral: 1, ...TIMES }, (v, at) => {
				const c = clipOf(at);
				if (c) return run(() => editor.setSpeed(c.id, (c.speed ?? 1) < 0 ? -v : v));
			})}
			<CheckRow checked={speed < 0} pending={editor.locked} onchange={() => run(() => editor.setSpeed(clip.id, -speed))}>
				Reverse
			</CheckRow>
			</InspectorSection>

			{#if picture && (reframe || sourceProjection)}
				<InspectorSection title="360 reframe" summary={reframe ? (reframeKeys.length ? `Animated · ${count(reframeKeys.length, 'key')}` : 'On') : 'Off'}>
				{#if reframe}
					{@render rangeRow('Yaw', cam.yaw, { min: -180, max: 180, step: 1, neutral: DEFAULT_REFRAME.yaw, ...DEGREES }, (v, at) =>
						setCam({ yaw: v }, at))}
					{@render rangeRow('Pitch', cam.pitch, { min: -90, max: 90, step: 1, neutral: DEFAULT_REFRAME.pitch, ...DEGREES }, (v, at) =>
						setCam({ pitch: v }, at))}
					{@render rangeRow('Roll', cam.roll, { min: -180, max: 180, step: 1, neutral: DEFAULT_REFRAME.roll, ...DEGREES }, (v, at) =>
						setCam({ roll: v }, at))}
					{@render rangeRow('FOV', cam.fov, { min: 20, max: 180, step: 1, neutral: DEFAULT_REFRAME.fov, ...DEGREES }, (v, at) =>
						setCam({ fov: v }, at))}
					{#if reframe.input === 'dual_fisheye'}
						{@render rangeRow('Lens FOV', cam.lens_fov, { min: 170, max: 220, step: 1, neutral: DEFAULT_REFRAME.lens_fov, ...DEGREES }, (v, at) => {
							const c = clipOf(at);
							if (c) return run(() => editor.setReframe(c.id, { lens_fov: v }));
						})}
						<p style="font-size:12px;color:var(--text-muted);margin:4px 0 0;line-height:1.4">
							Approximate stitch — the seam is a hard blend, not Insta360's optical-flow
							one. Tune Lens FOV to move it, or use a Studio equirect export for a clean
							join.
						</p>
					{/if}
					<div style="display:flex;flex-wrap:wrap;gap:6px;padding:6px 0 0">
						<Btn
							size="sm"
							icon="diamond"
							iconSize={12}
							pending={editor.locked}
							onclick={() =>
								run(() =>
									editor.addReframeKeyframe(
										clip.id,
										Math.round(Math.max(0, ui.time - clip.timeline_start) * 1000) / 1000
									)
								)}>Camera key</Btn
						>
						{#if reframeKeys.length}
							<Btn
								size="sm"
								pending={editor.locked}
								onclick={() => run(() => editor.setReframeKeyframes(clip.id, []))}
								>Clear {reframeKeys.length} key{reframeKeys.length === 1 ? '' : 's'}</Btn
							>
						{/if}
						<Btn size="sm" pending={editor.locked} onclick={() => run(() => editor.clearReframe(clip.id))}>Stop reframing</Btn>
					</div>
				{:else if sourceProjection}
					<p style="font-size:12px;color:var(--text-muted);margin:0 0 6px;line-height:1.4">
						360 source ({sourceProjection === 'dual_fisheye' ? 'dual fisheye' : sourceProjection}),
						shown raw.
					</p>
					<Btn
						size="sm"
						pending={editor.locked}
						onclick={() => run(() => editor.setReframe(clip.id, { input: sourceProjection! }))}
						>Reframe to flat</Btn
					>
				{/if}
				</InspectorSection>

			{/if}

			{#if picture}
				<InspectorSection title="Transform" summary={keyframes.length ? `Animated · ${count(keyframes.length, 'key')}` : `${Math.round(tf.scale * 100)}% · ${Math.round(tf.rotation)}°`}>
				{@render rangeRow('Scale', tf.scale, { min: 0.1, max: 2, step: 0.05, neutral: DEFAULT_TRANSFORM.scale, ...PERCENT }, (v, at) =>
					setTf({ scale: v }, at))}
				{@render rangeRow('Position X', tf.pos_x, { min: -0.5, max: 0.5, step: 0.01, neutral: DEFAULT_TRANSFORM.pos_x }, (v, at) =>
					setTf({ pos_x: v }, at))}
				{@render rangeRow('Position Y', tf.pos_y, { min: -0.5, max: 0.5, step: 0.01, neutral: DEFAULT_TRANSFORM.pos_y }, (v, at) =>
					setTf({ pos_y: v }, at))}
				{@render rangeRow('Rotation', tf.rotation, { min: -180, max: 180, step: 1, neutral: DEFAULT_TRANSFORM.rotation, ...DEGREES }, (v, at) =>
					setTf({ rotation: v }, at))}
				{@render rangeRow('Opacity', tf.opacity, { min: 0, max: 1, step: 0.05, neutral: DEFAULT_TRANSFORM.opacity, ...PERCENT }, (v, at) =>
					setTf({ opacity: v }, at))}
				</InspectorSection>

				<InspectorSection title="Framing" summary={hasCrop ? 'Cropped' : 'Full frame'}>
				<div style="display:flex;gap:6px;flex-wrap:wrap;align-items:center;margin-bottom:6px">
					<Btn
						size="sm"
						icon="crop"
						iconSize={12}
						pending={editor.locked} disabled={!reframable}
						title={"Sample where this shot's content is and crop to the delivery frame around it"}
						onclick={() =>
							runUndoable('Framed for the delivery frame', () => editor.smartCrop(clip.id))}
						>Smart crop</Btn
					>
					<Btn
						size="sm"
						pending={editor.locked} disabled={!hasCrop}
						title={'Clear the crop'}
						onclick={() =>
							run(() =>
								editor.setTransform(clip.id, {
									crop_left: 0,
									crop_right: 0,
									crop_top: 0,
									crop_bottom: 0
								})
							)}>Reset crop</Btn
					>
				</div>
				{#if !reframable}
					<div style="font:var(--type-caption);color:var(--text-muted);margin:-2px 0 6px">
						{clip.reframe
							? 'A 360 clip is framed by its virtual camera, above.'
							: 'This shot already matches the delivery frame.'}
					</div>
				{/if}
				{@render rangeRow('Left', tf.crop_left, { min: 0, max: 0.9, step: 0.01, neutral: DEFAULT_TRANSFORM.crop_left }, (v, at) => setTf({ crop_left: v }, at))}
				{@render rangeRow('Right', tf.crop_right, { min: 0, max: 0.9, step: 0.01, neutral: DEFAULT_TRANSFORM.crop_right }, (v, at) => setTf({ crop_right: v }, at))}
				{@render rangeRow('Top', tf.crop_top, { min: 0, max: 0.9, step: 0.01, neutral: DEFAULT_TRANSFORM.crop_top }, (v, at) => setTf({ crop_top: v }, at))}
				{@render rangeRow('Bottom', tf.crop_bottom, { min: 0, max: 0.9, step: 0.01, neutral: DEFAULT_TRANSFORM.crop_bottom }, (v, at) => setTf({ crop_bottom: v }, at))}
				</InspectorSection>

				<InspectorSection title="Color" summary={colorSummary(clip, col)}>
				{@render rangeRow('Brightness', col.brightness, { min: -1, max: 1, step: 0.05, neutral: DEFAULT_COLOR.brightness }, (v, at) => setCol({ brightness: v }, at), 'brightness')}
				{@render rangeRow('Contrast', col.contrast, { min: 0, max: 4, step: 0.05, neutral: DEFAULT_COLOR.contrast }, (v, at) => setCol({ contrast: v }, at), 'contrast')}
				{@render rangeRow('Saturation', col.saturation, { min: 0, max: 3, step: 0.05, neutral: DEFAULT_COLOR.saturation }, (v, at) => setCol({ saturation: v }, at), 'saturation')}
				{@render rangeRow('Warmth', col.temperature ?? 0, { min: -1, max: 1, step: 0.05, neutral: DEFAULT_COLOR.temperature ?? 0 }, (v, at) => setCol({ temperature: v }, at), 'temperature')}
				{@render rangeRow('Gamma', col.gamma, { min: 0.1, max: 3, step: 0.05, neutral: DEFAULT_COLOR.gamma }, (v, at) => setCol({ gamma: v }, at), 'gamma')}
				<div style="display:flex;justify-content:flex-end;margin-top:4px">
					<Btn size="sm" pending={editor.locked} title={'Back to the ungraded picture'} onclick={() => setCol(DEFAULT_COLOR)}>Reset color</Btn>
				</div>
				</InspectorSection>

				<GradeSections {clip} />

			<InspectorSection title="Mask" summary={mask ? (mask.shape === 'rect' ? 'Rectangle' : 'Ellipse') : 'None'}>
			<div style="display:flex;gap:5px;flex-wrap:wrap;align-items:center;margin-bottom:6px">
				<Chip
					pressed={!mask}
					pending={editor.locked}
					title={'No mask — the whole frame'}
					onclick={() => run(() => editor.setMask(clip.id, null))}>None</Chip
				>
				<Chip
					pressed={mask?.shape === 'rect'}
					pending={editor.locked}
					title={'Cut this clip to a rectangle'}
					onclick={() => patchMask({ shape: 'rect' })}>Rectangle</Chip
				>
				<Chip
					pressed={mask?.shape === 'ellipse'}
					pending={editor.locked}
					title={'Cut this clip to an ellipse'}
					onclick={() => patchMask({ shape: 'ellipse' })}>Ellipse</Chip
				>
			</div>
			{#if mask}
				{@render rangeRow('Center X', mask.x, { min: 0, max: 1, step: 0.01, neutral: DEFAULT_MASK.x, bipolar: true }, (v, at) => patchMask({ x: v }, at))}
				{@render rangeRow('Center Y', mask.y, { min: 0, max: 1, step: 0.01, neutral: DEFAULT_MASK.y, bipolar: true }, (v, at) => patchMask({ y: v }, at))}
				{@render rangeRow('Width', mask.width, { min: 0.02, max: 1.5, step: 0.01, neutral: DEFAULT_MASK.width }, (v, at) =>
					patchMask({ width: v }, at))}
				{@render rangeRow('Height', mask.height, { min: 0.02, max: 1.5, step: 0.01, neutral: DEFAULT_MASK.height }, (v, at) =>
					patchMask({ height: v }, at))}
				{@render rangeRow('Feather', mask.feather, { min: 0, max: 1, step: 0.01, neutral: DEFAULT_MASK.feather }, (v, at) =>
					patchMask({ feather: v }, at))}
				<CheckRow checked={!!mask.inverted} pending={editor.locked} onchange={(e) => patchMask({ inverted: e.currentTarget.checked })}>
					Invert
				</CheckRow>
				<div style="font:var(--type-caption);color:var(--text-muted);margin:2px 0 6px">
					Outside the shape this clip is transparent, so a lower track shows through. To blur a
					face: duplicate the shot onto the track above, blur the copy, mask the copy.
				</div>
			{/if}
			</InspectorSection>
			{/if}

			<InspectorSection title="Transition" summary={transition ? `${transition.kind.replaceAll('_', ' ')} · ${transition.duration}s` : 'None'}>
			<label style={ROW_GRID}>
				<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">Into clip</span>
				<select
					class="k-select"
					value={transition?.kind ?? ''}
					aria-disabled={editor.locked || undefined}
					onmousedown={swallow}
						onchange={(e) => {
						if (editor.locked) return;
						const k = e.currentTarget.value as '' | TransitionKind;
						if (!k) void run(() => editor.setTransition(clip.id, null));
						else void run(() => editor.setTransition(clip.id, { kind: k, duration: transition?.duration ?? DEFAULT_TRANSITION_SECONDS }));
					}}
					style="grid-column:2 / 5"
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
				{@render numRow('Duration', transition.duration, 0.1, (v, at) => {
					const c = clipOf(at);
					const was = c?.transition_in;
					if (c && was) void run(() => editor.setTransition(c.id, { kind: was.kind, duration: Math.max(0.05, v) }));
				})}
			{/if}
			</InspectorSection>

			{#if hasAudio && !soundDetached}
					{@render fxBlock('Audio effects', audioFx, AUDIO_FX, addAudioFx, setAudioFxParam, removeAudioFx)}
				{/if}

				{#if picture}
					{@render fxBlock('Video effects', effects, VIDEO_FX, addVideoFx, setVideoFxParam, removeVideoFx)}

					<InspectorSection title="Animation" summary={count(keyframes.length, 'keyframe')}>
					{@const offer = keyframeOffer(editor.timeline, editor.keyframeSource, clip.id)}
					<div style="display:flex;gap:7px;margin-bottom:6px">
						<Btn
							size="sm"
							icon="copy"
							iconSize={12}
							style="flex:1"
							pending={editor.locked || offer.copy.disabled}
							title={offer.copy.reason ?? 'Remember this clip’s animation, to paste onto another clip'}
							onclick={() => {
								if (!offer.copy.disabled) copyKeyframesOf(clip.id);
							}}>Copy keyframes</Btn
						>
						<Btn
							size="sm"
							icon="clipboard"
							iconSize={12}
							style="flex:1"
							pending={editor.locked || offer.paste.disabled}
							title={offer.paste.reason ?? 'Give this clip the copied clip’s animation (replaces the numbers it keys)'}
							onclick={() => {
								if (!offer.paste.disabled) void pasteKeyframesOnto(clip.id);
							}}>Paste keyframes</Btn
						>
					</div>
					<div style="display:flex;gap:7px;margin-bottom:4px">
						<Btn size="sm" icon="diamond" iconSize={12} style="flex:1" pending={editor.locked} onclick={addKeyframeHere}
							>Keyframe at playhead</Btn
						>
						{#if keyframes.length}
							<Btn
								size="sm"
								pending={editor.locked}
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
								{#if i < keyframes.length - 1}
									{@const current = easingId(k.easing)}
									<select
										class="k-select"
										value={current}
										aria-disabled={editor.locked || undefined}
										onmousedown={swallow}
																use:tip={'How the clip moves from this keyframe to the next'}
										aria-label={`Easing after the keyframe at ${k.time.toFixed(2)} seconds`}
										onchange={(e) => setEasing(e.currentTarget, k, current)}
										style="flex:none;width:104px"
									>
										{#each EASING_CHOICES as e (e.id)}
											<option value={e.id}>{e.label}</option>
										{/each}
										{#if current === 'custom'}
											<option value="custom" disabled>Custom</option>
										{/if}
									</select>
								{/if}
								<IconBtn
									size={28}
									data-tone="quiet"
									title={`Remove keyframe ${i + 1}`}
									pending={editor.locked}
									onclick={() => removeKeyframe(i)}><Icon n="x" s={14} /></IconBtn
								>
							</div>
						{/each}
						<div style="font-size:12px;color:var(--text-muted);margin-top:4px;line-height:1.4">
							Move the playhead and adjust Transform above to keyframe scale / position / rotation / opacity over time.
						</div>
					{/if}
					</InspectorSection>

					{#if !reframe && !sourceProjection}
						<!-- Kerf detects 360 footage from its tags, so a flat clip has nothing to say about it. The
						     override for footage it missed (a remux that lost the tag) is here, out of the way. -->
						<InspectorSection title="Advanced" summary="Not 360">
						<p style="font-size:12px;color:var(--text-muted);margin:0 0 6px;line-height:1.4">
							Kerf did not detect this as 360 footage. If it really is spherical, pick how the source is
							packed — the whole asset is marked, so every clip cut from it reframes.
						</p>
						<label style={ROW_GRID}>
							<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">Source is</span>
							<select
								class="k-select"
								value={manualProjection}
								aria-disabled={editor.locked || undefined}
								onmousedown={swallow}
						onchange={(e) => {
							if (editor.locked) e.currentTarget.value = manualProjection;
							else manualProjection = e.currentTarget.value as Projection;
						}}
								style="grid-column:2 / 5"
							>
								<option value="equirect">Equirectangular</option>
								<option value="dual_fisheye">Dual fisheye</option>
								<option value="fisheye">Fisheye</option>
							</select>
						</label>
						<div style="margin-top:6px">
							<Btn
								size="sm"
								icon="globe"
								pending={editor.locked}
								onclick={() =>
									run(async () => {
										await editor.setAssetProjection(clip.asset_id, manualProjection);
										await editor.setReframe(clip.id, { input: manualProjection });
									})}>This is 360 footage</Btn
							>
						</div>
						</InspectorSection>
					{/if}

				{/if}

				<div style="margin-top:18px;display:grid;grid-template-columns:1fr 1fr;gap:6px">
				<Btn
					variant="destructive"
					data-quiet
					size="sm"
					icon="trash"
					iconSize={13}
					pending={editor.locked}
					onclick={() => runUndoable('Clip removed', () => editor.remove(clip.id))}>Remove clip</Btn
				>
				<Btn
					size="sm"
					pending={editor.locked}
					title={'Remove the clip and close the gap it leaves'}
					onclick={() => runUndoable('Clip ripple-deleted', () => editor.rippleDelete(clip.id))}
					>Ripple delete</Btn
				>
			</div>
		{:else if editor.selectedAsset}
			<!-- Nothing on the timeline is selected, but the library has an asset: its facts are here, not in the
			     bin's right-click menu. -->
			<AssetInspector asset={editor.selectedAsset} />
		{:else}
			<EmptyState
				icon="sliders-horizontal"
				pad={40}
				title="Select a clip, a title or a file to inspect it"
				hint="Titles are added from the library’s Titles tab or the timeline’s titles lane."
			/>
		{/if}
	</div>
</div>
