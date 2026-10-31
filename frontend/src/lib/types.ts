// Mirrors the kerf-core domain model serialized over Tauri / MCP.

export type StreamKind = 'video' | 'audio' | 'subtitle' | 'data';

export interface StreamInfo {
	index: number;
	kind: StreamKind;
	codec: string;
	width?: number;
	height?: number;
	fps?: number;
	sample_rate?: number;
	channels?: number;
	/** True for a single-frame still image (looped, not seeked, on export). */
	image?: boolean;
	/** Set when the stream is 360 footage, detected at probe time. */
	projection?: Projection | null;
	/**
	 * Degrees counter-clockwise the file's coded frame is turned for display (a
	 * portrait phone clip is 90 or -90). `width`/`height` are already the
	 * displayed size; absent when 0.
	 */
	rotation?: number;
	/** Transfer characteristic as ffprobe names it: `arib-std-b67` (HLG), `smpte2084` (PQ), `bt709`, … */
	color_transfer?: string | null;
	/** Colour primaries as ffprobe names them: `bt2020`, `bt709`, … */
	color_primaries?: string | null;
	/** ffprobe's pixel format (`yuv420p`, `yuva420p`, ...); absent on an asset probed before it was recorded. */
	pix_fmt?: string | null;
	/** The YCbCr matrix the stream declares, as ffprobe names it (`bt709`, `smpte170m`, `bt2020nc`, …); absent when it declares none. */
	color_space?: string | null;
	/** The sample range the stream declares: `pc` (full) or `tv` (limited); absent when it declares none and for an asset probed before it was recorded. */
	color_range?: string | null;
	/** Seconds the video starts after the file's sound (a late-starting source); absent when 0. */
	lead?: number;
}

/**
 * How a video stream maps the world onto its frame. `flat` is ordinary video;
 * the rest are 360 sources (a raw Insta360 `.insv` is `dual_fisheye`, a stitched
 * Studio export is `equirect`).
 */
export type Projection = 'equirect' | 'dual_fisheye' | 'fisheye' | 'flat';

export interface Asset {
	id: string;
	path: string;
	name: string;
	duration: number;
	streams: StreamInfo[];
	imported_at: string;
	/** Original capture files a derived asset was built from (an Insta360 lens pair). */
	source_paths?: string[];
	/** Set on a synthesized voiceover: the script and settings it was made from. */
	voiceover?: Voiceover;
	/** Set on composed music: the score it was rendered from, which is what "edit and compose
	 *  again" starts from. Absent for ordinary media. */
	music_score?: MusicScore;
	/** Set on a synthesized sound effect: the spec it was rendered from, which is what "reshape it" starts from
	 *  (`updateSoundEffect`). Absent for ordinary media. */
	sound_effect?: SfxSpec;
	/** The labels on the asset (`location: Cumbuco`, `jump`), for finding it and for building a cut by a key.
	 *  Set with `editAssetTags`, never by probing. Absent when it has none. */
	tags?: Tag[];
}

/** A label on a whole asset (`kerf_core::Tag`): `{ key: 'location', value: 'Cumbuco' }` is `location: Cumbuco`,
 *  `{ value: 'jump' }` the plain tag `jump`. Keys are lowercased and a tag is compared ignoring case; `tags.ts` has the
 *  spelling rules (`parseTag`, `formatTag`). */
export interface Tag {
	key?: string;
	value: string;
}

/** A change to an asset's tags (`kerf_core::TagEdit`), applied in the order `remove_keys`, `remove`, `add`: replacing a
 *  location is `remove_keys: ['location']` with `add: [{ key: 'location', value: 'Jeri' }]`. */
export interface TagEdit {
	/** Tags to put on. */
	add?: Tag[];
	/** Tags to take off (the same key and value, ignoring case; a plain tag only takes off a plain tag). */
	remove?: Tag[];
	/** Keys to take every tag of off. */
	remove_keys?: string[];
}

/** One condition on an asset's tags (`kerf_core::TagFilter`): `{ key, value }` is that tag, `{ key }` any tag with that
 *  key, `{ value }` alone a plain tag with that value. Several together must all hold. */
export interface TagFilter {
	key?: string;
	value?: string;
}

/** A value in the vocabulary and how many assets carry it (`kerf_core::TagCount`). */
export interface TagCount {
	value: string;
	assets: number;
}

/** A key in the vocabulary: how many assets have it at all, and its values (`kerf_core::TagKeyCount`). */
export interface TagKeyCount {
	key: string;
	assets: number;
	values: TagCount[];
}

/** Every tag in use, for autocomplete and for choosing a key to group by (`kerf_core::TagSummary`). */
export interface TagSummary {
	/** Keyed tags by key, A to Z. */
	keys: TagKeyCount[];
	/** Plain tags, A to Z. */
	plain: TagCount[];
}

/** How `assembleByTag` titles each group with its value. */
export type GroupTitle = 'overlay' | 'card' | 'none';

/** Which way the assets of one group are ordered: oldest import first, or by name. */
export type SortWithin = 'imported' | 'name';

/** A cut built from the library by the value of one tag key (`kerf_core::AssembleByTag`). Every member but `key` is
 *  optional and left at its default when omitted. */
export interface AssembleByTag {
	/** The key to group by (`location`): one group per value. */
	key: string;
	/** The order of the groups, by value (ignoring case). Values it does not list follow A to Z; the default is A to Z. */
	order?: string[];
	/** Only assets matching every term are used. */
	filter?: TagFilter[];
	/** Assets with no value for `key` come last, in a group of their own with no title (default: left out). */
	include_untagged?: boolean;
	/** How a group is titled with its value (default `overlay`: a lower third over the first seconds of its first clip;
	 *  `card` is `title_seconds` of the cut before the group). */
	title?: GroupTitle;
	/** How long a title (a card) runs, above 0 and at most 60 s (default 3). */
	title_seconds?: number;
	/** How a group's assets are ordered (default `imported`). */
	sort_within?: SortWithin;
}

/** One group of the cut `assembleByTag` laid out (`kerf_core::AssembledGroup`). */
export interface AssembledGroup {
	/** The value that names the group; `null` for the assets with no value for the key. */
	value: string | null;
	/** Its assets, in the order they play. */
	asset_ids: string[];
	/** The group's title clip, when it has one. */
	title_clip_id?: string;
	/** Where the group begins on the timeline, seconds (a card's start, else its first clip's). */
	start: number;
	/** Where it ends. */
	end: number;
}

/** What `assembleByTag` added (`kerf_core::AssembleSummary`). */
export interface AssembleSummary {
	groups: AssembledGroup[];
	/** The clips added from the library, in play order: one for each of the groups' `asset_ids` in turn. */
	clip_ids: string[];
	/** Values of `order` that no asset carries. Absent when there are none. */
	unmatched?: string[];
}

/** The answer of `assembleByTag`: the refreshed cut and what was laid out in it. */
export interface AssembleEdit extends AssembleSummary {
	timeline: Timeline;
}

/** What a generated voiceover was made from, kept on its asset so it can be
 *  regenerated. `segments` are the script's sentences in asset time. */
export interface Voiceover {
	text: string;
	voice: string;
	speed: number;
	segments: TranscriptSegment[];
}

export interface TimeRange {
	start: number;
	end: number;
}

export interface TranscriptSegment {
	start: number;
	end: number;
	text: string;
	/** The line's words with their own source times, when known. Absent in a transcript
	 *  made before words were recorded and in imported captions. */
	words?: Word[];
	/** Who speaks the line (`S1`, `S2`, …), when speakers were labelled. */
	speaker?: string | null;
}

/** One word of a transcript line, in the asset's source time. */
export interface Word {
	text: string;
	start: number;
	end: number;
	confidence?: number | null;
	/** Spread over the line by character share (the backend reports no word times), then
	 *  tightened to the speech energy. */
	estimated?: boolean;
	speaker?: string | null;
}

export interface Loudness {
	integrated_lufs: number;
	loudness_range: number;
	true_peak_dbtp: number;
	threshold_lufs: number;
}

export interface Tempo {
	bpm: number;
	beats: number[];
	confidence: number;
	/** Bar starts in source seconds; empty when no bar grid could be fitted (the beats are then
	 *  a plain estimate). With a grid, `beats` are its beats. */
	downbeats: number[];
}

export interface BeatGrid {
	period_s: number;
	phase_s: number;
	downbeat_offset: number;
	beats_per_bar: number;
}

export interface PhraseMatch {
	a: number;
	b: number;
	bars: number;
}

export interface MusicAnalysis {
	grid: BeatGrid;
	duration: number;
	bar_chroma: number[][];
	phrases: PhraseMatch[];
}

/** One piece of the source, placed at `output_start` seconds into the fitted music
 *  (`kerf_core::MusicSegment`). */
export interface MusicSegment {
	source_start: number;
	source_end: number;
	output_start: number;
}

/** A planned "fit music to length" (`kerf_core::MusicFit`): the segments in output order, how
 *  long they last and how far that is from the target. `remainder = target - duration`:
 *  positive is short of it, negative runs over and wants a fade-out. */
export interface MusicFit {
	segments: MusicSegment[];
	target: number;
	duration: number;
	remainder: number;
	/** Whole bars played between the intro and the ending. */
	bars: number;
	splices: number;
}

/** What fitting did (`kerf_core::MusicFitReport`): the clips it placed (in order), the plan they
 *  came from, whether an overrun was cut and faded, and how long the music now lasts. */
export interface MusicFitReport {
	clips: string[];
	fit: MusicFit;
	faded: boolean;
	duration: number;
}

export type AudioClass = 'speech' | 'music' | 'mixed' | 'unknown';

export interface AudioClassification {
	class: AudioClass;
	confidence: number;
}

export interface AssetAnalysis {
	asset_id: string;
	silence_segments: TimeRange[];
	scene_changes: number[];
	transcript: TranscriptSegment[];
	loudness: Loudness | null;
	onsets: number[];
	tempo: Tempo | null;
	audio_class: AudioClassification | null;
	/** The kinds whose step ran to completion — what tells "ran and found nothing" from
	 *  "never ran". Absent in an analysis cached before it was recorded (a kind then
	 *  counts as done when it has data). */
	ran?: AnalysisKind[];
	music: MusicAnalysis | null;
}

/** One kind of analysis: each is a step of its own, run alone and cached independently. */
export type AnalysisKind = 'silence' | 'scenes' | 'loudness' | 'rhythm' | 'transcript';

/** Where one kind of an asset's analysis stands. `off`: not run and will not by itself
 *  (switched off in Settings, or no speech backend). */
export type AnalysisState = 'done' | 'not_run' | 'running' | 'failed' | 'off';

export interface AnalysisKindStatus {
	kind: AnalysisKind;
	state: AnalysisState;
	/** Why it failed or is off. */
	reason?: string | null;
}

/** An asset's analysis kind by kind (`analysis_statuses`, the `analysis-status` event):
 *  always one entry per kind, in pass order. */
export interface AnalysisStatus {
	asset_id: string;
	kinds: AnalysisKindStatus[];
}

/** Which analyses run by themselves on import (Settings › Analysis). */
export interface AutoAnalysis {
	/** Analyze new media on import at all. */
	enabled: boolean;
	silence: boolean;
	scenes: boolean;
	loudness: boolean;
	rhythm: boolean;
	transcript: boolean;
}

/** Where an asset's preview proxy stands. */
export type ProxyPhase = 'not_needed' | 'off' | 'missing' | 'queued' | 'building' | 'ready' | 'failed';

/** An asset's preview proxy (`proxy_statuses`, the `proxy-progress` event). */
export interface ProxyStatus {
	asset_id: string;
	state: ProxyPhase;
	/** 0..1, while building. */
	fraction?: number | null;
	/** Why it is off, not needed or failed. */
	reason?: string | null;
	/** How wide the proxy is (or would be), pixels across. */
	width?: number | null;
	/** The proxy file's size on disk. */
	bytes?: number | null;
	elapsed_secs?: number | null;
	eta_secs?: number | null;
}

/** How wide preview proxies are, pixels across; 0 = none are made. 1280 is the default. */
export type ProxySize = 0 | 720 | 1080 | 1280;

/** Which file the preview decodes: the proxy once it is ready, always the original, or
 *  only the proxy (waiting for it). Export always reads the originals. */
export type PreviewSource = 'auto' | 'original' | 'proxy_only';

export interface Transform {
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
	crop_left: number;
	crop_right: number;
	crop_top: number;
	crop_bottom: number;
}

export interface Color {
	brightness: number;
	contrast: number;
	saturation: number;
	gamma: number;
	/** Warm/cool shift −1..1 (0 = neutral; positive warms, negative cools). */
	temperature: number;
}

/** A point of a tone curve: input to output, both 0..1. */
export interface CurvePoint {
	x: number;
	y: number;
}

/** Tone curves; an absent or empty list is the identity. A channel's curve runs first, the master on its result. */
export interface Curves {
	master?: CurvePoint[];
	red?: CurvePoint[];
	green?: CurvePoint[];
	blue?: CurvePoint[];
}

/** One colour wheel (`colorbalance` for its range): a puck in the unit disc and a lightness −1..1. */
export interface Wheel {
	x: number;
	y: number;
	lightness: number;
}

export interface Wheels {
	shadows: Wheel;
	midtones: Wheel;
	highlights: Wheel;
}

/** A `.cube` 3D LUT a clip is looked up through, and how much of it (0..1). */
export interface ClipLut {
	path: string;
	intensity: number;
}

/** Curves, wheels and a LUT, applied after `color` (LUT, curves, wheels). Each absent when unused. */
export interface Grade {
	curves?: Curves | null;
	wheels?: Wheels | null;
	lut?: ClipLut | null;
}

/** What a `.cube` file holds. */
export interface LutInfo {
	path: string;
	size: number;
	title?: string;
}

export type MaskShape = 'rect' | 'ellipse';

/** How a clip composites onto the tracks below. Mirrors `BlendMode` in crates/kerf-core/src/model.rs
 *  (`blend-modes.ts` lists them for a picker). */
export type BlendMode =
	| 'normal'
	| 'multiply'
	| 'screen'
	| 'overlay'
	| 'hard_light'
	| 'add'
	| 'darken'
	| 'lighten'
	| 'difference'
	| 'exclusion';

/** A shape cut out of a clip: inside it the clip is kept, outside it goes
 *  transparent so a lower track shows through. Everything is a fraction of the
 *  rendered frame. Mirrors `Mask` in crates/kerf-core/src/model.rs. */
export interface Mask {
	shape: MaskShape;
	/** Centre of the shape, 0..1 across and down the frame. */
	x: number;
	y: number;
	/** Full width / height of the shape as a fraction of the frame. */
	width: number;
	height: number;
	/** Edge softness as a fraction of the shape's half-size; 0 is a hard cut. */
	feather: number;
	/** Keep what is outside the shape instead of inside it. */
	inverted?: boolean;
}

/** Mirrors `Mask::default()` in crates/kerf-core/src/model.rs. */
export const DEFAULT_MASK: Mask = {
	shape: 'rect',
	x: 0.5,
	y: 0.5,
	width: 0.5,
	height: 0.5,
	feather: 0.15,
	inverted: false
};

export type TransitionKind =
	| 'crossfade'
	| 'dip_to_black'
	| 'dip_to_white'
	| 'slide_left'
	| 'slide_right'
	| 'slide_up'
	| 'slide_down'
	| 'push_left'
	| 'push_right'
	| 'push_up'
	| 'push_down';

export interface Transition {
	kind: TransitionKind;
	duration: number;
}

// Discriminated unions mirroring kerf_core::{VideoEffect, AudioEffect}
// (serde `#[serde(tag = "type")]`).
export type VideoEffect =
	| { type: 'blur'; sigma: number }
	| { type: 'sharpen'; amount: number }
	| { type: 'grayscale' }
	| { type: 'invert' }
	| { type: 'vignette' }
	| { type: 'chroma_key'; color: string; similarity: number; blend: number };

export type AudioEffect =
	| { type: 'highpass'; hz: number }
	| { type: 'lowpass'; hz: number }
	| { type: 'equalizer'; hz: number; width: number; gain_db: number }
	| {
			type: 'compressor';
			threshold_db: number;
			ratio: number;
			attack_ms: number;
			release_ms: number;
			makeup_db: number;
	  }
	| { type: 'gate'; threshold_db: number }
	| { type: 'denoise'; reduction_db: number; floor_db: number; track_noise: boolean };

/** How a keyframe travels to the next one (kerf-core's `Easing`); omitted is linear. */
export type Easing =
	| 'linear'
	| 'hold'
	| 'ease_in'
	| 'ease_out'
	| 'ease_in_out'
	| { bezier: { x1: number; y1: number; x2: number; y2: number } };

/** One keyframe of a clip's animated transform. */
export interface Keyframe {
	time: number;
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
	/** The shape of the segment leaving this key; absent is linear. */
	easing?: Easing;
}

/** One animatable number of a clip (kerf-core's `Property`): a transform number, a colour
 *  number, the clip's own gain (linear), a number of its mask or the animated crop (a zoom into
 *  the picture and where its window sits). */
export type Property =
	| 'scale'
	| 'pos_x'
	| 'pos_y'
	| 'rotation'
	| 'opacity'
	| 'brightness'
	| 'contrast'
	| 'saturation'
	| 'gamma'
	| 'temperature'
	| 'volume'
	| 'mask_x'
	| 'mask_y'
	| 'mask_width'
	| 'mask_height'
	| 'mask_feather'
	| 'crop_zoom'
	| 'crop_x'
	| 'crop_y';

/** One key of a property's own animation. `time` is seconds from the clip's start. */
export interface PropertyKey {
	time: number;
	value: number;
	/** The shape of the segment leaving this key; absent is linear. */
	easing?: Easing;
}

/** The keys of one property. A property with a track is driven by it (even an empty one:
 *  static, whatever `keyframes` says); a transform number without one reads `keyframes`. */
export interface PropertyTrack {
	prop: Property;
	keys: PropertyKey[];
}

/** One keyframe of a 360 clip's animated virtual camera. */
export interface ReframeKeyframe {
	time: number;
	yaw: number;
	pitch: number;
	roll: number;
	fov: number;
}

/**
 * Per-clip reprojection of 360 footage: aim a virtual camera into the sphere and
 * render what it sees. `output` is `flat` for a normal deliverable, or `equirect`
 * to stitch a dual-fisheye source without choosing a direction.
 */
export interface Reframe {
	input: Projection;
	output: Projection;
	/** Field of view of each physical lens (dual-fisheye sources only). */
	lens_fov: number;
	yaw: number;
	pitch: number;
	roll: number;
	/** Diagonal field of view of the virtual camera, in degrees. */
	fov: number;
	keyframes?: ReframeKeyframe[];
}

/** One keyframe of an animated text overlay (position + opacity). */
export interface TextKeyframe {
	time: number;
	pos_x: number;
	pos_y: number;
	opacity: number;
}

/** A timed text element (title / lower-third / caption) drawn over the cut. */
export interface TextOverlay {
	id: string;
	text: string;
	start: number;
	end: number;
	pos_x: number;
	pos_y: number;
	size: number;
	color: string;
	bg?: string | null;
	font?: string | null;
	bold: boolean;
	keyframes?: TextKeyframe[];
	/** Written by `generate_captions` or `import_captions` rather than by hand.
	 *  Regenerating or re-importing replaces these and leaves typed titles alone. */
	generated?: boolean;
}

/** The ready-made animated titles (`kerf_core::SvgPresetKind`). */
export type SvgPresetKind = 'fade_up' | 'typewriter' | 'lower_third' | 'word_pop';

/** What a preset is filled with. */
export interface SvgPresetParams {
	text: string;
	subtitle?: string | null;
	/** Any SVG colour. */
	color: string;
	/** The second colour a preset draws in (bar, last word, subtitle). */
	accent: string;
	/** A font family; unset / `null` is the font Kerf ships. */
	font?: string | null;
}

/** Params as a caller may write them: the backend fills what is left out. */
export type SvgPresetParamsInput = Pick<SvgPresetParams, 'text'> & Partial<Omit<SvgPresetParams, 'text'>>;

/** How an animated title was made when it came from a preset. */
export interface SvgPreset {
	kind: SvgPresetKind;
	params: SvgPresetParams;
}

/** An animated SVG title: SMIL-animated artwork drawn over the cut, above the clips and below the
 *  text overlays. `svg` is the **sanitised** source (stored = rendered); the page never inlines it,
 *  only shows a placement box and `<img>` data-URL thumbnails. The artwork is fit "contain",
 *  centred, into the rectangle `x, y, width, height` (fractions of the frame, top-left origin). */
export interface SvgOverlay {
	id: string;
	name: string;
	svg: string;
	start: number;
	end: number;
	x: number;
	y: number;
	width: number;
	height: number;
	opacity: number;
	/** Seconds of the document's own clock already elapsed at `start` (a slice cut into it).
	 *  The backend omits it at 0. */
	offset?: number;
	/** Absent on custom artwork. */
	preset?: SvgPreset;
	/** Present on a title made from a motion template: what it remembers of it. The artwork is still this
	 *  title's own `svg`, so a value change is `set_template_values`, never a patch of `svg` (that ends the template). */
	template?: TemplateInstance;
}

/** Where an animated title's artwork comes from; externally tagged, as the backend reads it. */
export type SvgSource = { custom: string } | { preset: { kind: SvgPresetKind; params: SvgPresetParamsInput } };

/** Name and placement of a new animated title; a field left out takes the default (full frame, opaque). */
export interface SvgLayout {
	name?: string;
	x?: number;
	y?: number;
	width?: number;
	height?: number;
	opacity?: number;
}

/** Fields of an animated title to change; one left out stays. `svg` replaces the artwork with
 *  custom source (and so drops the preset); `preset_params` re-fills a preset title and `preset_kind`
 *  switches it to another kind, keeping its id and place in the draw order (a custom title refuses
 *  both); `svg` is exclusive with either. */
export interface SvgOverlayPatch {
	name?: string;
	svg?: string;
	preset_params?: SvgPresetParamsInput;
	preset_kind?: SvgPresetKind;
	start?: number;
	end?: number;
	x?: number;
	y?: number;
	width?: number;
	height?: number;
	opacity?: number;
}

/** One parameter a preset takes. */
export interface SvgPresetParamInfo {
	name: string;
	required: boolean;
	default?: string;
}

/** A preset and what it takes (`list_svg_presets`). */
export interface SvgPresetInfo {
	kind: SvgPresetKind;
	label: string;
	params: SvgPresetParamInfo[];
}

/** What `validate_svg` made of a source: the sanitised document, what could not be honoured, and how
 *  long its animations run (`null` when they never end or there are none). */
export interface SvgCheck {
	svg: string;
	warnings: string[];
	natural_duration: number | null;
}

/** What a template control changes (`kerf_core::TemplateKind`): an element's text, a colour attribute, a
 *  plain number in an attribute, a `font-family`, or an `<image>`'s `href` (a `data:` URL). */
export type TemplateKind = 'text' | 'color' | 'number' | 'font' | 'image';

/** Where a control's value goes: the element with this `id`, and which attribute (none for `text`). */
export interface TemplateTarget {
	element: string;
	/** As the SVG spells it (`fill`, `stop-color`, `xlink:href`); required for every kind but `text`. */
	attribute?: string;
}

/** One thing about a template that can be changed without opening the artwork. Values are strings:
 *  text as it reads; a colour as `#rgb[a]` / `#rrggbb[aa]` / an SVG colour name / `none` / `currentColor`
 *  (no `rgb()`); a number unitless in plain decimal; a font as one family name; an image as a `data:` URL
 *  (`template_image_value` makes one from a file). */
export interface TemplateControl {
	/** Unique in the template; the key of a value. `[A-Za-z][A-Za-z0-9_-]{0,63}`. */
	id: string;
	label: string;
	kind: TemplateKind;
	/** Every place the one value is written (1 to 16). */
	targets: TemplateTarget[];
	/** Bounds of a `number` control; absent otherwise. */
	min?: number;
	max?: number;
}

/** What a title on the timeline remembers of the template it came from (`SvgOverlay.template`). */
export interface TemplateInstance {
	/** The library id it came from; the library is never consulted again. */
	template_id: string;
	name: string;
	controls: TemplateControl[];
	/** Each control's current value, read back from the title's `svg` after every change; a control whose
	 *  element is gone is absent, and **`image` controls are never here** (their `data:` URL is in the `svg`). */
	values: Record<string, string>;
}

/** A template in the library, as the grid shows it. Its poster is fetched on its own (`templatePoster`). */
export interface TemplateInfo {
	/** A lower-case UUID. */
	id: string;
	name: string;
	description: string;
	/** Seconds it runs: the length an applied one gets. */
	duration: number;
	/** The artwork's own shape in pixels. */
	width: number;
	height: number;
	controls: TemplateControl[];
	/** The families of the fonts it carries. */
	fonts: string[];
	has_poster: boolean;
	size_bytes: number;
}

/** A file in the library that could not be read (it is listed, not a failure of the list). */
export interface TemplateProblem {
	file: string;
	error: string;
}

/** The library: the templates that read, by name, and the files that did not. */
export interface TemplateList {
	templates: TemplateInfo[];
	problems: TemplateProblem[];
}

/** A template saved into the library, and what could not be carried over (a font whose licence or size
 *  keeps it out of the file, ...). */
export interface TemplateSaved {
	info: TemplateInfo;
	warnings: string[];
}

/** What importing a `.kerftemplate` did: it never overwrites. `already_present`: the library had this very
 *  template, nothing was written. `added_as_copy`: another template has the id, so this was kept under a new one. */
export type ImportStatus = 'added' | 'already_present' | 'added_as_copy';

export interface ImportOutcome {
	info: TemplateInfo;
	status: ImportStatus;
	warnings: string[];
}

/** What applying a template, adding or changing a title clip or setting a template title's values hands back: the
 *  refreshed cut, the title that was made or changed (the one to select: `overlay_id` for a floating one, `clip_id` for
 *  one on a track), and what the sanitiser or the machine could not honour (e.g. a font that is not installed). */
export interface TemplateEdit {
	timeline: Timeline;
	/** The floating title made or changed; absent for a title on a track. */
	overlay_id?: string;
	/** The title clip made or changed, for a title on a track. */
	clip_id?: string;
	warnings: string[];
}

/** How a text fits its box (`TextFit`): its lines wrap at the width, or it stays on one line made smaller to fit. */
export type TextFit = 'wrap' | 'shrink';

/** A text's box (`TextBox`): a width in the title's own units from the text's anchor; `null` is no box. Drawn only when
 *  the title's text is glyph outlines. */
export interface TextBox {
	width: number | null;
	fit: TextFit;
}

/** What a text animation's selectors count (`AnimateBy`). */
export type AnimateBy = 'character' | 'word' | 'line';

/** The pace of the window's slide (`Easing`). */
export type AnimationEasing = 'linear' | 'ease_in' | 'ease_out' | 'ease_in_out';

/** The amount inside the window (`Shape`). */
export type AnimationShape = 'square' | 'ramp_up' | 'ramp_down' | 'triangle' | 'round' | 'smooth';

/** A window of the text and how it slides (`RangeSelector`): fractions of the text, the offset from the animation's
 *  start to its end. */
export interface AnimationRange {
	start: number;
	end: number;
	offset: { from: number; to: number };
	shape: AnimationShape;
}

/** Seeded noise that multiplies the range's amount (`Wiggle`). */
export interface AnimationWiggle {
	rate: number;
	min: number;
	max: number;
	correlation: number;
	seed: number;
}

/** What a unit does at an amount of 1 (`Properties`): x / y / blur in em, rotation in degrees. */
export interface AnimationProperties {
	x: number;
	y: number;
	scale: number;
	rotation: number;
	opacity: number;
	blur: number;
}

/** The path a property takes to its resting value (`Curve`): After Effects' speed and influence (the speeds in multiples of
 *  the average speed, -5 to 5; the influences the share of the way each holds for, 0.01 to 1), or a damped spring
 *  (`bounce` 0.05 to 0.8). Written for a unit settling into its place; a value may pass its resting one and come back. */
export type AnimationCurve =
	| { kind: 'speed'; out_speed: number; out_influence: number; in_speed: number; in_influence: number }
	| { kind: 'spring'; bounce: number };

/** A curve for each property of an animation (`Curves`); one left out, or `null`, is a straight line. */
export interface AnimationCurves {
	x?: AnimationCurve | null;
	y?: AnimationCurve | null;
	scale?: AnimationCurve | null;
	rotation?: AnimationCurve | null;
	opacity?: AnimationCurve | null;
	blur?: AnimationCurve | null;
}

/** A named curve (`CurvePreset`): `curve` is `null` for the straight line. */
export interface AnimationCurvePreset {
	id: string;
	name: string;
	description: string;
	curve: AnimationCurve | null;
}

/** A text's animation (`TextAnimation`), written on its `<text>` as `data-kerf-animate` and compiled to per-glyph SMIL
 *  when the text is glyph outlines. `curves` is absent when no property has one. */
export interface TextAnimation {
	by: AnimateBy;
	delay: number;
	duration: number;
	easing: AnimationEasing;
	range: AnimationRange;
	wiggle: AnimationWiggle | null;
	properties: AnimationProperties;
	curves?: AnimationCurves;
}

/** A ready-made animation (`TextAnimationPreset`): choosing one copies `animation` onto the text. */
export interface TextAnimationPreset {
	id: string;
	name: string;
	description: string;
	animation: TextAnimation;
}

/** How a title's text is drawn (`TitleTextForm`): as glyph outlines, or with its fonts. */
export type TitleTextForm = 'outlines' | 'text';

/** An element of a title that has an `id` and its tag name: what a control can aim at (`svg_element_ids`). */
export interface SvgElementId {
	id: string;
	tag: string;
}

/** The look a generated caption set takes: a held subtitle line, or one large
 *  word at a time — the style social captions have converged on. */
export type CaptionStyle = 'lines' | 'word_punch';

/** How a transcript is turned into on-screen captions. Everything but the style
 *  is an override: omit a field and it follows the style, so asking for
 *  `word_punch` alone gets the whole look. */
export interface CaptionOptions {
	style?: CaptionStyle;
	max_words?: number;
	max_chars?: number;
	pos_y?: number;
	size?: number;
}

/** A subtitle format Kerf reads: SubRip (`.srt`), WebVTT (`.vtt`) or ASS / SSA (`.ass`, `.ssa`). */
export type CaptionFormat = 'srt' | 'ass' | 'vtt';

/** Which clock an imported subtitle file's times are on. `timeline`: a subtitle
 *  track made for the finished cut (the default). `source`: the file times one
 *  asset's own footage — a transcript, or subtitles for the uncut recording — and
 *  each cue is projected through that asset's clips like a transcript. */
export type CaptionTimeBase = 'timeline' | 'source';

/** What an import did. Every usable cue lands in exactly one of `placed`,
 *  `dropped_outside`, `dropped_short` or `dropped_overlap`, so
 *  `cues === placed + dropped_outside + dropped_short + dropped_overlap`;
 *  `captions` can exceed `placed` because a long cue is split into several lines. */
export interface CaptionImportSummary {
	format: CaptionFormat;
	/** Usable cues read from the file. */
	cues: number;
	/** Cues that put at least one caption on screen. */
	placed: number;
	/** Caption overlays written. */
	captions: number;
	/** Entries of the file that could not be used. */
	skipped_lines: number;
	/** Cues past the end of the cut (or before its start), or timing footage no
	 *  clip shows. */
	dropped_outside: number;
	/** Cues that do reach the cut but only for a moment too short to read. */
	dropped_short: number;
	/** Cues that lost their slot: captions are one lane, never two at once. */
	dropped_overlap: number;
	/** Earlier generated / imported captions this import replaced. */
	replaced: number;
}

/** The refreshed cut and what the import did. */
export interface CaptionImportResult {
	timeline: Timeline;
	summary: CaptionImportSummary;
}

/** How a subtitle file is laid onto the cut. */
export interface CaptionImportRequest {
	/** Defaults to `timeline`; `source` needs `assetId`, and `assetId` alone implies it. */
	base?: CaptionTimeBase;
	assetId?: string;
	/** The caption look, as `generate_captions` takes it. */
	options?: CaptionOptions;
	/** Seconds added to every cue before it is placed; negative moves them
	 *  earlier. For a file whose clock does not start where the picture does — a
	 *  broadcast SRT that begins at 01:00:00 wants `-3600`. */
	offset?: number;
}

/** One key of a clip's speed ramp (`SpeedKey`): the speed magnitude from `time` (clip-local timeline seconds), and how
 *  it moves from here to the next key (`linear` when absent, the backend omits it). */
export interface SpeedKey {
	time: number;
	speed: number;
	easing?: 'linear' | 'hold';
}

export interface Clip {
	id: string;
	asset_id: string;
	source_in: number;
	source_out: number;
	timeline_start: number;
	volume: number;
	fade_in: number;
	fade_out: number;
	// New per-clip primitives. Optional so browser-sample / older clip literals
	// still type-check; the backend always serializes them.
	speed?: number;
	/** The clip's playback rate over its length (`speed_ramp.rs`): keys in clip-local timeline seconds. While it
	 *  holds a well-formed ramp the keys decide the rate and `speed` only gives the direction (its sign). The backend
	 *  omits it when empty. */
	speed_ramp?: SpeedKey[];
	transform?: Transform;
	color?: Color;
	/** Curves, colour wheels and a LUT; absent when unused (the backend omits it). */
	grade?: Grade;
	transition_in?: Transition | null;
	/** A shape cut out of this clip; absent is the whole frame. */
	mask?: Mask | null;
	/** How the clip composites onto the tracks below; absent is `normal` (the backend omits it). */
	blend_mode?: BlendMode;
	effects?: VideoEffect[];
	audio?: AudioEffect[];
	keyframes?: Keyframe[];
	/** Per-property animation (the backend omits it when empty): any one number of the
	 *  transform, the colour or the clip's volume with keys of its own. */
	channels?: PropertyTrack[];
	/** 360 reprojection; absent for ordinary flat footage. */
	reframe?: Reframe | null;
	/** Crops kept for delivery shapes other than the project's own, written by
	 *  the multi-format export's framing pass. The project frame's crop is the
	 *  transform's. */
	framings?: Framing[];
	/** Whether the clip renders. Absent means enabled (the backend omits it when true). */
	enabled?: boolean;
	/** The link group: clips sharing a `link_id` (a picture and its detached sound) are
	 *  edited together — moved, trimmed, split, removed. Absent is unlinked (the backend
	 *  omits it). A group of one is not a link; `linkPartners` is the question to ask. */
	link_id?: string | null;
	/** Whether the clip plays the audio of its own asset. `false` once its sound was
	 *  detached onto an audio track (the picture is then silent). Absent means it does
	 *  (the backend omits it when true). */
	source_audio?: boolean;
	/** What the clip draws when its picture is generated rather than cut from an asset: a title on a track
	 *  (`asset_id` is then `GENERATED_ASSET` and the source is the title's own clock). Absent for footage. */
	generator?: Generator | null;
	/** Video stabilization: the camera's shake taken out of the picture once its analysis lands
	 *  (`clipStabilizationStatus`). Absent when the clip is not stabilized (the backend omits it). */
	stabilize?: Stabilize | null;
	/** Live behaviours (`Behavior`): procedural motion on top of the pose and its keys, in list order. Absent when
	 *  the clip has none (the backend omits it); at most `MAX_BEHAVIORS`. */
	behaviors?: Behavior[];
}

/** How strongly a clip is stabilized (`Stabilize`). */
export interface Stabilize {
	/** How much of the camera's movement is smoothed away, 0..1 (default 0.5). */
	strength: number;
	/** The picture enlarged by this many percent (0..30) to hide the moved edges; absent is automatic. */
	zoom?: number | null;
}

/** The most a stabilized picture is enlarged, in percent (`MAX_STABILIZE_ZOOM`). */
export const MAX_STABILIZE_ZOOM = 30;

/** Where a clip's stabilization stands (`StabilizeStatus`, tagged by `state`). */
export type StabilizeStatus =
	| { state: 'off' }
	| { state: 'unsupported'; reason: string }
	| { state: 'basic' }
	| { state: 'missing' }
	| { state: 'queued' }
	| { state: 'analyzing'; fraction: number }
	| { state: 'failed'; reason: string }
	| { state: 'ready' };

/** One clip's stabilization status, as `clip_stabilization_status` and `stabilize-progress` carry it. */
export type ClipStabilizeStatus = { clip_id: string } & StabilizeStatus;

/** The `asset_id` of every generated clip: the nil UUID, which no asset has. */
export const GENERATED_ASSET = '00000000-0000-0000-0000-000000000000';

/** An animated SVG title on a video track (`Clip.generator`): drawn over the whole frame, then placed, faded and keyed
 *  as its clip is. `svg` is the sanitised source; `template` as on a floating title. */
export interface TitleClip {
	name: string;
	svg: string;
	template?: TemplateInstance | null;
}

/** What a generated clip draws, tagged by `kind`. */
export type Generator = { kind: 'title' } & TitleClip;

/** A title clip's content to change (`update_title_clip`); its timing and look are the clip's own edits. */
export interface TitleClipPatch {
	name?: string;
	/** New custom artwork; it ends a template. */
	svg?: string;
}

/** A crop for one delivery shape (`aspect_w:aspect_h` in lowest terms). */
export interface Framing {
	aspect_w: number;
	aspect_h: number;
	crop_left: number;
	crop_right: number;
	crop_top: number;
	crop_bottom: number;
}

export const DEFAULT_TRANSFORM: Transform = {
	scale: 1,
	pos_x: 0,
	pos_y: 0,
	rotation: 0,
	opacity: 1,
	crop_left: 0,
	crop_right: 0,
	crop_top: 0,
	crop_bottom: 0
};

export const DEFAULT_COLOR: Color = { brightness: 0, contrast: 1, saturation: 1, gamma: 1, temperature: 0 };

/** A level, forward-facing 100° view — what a 360 clip gets on arrival. */
export const DEFAULT_REFRAME: Reframe = {
	input: 'equirect',
	output: 'flat',
	lens_fov: 190,
	yaw: 0,
	pitch: 0,
	roll: 0,
	fov: 100
};

/** One clip's destination in a group move (`move_clips`): where it starts
 *  afterwards (absolute seconds) and, optionally, another track of the same
 *  kind. Omitted, the clip stays on its track. */
export interface ClipMove {
	clip_id: string;
	timeline_start: number;
	track_id?: string;
}

/** One clip's cut in a group `split_remove_clips`: the clip and the timeline time it
 *  is cut at (inside it). */
export interface ClipCut {
	clip_id: string;
	at: number;
}

/** Which half of a split `split_remove` throws away: everything before the cut
 *  (`left` — trim the start to it) or everything after it (`right`). */
export type SplitSide = 'left' | 'right';

/** What a roll, slip or slide did (`roll_edit` / `slip_clip` / `slide_clip` over MCP;
 *  the Tauri commands answer with the refreshed timeline): how far it was asked to
 *  go, how far it went (it clamps to the footage and the neighbours), and the clips
 *  it changed as they stand afterwards, in timeline order. */
export interface EditOutcome {
	requested: number;
	applied: number;
	/** `applied` is not what was asked for. */
	clamped: boolean;
	clips: Clip[];
}

export interface Track {
	id: string;
	kind: StreamKind;
	name: string;
	clips: Clip[];
	/** Ducked under the rest of the mix on export (sidechain compression). */
	duck?: boolean;
	/** Silenced (audio) or hidden (video) — the track's clips do not render. */
	muted?: boolean;
	/** Soloed. While any track of a kind is soloed, the rest of that kind are shadowed. */
	solo?: boolean;
	/** Guarded against editing. A locked track still renders. */
	locked?: boolean;
	/** Track fader: a linear gain over every clip on the track. 1 is unity. */
	volume?: number;
	/** Stereo placement, -1 hard left to 1 hard right. 0 is centre. */
	pan?: number;
}

/** A named point on the timeline. Renders nothing — it is shared vocabulary
 *  for places in the cut, for the user and the agent alike. */
export interface Marker {
	id: string;
	time: number;
	name: string;
	color?: string | null;
	/** How long the marked span runs from `time`, seconds. Absent / 0 is a point. */
	duration?: number;
	/** A free-form note. Absent is none. */
	comment?: string;
	/** Absent is `comment`. A `chapter` starts a chapter of the exported file. */
	kind?: MarkerKind;
}

/** What a marker is for — mirrors `kerf_core::MarkerKind`. */
export type MarkerKind = 'comment' | 'chapter';

/** A change to a marker; each absent field is left alone. */
export interface MarkerPatch {
	time?: number;
	name?: string;
	color?: string;
	duration?: number;
	comment?: string;
	kind?: MarkerKind;
}

/** A subtitle format Kerf writes (`kerf_core::SubtitleFormat`). */
export type SubtitleFormat = 'srt' | 'vtt';

/** An interchange format (`kerf_core::InterchangeFormat`). */
export type InterchangeFormat = 'otio' | 'edl';

/** A marker list a delivery carries (`kerf_core::MarkerListFormat`). */
export type MarkerListFormat = 'csv' | 'youtube';

/** One line of an export `Report`: what was dropped or approximated, and how often. */
export interface ReportEntry {
	level: 'info' | 'warning';
	message: string;
	count: number;
}

export interface Report {
	entries: ReportEntry[];
}

/** A side-file written, and what its format could not carry (`kerf_core::ExportedFile`). */
export interface ExportedFile {
	path: string;
	report: Report;
}

/** The frame the project is cut *for* — the shape of the delivered video.
 *  Every rendered picture (preview, scrubbed still, export) uses it, so a
 *  vertical crop is visible while cutting rather than only in the output. */
export interface Delivery {
	width: number;
	height: number;
	fit: Fit;
	/** The frame rate the cut is delivered at (1 to 240): what an export, the playback and the stills
	 *  default to when the export names none. Absent follows the footage — the first clip with a picture.
	 *  An NTSC rate is the exact `n * 1000 / 1001` (`checkDeliveryFps`). */
	fps?: number;
}

/**
 * The last stage of the mix — mirrors `kerf_core::MasterBus`. A fader and a
 * safety limiter on the finished mix, after every track and the duck bus and
 * before `loudnorm`. Absent (the default, and every project that never touched
 * it) means unity gain, no limiter.
 */
export interface MasterBus {
	/** Linear gain on the finished mix; 1 is unity, clamped to 0..4 (+12 dB). */
	volume: number;
	/** A lookahead limiter holding the mix under `ceiling_db`. */
	limiter: boolean;
	/** Limiter ceiling in dBFS, -24..0 (-1.5 by default). A sample-peak ceiling. */
	ceiling_db: number;
	/** How tracks flagged `duck` dip. Absent / `null` is the sidechain compressor, which
	 *  dips by how loud the rest of the mix is; a number is the **speech gate**: the ducked
	 *  bus drops by exactly that many dB (negative, clamped to -40..-1) while the rest speaks.
	 *  The backend omits it when unset. */
	duck_depth_db?: number | null;
	/** The limiter holds the true peak (runs 4x oversampled). Absent / false is the sample-peak
	 *  limiter; the backend omits it when off. */
	true_peak?: boolean;
}

export interface Timeline {
	tracks: Track[];
	overlays?: TextOverlay[];
	/** Animated SVG titles, in draw order. The backend omits the list when empty. */
	svg_overlays?: SvgOverlay[];
	markers?: Marker[];
	/** Unset = the shape follows the footage (the historical behaviour). */
	format?: Delivery | null;
	/** The master bus. Unset = unity gain, no limiter. */
	master?: MasterBus;
	/** Thumbnails designed beside the cut, never drawn in it. The backend omits the list when empty. */
	thumbnails?: Thumbnail[];
}

/** Which of the video's own graphics a thumbnail's background keeps — mirrors `kerf_core::VideoGraphics`. */
export type VideoGraphics = 'none' | 'titles' | 'all';

/** One picture over a thumbnail's background — mirrors `kerf_core::ThumbnailLayer`. Its SVG is fitted
 *  "contain" into the rect `x, y, width, height` (fractions of the thumbnail). */
export interface ThumbnailLayer {
	id: string;
	name: string;
	svg: string;
	/** Set for a headline: what its artwork is made from. */
	text?: SvgPresetParams | null;
	/** Set for a layer made from a motion template. */
	template?: TemplateInstance | null;
	/** The moment of the artwork drawn (seconds); unset = its settled state. */
	at?: number | null;
	x: number;
	y: number;
	width: number;
	height: number;
	opacity: number;
}

/** A thumbnail — mirrors `kerf_core::Thumbnail`: a frame of the cut at `time`, cropped to `width` x `height`
 *  around the focus point, under its own layers. */
export interface Thumbnail {
	id: string;
	name: string;
	time: number;
	width: number;
	height: number;
	focus_x: number;
	focus_y: number;
	graphics?: VideoGraphics;
	layers?: ThumbnailLayer[];
}

/** One layer of a `ThumbnailSpec` — mirrors `kerf_core::ThumbnailLayerSpec`: an existing layer (`layer_id`), or a
 *  new one from exactly one of `text`, `template_id`, `svg`, `image` (a `data:` URL). */
export interface ThumbnailLayerSpec {
	layer_id?: string | null;
	text?: SvgPresetParams | null;
	template_id?: string | null;
	values?: Record<string, string> | null;
	svg?: string | null;
	image?: string | null;
	name?: string | null;
	/** Negative goes back to the settled moment. */
	at?: number | null;
	x?: number | null;
	y?: number | null;
	width?: number | null;
	height?: number | null;
	opacity?: number | null;
}

/** What a thumbnail should become — mirrors `kerf_core::ThumbnailSpec`; each unset field keeps what it has. */
export interface ThumbnailSpec {
	name?: string | null;
	time?: number | null;
	/** `youtube`, `vertical`, `square`, `4:5`, `16:9` or `WxH`. */
	size?: string | null;
	focus_x?: number | null;
	focus_y?: number | null;
	graphics?: VideoGraphics | null;
	/** The whole new stack, bottom first. */
	layers?: ThumbnailLayerSpec[] | null;
}

/** What `set_thumbnail` answers. */
export interface ThumbnailEdit {
	timeline: Timeline;
	thumbnail_id: string;
	warnings: string[];
}

/** A frame `suggest_thumbnail_frames` proposes — mirrors `kerf_core::thumbnail::SuggestedFrame`. */
export interface SuggestedFrame {
	time: number;
	/** 0..1: half sharpness, a quarter exposure, a quarter contrast. */
	score: number;
	sharpness: number;
	exposure: number;
	contrast: number;
}

/**
 * What one `ebur128` meter read — mirrors `kerf_core::LevelReading`. `null`
 * wherever there was nothing to report (silence, or a span under 3 s for the
 * short-term maximum).
 */
export interface LevelReading {
	/** Integrated (programme) loudness, LUFS. */
	integrated_lufs: number | null;
	/** Loudness range, LU. */
	loudness_range_lu: number | null;
	/** The loudest 3 s window, LUFS. */
	short_term_max_lufs: number | null;
	/** Highest sample, dBFS. */
	peak_dbfs: number | null;
	/** Highest true (inter-sample) peak, dBTP — what a re-encode can clip on. */
	true_peak_dbtp: number | null;
}

/** One track's strip: after its fader and pan, before the duck bus and the master. */
export interface TrackLevels {
	track_id: string;
	name: string;
	kind: StreamKind;
	ducked: boolean;
	/** Reaches the render at all — false for a muted or solo-shadowed track. */
	heard: boolean;
	level: LevelReading | null;
}

/** How loud the cut is (`get_levels`) — mirrors `kerf_core::Levels`. */
export interface Levels {
	/** Seconds of the cut measured. */
	duration: number;
	/** The finished mix; null when the cut has no audio. */
	master: LevelReading | null;
	tracks: TrackLevels[];
	/** The measurement ran through `loudnorm`. */
	loudnorm: boolean;
	/** The streaming target the notes judge against (-14 LUFS). */
	target_lufs: number;
	/** What the numbers mean for delivery, as advice. */
	notes: string[];
	/** Set **only** by the browser harness, whose numbers are a stand-in
	 *  estimated from the sample analysis — never by the backend. */
	estimated?: boolean;
}

export interface AssetMetadata {
	asset: Asset;
	analysis: AssetAnalysis | null;
}

/**
 * A window of an asset's audio as min/max peaks (`get_waveform_range`).
 * Mirrors `kerf_core::WaveformRange`. `min` / `max` are `[channel][bucket]`,
 * each `buckets` long and in -1..1; a bucket outside the media is 0 / 0.
 */
export interface WaveformRange {
	/** Lanes: 1 (mono) or 2 (stereo). */
	channels: number;
	/** Buckets per channel actually returned — the request, capped at 4096. */
	buckets: number;
	/** Length of the decoded audio in seconds. */
	duration: number;
	/** Resolution of the pyramid level served (10, 25, 100 or 500). */
	peaks_per_second: number;
	min: number[][];
	max: number[][];
}

/**
 * One JPEG of a filmstrip (`get_filmstrip`): `count` thumbnails side by side,
 * from the left. Mirrors `kerf_core::FilmstripSheet` plus the transport the app
 * adds. `width` is always `columns * frame_width`, so the last sheet of a strip
 * may be wider than `count * frame_width` — the tail is padding; trust `count`.
 */
export interface FilmstripSheet {
	/** Index of the first thumbnail on this sheet. */
	first_frame: number;
	/** Thumbnails on this sheet. */
	count: number;
	/** Pixel width of the image. */
	width: number;
	/** Pixel height of the image: the strip's `frame_height`. */
	height: number;
	/** `data:image/jpeg;base64,…` — a `data:` URL because the CSP admits no `blob:`.
	 *  (An SVG data URL in the browser harness, which has no decoder.) */
	data_url: string;
}

/**
 * An asset's thumbnails over time (`get_filmstrip`). Mirrors
 * `kerf_core::Filmstrip`; `filmstrip-geometry.ts` is the mirror of its lookups.
 * Thumbnail `k` is the frame on screen at source time `k * interval`.
 */
export interface Filmstrip {
	/** Seconds of source between two thumbnails. */
	interval: number;
	/** Width of one thumbnail in pixels (even). */
	frame_width: number;
	/** Height of one thumbnail in pixels (96). */
	frame_height: number;
	/** Thumbnails in the strip: at least 1, at most 300. */
	frames: number;
	/** Thumbnails per sheet (the last may hold fewer): thumbnail `k` is on sheet
	 *  `floor(k / columns)`, at `x = (k % columns) * frame_width`, `y = 0`. */
	columns: number;
	/** The sheets, in order; together they hold thumbnails `0..frames`. */
	sheets: FilmstripSheet[];
}

export type EditSource = 'user' | 'agent' | 'system';

export interface Revision {
	seq: number;
	label: string;
	source: EditSource;
	created_at: string;
	current: boolean;
}

export type DiffKind =
	| 'track_added'
	| 'track_removed'
	| 'track_changed'
	| 'clip_added'
	| 'clip_removed'
	| 'clip_moved'
	| 'clip_retrimmed'
	| 'clip_changed'
	| 'overlay_added'
	| 'overlay_removed'
	| 'overlay_changed'
	| 'marker_added'
	| 'marker_removed'
	| 'marker_changed'
	| 'format_changed'
	| 'master_changed'
	| 'thumbnails_changed';

export interface DiffEntry {
	/** The change's stable name, `<kind>:<the clip, text, marker or track it is about>`, which it keeps for
	 *  as long as the proposal holds it: what `applyStagedSubset` is sent. The backend always sets it; it is
	 *  optional here so a fixture need not, and `entryId` (`staged-subset.ts`) derives the same name. */
	id?: string;
	kind: DiffKind;
	summary: string;
	detail?: string | null;
	track_id?: string | null;
	clip_id?: string | null;
	/** The title or caption an `overlay_*` entry is about. */
	overlay_id?: string | null;
	/** The marker a `marker_*` entry is about. */
	marker_id?: string | null;
	at?: number | null;
}

export interface TimelineDiff {
	entries: DiffEntry[];
	duration_before: number;
	duration_after: number;
	clips_before: number;
	clips_after: number;
}

/** A batch of agent edits held back from the live timeline for review. */
export interface StagedEdit {
	base_seq: number;
	task_id?: string | null;
	note?: string | null;
	/** Some agent session owns the proposal. (The session id itself never leaves the backend.) */
	owned_by_agent?: boolean;
	edits: string[];
	/** How many of the latest staged steps `undoStagedEdit` can still take back (absent: none). */
	undo_depth?: number;
	/** Changes to the library held back with the proposal (an asset marked as 360, an asset's tags changed); they land with it. */
	asset_changes?: StagedAssetChange[];
	created_at: string;
	updated_at: string;
	/** The live timeline moved on since this branched — applying replaces it. */
	stale: boolean;
	diff: TimelineDiff;
}

/** A change to an *asset* an agent made while a proposal was open (`kerf_core::StagedAssetChange`): either a projection
 *  mark (`tag_edit` absent) or a tag change (`tag_edit` set). Show `summary`; do not read `projection` of a tag change, which
 *  is `null` without meaning "mark as flat". */
export interface StagedAssetChange {
	/** `asset_projection:<asset id>` or `asset_tags:<asset id>`, usable wherever a `DiffEntry.id` is. */
	id: string;
	asset_id: string;
	/** The projection the asset is to be marked with; `null` clears it. Not read by a tag change (it is `null`). */
	projection: Projection | null;
	/** A tag change: the agent's edits to the asset's tags composed into one (`TagEdit::then`). It is run over the tags
	 *  the asset has when it lands, so what the user tagged in between stays. Absent for a projection mark. */
	tag_edit?: TagEdit;
	/** One line for the review card, e.g. `Mark interview.mp4 as equirect 360` or `Tag clip.mp4: +location: Cumbuco, -jump`. */
	summary: string;
}

/** A change applied although the reviewer did not pick it, and why it had to be. */
export interface AutoIncluded {
	id: string;
	summary: string;
	because: string;
}

/** A picked change that no longer applies, and why. */
export interface SkippedChange {
	id: string;
	summary: string;
	why: string;
}

/** What `applyStagedSubset` did (`kerf_core::SubsetApplied`). */
export interface SubsetApplied {
	/** Changes applied, the ones that came along included. */
	applied: number;
	auto_included: AutoIncluded[];
	skipped: SkippedChange[];
	/** Changes still waiting for review in the proposal. */
	remaining: number;
	/** Nothing is left, so the proposal closed. */
	closed: boolean;
	/** One sentence for the toast. */
	message: string;
	/** The live timeline afterwards. */
	timeline: Timeline;
}

/** What `undoStagedEdit` took back: the step's label and the proposal as it now stands. */
export interface StagedUndone {
	label: string;
	staged: StagedEdit;
}

/** What `relinkAsset` did (`kerf_core::Relinked`). */
export interface Relinked {
	asset: Asset;
	/** The new file is the old media moved, so its analysis still holds; otherwise it was dropped. */
	analysis_kept: boolean;
	previous_path: string;
}

/** What `checkRelink` found, before anything is changed (`kerf_core::RelinkCheck`). */
export interface RelinkCheck {
	/** The file is the old media moved, so the analysis would be kept. */
	analysis_kept: boolean;
	/** The asset has analysis to lose: for one without, a different recording costs nothing. */
	has_analysis: boolean;
}

/** What removing an asset did (`kerf_core::RemovedAsset`). */
export interface RemovedAsset {
	asset: Asset;
	/** Clips of the cut removed with it (only with `force`). */
	clips_removed: number;
}

/** The `remove_asset` answer: what was removed and the refreshed cut. */
export interface AssetRemoval extends RemovedAsset {
	timeline: Timeline;
}

/** One transcript line as the cut plays it: the words of it one clip keeps (`kerf_core::CutLine`). */
export interface CutLine {
	/** Timeline seconds. */
	start: number;
	end: number;
	text: string;
	speaker?: string | null;
	clip_id: string;
	track_id: string;
	asset_id: string;
	/** The same span in the asset's own seconds. */
	source_start: number;
	source_end: number;
	words?: CutWord[];
}

/** One word of a `CutLine`, in timeline and source seconds (`kerf_core::CutWord`). */
export interface CutWord {
	text: string;
	start: number;
	end: number;
	source_start: number;
	source_end: number;
	estimated?: boolean;
}

/** An asset the cut plays the sound of with no transcript yet (`kerf_core::Untranscribed`). */
export interface Untranscribed {
	asset_id: string;
	name: string;
	clips: number;
}

/** What the cut says (`kerf_core::CutTranscript`). */
export interface CutTranscript {
	lines: CutLine[];
	untranscribed: Untranscribed[];
}

/** An asset whose source file is gone (`kerf_core::MissingAsset`). */
export interface MissingAsset {
	asset_id: string;
	name: string;
	path: string;
	/** Clips of the cut that use it. */
	clips: number;
}

/** How the words of a transcript search must appear: in order and adjacent (across lines too), all
 *  within one line, or any one of them. */
export type MatchMode = 'phrase' | 'all_words' | 'any_word';

export interface TranscriptQuery {
	query: string;
	/** Only these assets; absent searches the whole library. */
	asset_ids?: string[] | null;
	speaker?: string | null;
	mode?: MatchMode;
	offset?: number | null;
	/** Default 50, at most 500. */
	limit?: number | null;
}

/** A clip of the cut that uses (part of) a hit, in timeline seconds. */
export interface CutUse {
	clip_id: string;
	track_id: string;
	start: number;
	end: number;
	/** The clip holds only part of the hit. */
	partial?: boolean;
	disabled?: boolean;
}

export interface TranscriptHit {
	asset_id: string;
	asset_name: string;
	/** Source seconds, first matched word's start to the last one's end. */
	start: number;
	end: number;
	speaker?: string;
	text: string;
	/** The line(s) around the match, the match between « and ». */
	context: string;
	words_estimated?: boolean;
	/** Empty when the cut does not use it. */
	in_cut: CutUse[];
}

export interface TranscriptSearch {
	mode: MatchMode;
	total: number;
	offset: number;
	returned: number;
	next_offset?: number;
	searched_assets: number;
	/** Assets with sound (or named) not transcribed yet: how many, and the first ids. */
	assets_without_transcript: { count: number; asset_ids: string[] };
	hits: TranscriptHit[];
}

export type TaskStatus = 'queued' | 'working' | 'ready' | 'done' | 'failed';

export interface Task {
	id: string;
	prompt: string;
	status: TaskStatus;
	result?: string | null;
	created_at: string;
	updated_at: string;
}

// ---- export options (mirrors kerf_core::engine::ExportOptions) -------------

export type Container = 'mp4' | 'mov' | 'mkv' | 'webm' | 'gif' | 'mp3' | 'm4a' | 'wav' | 'flac';

export type RateControl = 'crf' | 'bitrate' | 'two_pass' | 'lossless';

export interface ExportOptions {
	container: Container;
	video_codec?: string | null;
	audio_codec?: string | null;
	rate_control: RateControl;
	crf?: number | null;
	video_bitrate?: string | null;
	max_rate?: string | null;
	buf_size?: string | null;
	preset?: string | null;
	prores_profile?: number | null;
	tune?: string | null;
	profile_v?: string | null;
	pix_fmt?: string | null;
	hwaccel?: string | null;
	resolution?: [number, number] | null;
	/** How footage of a different shape is fitted to `resolution`: letterboxed
	 *  (`contain`, the default), filled and cropped (`cover`), or letterboxed over a
	 *  blurred copy of itself (`blur`). Only matters when the delivery aspect differs
	 *  from the footage. */
	fit?: Fit;
	fps?: number | null;
	scaler?: string | null;
	audio_sample_rate?: number | null;
	audio_channels?: number | null;
	audio_bitrate?: string | null;
	flac_compression?: number | null;
	include_audio: boolean;
	faststart: boolean;
	gif_dither?: string | null;
	gif_loop: boolean;
	metadata_title?: string | null;
	/** Render only this timeline span (e.g. the in/out marks); omit for all. */
	range?: TimeRange | null;
	/** Normalize the final mix to -14 LUFS before encoding. The same as `loudness: 'youtube'`. */
	loudnorm?: boolean;
	/** Normalize the final mix to a platform's loudness target. Wins over `loudnorm`. */
	loudness?: LoudnessPreset | null;
	/** Reach the target with one measured static gain and a true-peak limiter instead of
	 *  `loudnorm` (the export measures the mix first; dynamics stay as mixed). */
	loudness_static?: boolean;
	/** Mux the cut's captions in as a soft subtitle stream (mp4 / mov / mkv / webm). */
	embed_captions?: boolean;
	/** Caption files written beside the export (`cut.srt`, `cut.vtt`), once it completes. */
	caption_sidecars?: SubtitleFormat[];
	/** Replace caption files already beside the export; otherwise the export is refused. */
	overwrite_sidecars?: boolean;
	/** Leave the generated captions out of the picture (typed titles are still drawn). */
	skip_burned_captions?: boolean;
}

/** What syncing a clip to another by their sound found (`kerf_core::AudioSync`). */
export interface AudioSync {
	reference_id: string;
	clip_id: string;
	/** Seconds the clip moves on the timeline; positive is later. */
	offset: number;
	/** Where the clip starts in sync. */
	timeline_start: number;
	/** Correlation peak over its RMS; 8 or more is a clear match. */
	confidence: number;
	/** Peak over the next-best match more than 50 ms away; 1.5 or more is unambiguous. `null` when
	 *  nothing else matched at all. */
	distinct: number | null;
	reliable: boolean;
	/** One recording has inverted polarity. */
	inverted: boolean;
	/** The clip was moved (an unreliable match is only reported unless forced). */
	applied: boolean;
}

/** `sync_clips_by_audio`'s answer: the sync and the timeline after it. */
export interface SyncedByAudio {
	sync: AudioSync;
	timeline: Timeline;
}

/** A delivery's loudness target (`kerf_core::LoudnessPreset`): YouTube and Spotify -14 LUFS,
 *  Apple -16, EBU R128 broadcast -23, all with a -1 dBTP true-peak ceiling. */
export type LoudnessPreset = 'youtube' | 'spotify' | 'apple' | 'broadcast';

/** How a clip's picture is fitted to an output frame of a different shape: `blur` is `contain`
 *  over a blurred, enlarged copy of the picture (a clip placed by its transform gets plain bars). */
export type Fit = 'contain' | 'cover' | 'blur';

/** A place a finished cut gets published, mirroring `kerf_core::platform`. */
export interface PlatformTarget {
	id: string;
	label: string;
	width: number;
	height: number;
	accepts: [number, number][];
	max_secs: number | null;
	reach_max_secs: number | null;
	min_secs: number | null;
	notes: string;
}

/** `error` = the platform rejects it, `warning` = accepted then under-distributed
 *  or letterboxed, `tip` = advice. */
export type Severity = 'error' | 'warning' | 'tip';

/** What an issue is about, so a UI can group four identical shape complaints
 *  into one line naming four platforms. */
export type IssueKind = 'empty' | 'length' | 'shape' | 'resolution' | 'captions';

export interface DeliveryIssue {
	severity: Severity;
	kind: IssueKind;
	message: string;
}

/** A cut's readiness for one publishing target. */
export interface DeliveryCheck {
	target: string;
	label: string;
	ok: boolean;
	issues: DeliveryIssue[];
}

/** What a readiness check read off the cut (`kerf_core::CutSummary`): the frame and length the
 *  file would have, which is not always the project's. */
export interface CutSummary {
	duration: number;
	width: number;
	height: number;
	has_audio: boolean;
	has_text: boolean;
}

/** `kerf_core::PlatformReport`: every target's check, with the cut they judged. */
export interface PlatformReport {
	cut: CutSummary;
	targets: DeliveryCheck[];
}

/** One check of the editing lint, and the `kind` of what it finds (`kerf_core::review::Check`). */
export type ReviewCheck =
	| 'flash_frame'
	| 'mid_word_cut'
	| 'hard_audio_cut'
	| 'jump_cut'
	| 'stale_captions'
	| 'black_gap'
	| 'unstabilized';

/** A defect in the cut (`kerf_core::ReviewIssue`), at timeline `time` (to `end` for a span), with the tools that fix it. */
export interface ReviewIssue {
	kind: ReviewCheck;
	severity: Severity;
	time: number;
	end?: number;
	clip_ids: string[];
	message: string;
	fix: string;
}

/** `kerf_core::CutReview`: what the editing lint found, which checks ran and why the others did not. */
export interface CutReview {
	issues: ReviewIssue[];
	counts_by_kind: Partial<Record<ReviewCheck, number>>;
	checked: ReviewCheck[];
	skipped: { check: ReviewCheck; reason: string }[];
}

/** The part of an export that is running (`kerf_core::ExportStage`). */
export type ExportStage = 'metering' | 'starting' | 'rendering' | 'finishing' | 'measuring';

/** Payload of the `export-progress` event streamed during a render. `fraction` is over the whole
 *  export: its stages each own a share of the bar, so it only moves forward. */
export interface ExportProgress {
	fraction: number;
	/** Since the export began (not since the render did). */
	elapsed_secs: number;
	/** Time left, once the render has a rate of its own (not counting the seconds ffmpeg spent opening the footage). */
	eta_secs?: number | null;
	/** Set while the work has not started: the machine is busy with another heavy job
	 *  (`waiting for the preview proxy`), or the animated titles are being drawn first
	 *  (`rendering animated titles`) — so the export is not hung at 0%. A multi-format export
	 *  sets it too. Wins over `stage`. */
	waiting?: string | null;
	/** What the export is doing now: `metering` (a static loudness normalization reads the mix first),
	 *  `starting` (ffmpeg opens the footage and decodes up to a range's start; nothing is written yet),
	 *  `rendering`, `finishing` (the muxer closes the file) and `measuring` (the written file is read back). */
	stage?: ExportStage | null;
	/** Set by a multi-format export: which file is rendering, of how many.
	 *  `fraction` then spans all of them. */
	variant?: number;
	total?: number;
	/** A multi-format export rendering every file in one pass: `variant` is then 0. */
	together?: boolean;
	/** Set while an export that is rendered in parts is under way (a span with more open at once than one
	 *  ffmpeg run should hold): which part, of how many, then `encoding` once every part is done and the
	 *  one last pass writes the file. A multi-format export sets it for the file in flight. */
	part?: ExportPart | null;
}

/** Where an export rendered in parts has got to (`kerf_core::ExportPart`). */
export interface ExportPart {
	/** The part being rendered, from 1. */
	index: number;
	/** How many parts the export is rendered in. */
	total: number;
	/** Every part is rendered and the file is being written. */
	encoding: boolean;
}

/** The first picture stream of a written file (`kerf_core::WrittenVideo`). */
export interface WrittenVideo {
	codec: string;
	width: number | null;
	height: number | null;
	fps: number | null;
	pix_fmt: string | null;
	/** Bits per second, when the container records it. */
	bit_rate: number | null;
}

/** The first sound stream of a written file (`kerf_core::WrittenAudio`). */
export interface WrittenAudio {
	codec: string;
	sample_rate: number | null;
	channels: number | null;
	/** Bits per second. */
	bit_rate: number | null;
}

/** A delivery's loudness target (`kerf_core::LoudnessTarget`). */
export interface LoudnessTarget {
	lufs: number;
	/** The true-peak ceiling, dBTP. */
	true_peak_dbtp: number;
}

/**
 * What an export wrote, read back from the file (`kerf_core::ExportReport`) — the answer of
 * `export_timeline` (one) and `export_variants` (one per file). `get_levels` measures the timeline's
 * mix; this is the FILE, which a lossy encode can move (an AAC re-encode raised a limited peak back
 * over its ceiling). Every field is null where it could not be read or does not apply.
 */
export interface ExportReport {
	path: string;
	size_bytes: number | null;
	/** ffprobe's format name (`mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`, `wav`). */
	container: string | null;
	duration_secs: number | null;
	/** The whole file's bit rate, bits per second. */
	bit_rate: number | null;
	video: WrittenVideo | null;
	audio: WrittenAudio | null;
	/** The file's own audio, metered. Null for a file with no sound. */
	loudness: LevelReading | null;
	/** The loudness target the export was asked for, if any. */
	target: LoudnessTarget | null;
	/** How far the file's true peak is over the target's ceiling, in dB, only when it is over it. */
	true_peak_over_db: number | null;
	/** Why part of the measurement is missing (the file could not be probed or metered, or the measurement
	 *  was stopped). The export itself succeeded. */
	problem: string | null;
}

/** Payload of the `import-progress` event, emitted while a 360 pair is stitched. */
export interface ImportProgress extends ExportProgress {
	/** The file the user picked, so a batch import can label which one is working. */
	path: string;
}

/**
 * Payload of the `analysis-progress` event: one step of an analysis pass.
 * Transcription can download a model and then run for minutes, so analysis
 * reports where it is rather than showing an indeterminate spinner.
 */
export interface AnalysisProgress {
	asset_id: string;
	/** `waiting` (queued behind a preview proxy) · `silence` · `scenes` · `loudness` · `rhythm` ·
	 *  `download_model` · `transcribe` · `done` */
	stage: string;
	fraction?: number | null;
	/** A short note, e.g. `84 MB / 142 MB`. */
	detail?: string | null;
}

/** Payload of the `smart-crop-progress` event: how much of the shots have been sampled. */
export interface SmartCropProgress {
	fraction: number;
}

/** Payload of the `model-progress` event, emitted while a speech model downloads. */
export interface ModelProgress {
	model: string;
	downloaded_bytes: number;
	total_bytes?: number | null;
	fraction?: number | null;
}

/** A downloadable whisper.cpp speech model. */
export interface SpeechModelInfo {
	name: string;
	approx_bytes: number;
	multilingual: boolean;
}

/** Which speech-to-text backend this build uses, and whether it is ready. */
export interface TranscriptionStatus {
	/** `libwhisper` (in-process) · `ffmpeg_filter` · `none` */
	backend: string;
	available: boolean;
	/** The Settings toggle: off, an analyzed asset has no transcript. */
	enabled: boolean;
	model?: string | null;
	model_path?: string | null;
	model_ready: boolean;
	approx_download_bytes?: number | null;
	models: SpeechModelInfo[];
	reason?: string | null;
}

/** One Kokoro voice. `downloaded` is whether its style file is already cached. */
export interface VoiceInfo {
	id: string;
	name: string;
	accent: 'us' | 'gb';
	gender: 'female' | 'male';
	downloaded: boolean;
}

/** Whether voiceover can run, and what the first use still has to fetch. */
export interface VoiceoverStatus {
	available: boolean;
	/** The runtime and model are cached; only a new voice may still download. */
	ready: boolean;
	approx_download_bytes: number;
	default_voice: string;
	voices: VoiceInfo[];
	reason: string | null;
}

/** Payload of the `voiceover-progress` event, from the GUI or an agent. */
export interface VoiceoverProgress {
	stage: 'download_runtime' | 'download_model' | 'download_voice' | 'synthesize' | 'waiting';
	fraction: number | null;
	/** A short note, e.g. `42 MB / 88 MB` or `3 of 7 sentences`. */
	detail: string | null;
}

/** The arguments `generate_voiceover` takes; all but the script are optional. */
export interface VoiceoverRequest {
	text: string;
	voice?: string;
	speed?: number;
	/** The audio track to land on; omitted, the "VO" track (made on first use). */
	trackId?: string;
	/** Where it starts; omitted, it is appended to the track's end. */
	timelineStart?: number;
	/** Caption the cut from the script once it is placed. */
	captions?: CaptionOptions;
}

export interface VoiceoverResult {
	asset: Asset;
	timeline: Timeline;
}

// ---- composed music (kerf_core::MusicScore, `compose_music`) ------------------------------

/** The sound of a piece (`kerf_core::MusicStyle`, snake_case on the wire). */
export type MusicStyle = 'tropical_house' | 'pop' | 'lofi' | 'cinematic' | 'ambient' | 'rock';

/** What a section is for; it decides which instruments play and how dense they are. */
export type SectionKind = 'intro' | 'verse' | 'build' | 'drop' | 'breakdown' | 'outro';

/** A moment the picture asks of the music. */
export type HitKind = 'impact' | 'riser' | 'dropout' | 'whoosh';

/** A run of whole bars with a chord per bar. */
export interface ScoreSection {
	kind: SectionKind;
	/** 1 to 128. */
	bars: number;
	/** One chord per bar, as names (`C`, `Em`, `F#m7`, `Gsus4`, `D/F#`) or roman numerals in the
	 *  score's key (`I`, `V`, `vi`, `IV`, `ii7`, `bVII`); a shorter list cycles. */
	chords: string[];
	/** 0 to 1: how loud and busy the section is. Absent: by kind (intro 0.3, verse 0.5, build 0.7,
	 *  drop 1, breakdown 0.3, outro 0.3). */
	energy?: number;
}

/** A sound effect placed at a time in the music. */
export interface ScoreHit {
	/** Seconds from the start of the music (not the timeline). */
	at_s: number;
	kind: HitKind;
}

/** General MIDI programs (0 to 127) that replace a style's instruments. `drums` is the kit's
 *  program (0 standard, 8 room, 16 power, 24 electronic, 25 808/909, 32 jazz, 40 brush, 48
 *  orchestral). */
export interface InstrumentOverrides {
	drums?: number;
	bass?: number;
	/** The sustained chords. */
	pad?: number;
	/** The rhythmic chords (stabs, arpeggios, power chords). */
	keys?: number;
	/** The hook. */
	lead?: number;
}

/** A piece of music in 4/4. */
export interface MusicScore {
	/** 40 to 240. */
	bpm: number;
	/** A tonic and an optional `m`: `G`, `F#m`, `Bb`, `Ebm`. */
	key: string;
	style: MusicStyle;
	sections: ScoreSection[];
	/** Absent when there are none (the backend omits an empty list). */
	hits?: ScoreHit[];
	/** Varies the humanization and the hook; the same score and seed are the same audio. 0 to 2^53 - 1
	 *  (9007199254740991): the largest integer JSON in JavaScript keeps exactly. */
	seed: number;
	instruments?: InstrumentOverrides;
}

/** What to fit a score to: the cuts of a video track, or the timeline's markers. Like the score and the request,
 *  it refuses a key it does not have (`deny_unknown_fields`). */
export interface ScoreAlign {
	to: 'cuts' | 'markers';
	/** With `cuts`: the video track (absent: the first). */
	track_id?: string;
	/** The most the tempo may change, as a fraction: 0.06 (absent) is 6 %, 0 keeps it, 0.25 at most. */
	max_tempo_change?: number;
}

/** The arguments of `compose_music` (`kerf_core::ComposeRequest`). */
export interface ComposeRequest {
	score: MusicScore;
	/** The audio track to land on; absent, the "M" track (made on first use). */
	track_id?: string;
	/** Where it starts on the timeline, seconds; absent, after what the track holds. */
	at?: number;
	align?: ScoreAlign;
	/** Replace this music with the new render instead of adding a clip: **the whole bed** goes — every clip of this
	 *  clip's asset on its track (what Fit to video made of one piece) becomes one clip of the new render, at the
	 *  earliest of their starts, with this clip's id, the first one's level, fade-in and effects and the last one's
	 *  fade-out. `at` and `track_id` are ignored. */
	replace_clip_id?: string;
}

/** Where one cut sits against the beat grid. */
export interface BeatReading {
	on_beat: boolean;
	on_downbeat: boolean;
	/** Seconds from the nearest beat; positive is late. */
	offset_s: number;
}

/** One cut, before and after aligning. */
export interface CutAlignment {
	/** The time as given (timeline seconds). */
	cut_s: number;
	/** The same moment in the music's own time. */
	music_s: number;
	before: BeatReading;
	after: BeatReading;
}

/** What fitting the score to the picture did (`kerf_core::AlignReport`). */
export interface AlignReport {
	/** The cuts that fall inside the music. */
	cuts: number;
	bpm_before: number;
	bpm_after: number;
	/** `bpm_after / bpm_before - 1`. */
	tempo_change: number;
	on_beat_before: number;
	on_downbeat_before: number;
	on_beat_after: number;
	on_downbeat_after: number;
	hits_added: ScoreHit[];
	details: CutAlignment[];
	notes: string[];
}

/** What `compose_music` made (`kerf_core::Composed`): the asset and clip, the score that was
 *  rendered (aligned, when asked) and what aligning did. */
export interface Composed {
	asset: Asset;
	clip: Clip;
	score: MusicScore;
	align: AlignReport | null;
	/** The clips the new one replaced — the whole music bed of `replace_clip_id`, in time order; empty when a clip
	 *  was added. */
	replaced: string[];
}

export interface ComposeResult extends Composed {
	timeline: Timeline;
}

/** One style, for a picker. */
export interface MusicStyleInfo {
	name: MusicStyle;
	label: string;
	default_bpm: number;
}

/** What composing has on disk and in flight (`kerf_core::MusicStatus`). */
export interface MusicStatus {
	/** The SoundFont is on disk: the next composition renders at once. */
	soundfont_ready: boolean;
	soundfont: string;
	/** What the first composition downloads, bytes (0 when it is ready). */
	approx_download_bytes: number;
	/** Compositions rendering now. */
	rendering: number;
	sample_rate: number;
	styles: MusicStyleInfo[];
}

/** Payload of the `music-progress` event, from the GUI or an agent. */
export interface MusicProgress {
	stage: 'download_soundfont' | 'waiting' | 'render';
	fraction: number | null;
	/** A short note, e.g. `12 MB / 32 MB`, `12 of 90 s`, or why a render is waiting. */
	detail: string | null;
}

// ---- sound effects (kerf_core::{SfxSpec, SfxPreset, SfxPlacement, SfxSuggestion}) ----------------

/** What an effect is, by role in an edit (`kerf_core::SfxCategory`, snake_case on the wire). */
export type SfxCategory = 'transition' | 'impact' | 'build' | 'ui' | 'texture';

/** Which sound (`kerf_core::SfxKind`, snake_case on the wire): the catalogue has one row for each. */
export type SfxKind =
	| 'whoosh'
	| 'swoosh'
	| 'swipe'
	| 'impact'
	| 'boom'
	| 'thud'
	| 'punch'
	| 'riser'
	| 'downlifter'
	| 'reverse_swell'
	| 'pop'
	| 'click'
	| 'ding'
	| 'notify'
	| 'bubble'
	| 'typewriter'
	| 'glitch'
	| 'tape_stop'
	| 'record_scratch'
	| 'shutter'
	| 'heartbeat';

/** What to synthesize, as the backend writes it (an asset's `sound_effect`): every field but `duration_s`, which is
 *  absent for the kind's own length. */
export interface SfxSpec {
	kind: SfxKind;
	/** How long the render is, seconds; absent is the kind's default. Clamped to the kind's range, held to whole milliseconds. */
	duration_s?: number;
	/** Semitones, -12 to 12 (default 0). */
	pitch: number;
	/** 0 to 1 (default 0.7): brightness, aggression and density. */
	intensity: number;
	/** 0 to 1 (default 0.3): how much reverb / echo rings after the sound. */
	tail: number;
	/** Played backwards: a swell into the hit. The hit point mirrors. */
	reverse: boolean;
	/** Varies the noise and the details; the same spec and seed are the same audio. 0 to 2^53 - 1. */
	seed: number;
}

/** A spec as a caller may write it: `{ kind: 'whoosh' }` alone is one, and the backend fills what is left out. */
export type SfxSpecInput = Pick<SfxSpec, 'kind'> & Partial<Omit<SfxSpec, 'kind'>>;

/** Where an effect's hit point is (`kerf_core::SfxAnchor`, tagged by `type`). */
export type SfxAnchor =
	| { type: 'hit'; seconds: number }
	| { type: 'peak'; fraction: number }
	| { type: 'end' };

/** One row of the catalogue (`sfx_catalogue`). */
export interface SfxPreset {
	kind: SfxKind;
	label: string;
	category: SfxCategory;
	/** What it sounds like and where it belongs in an edit. */
	description: string;
	default_duration_s: number;
	min_duration_s: number;
	max_duration_s: number;
	/** The hit point, before `reverse` mirrors it. */
	anchor: SfxAnchor;
	/** How far below the common peak the kind is meant to sit, dB (0 or less). */
	level_db: number;
}

/** One effect to place (`kerf_core::SfxPlacement`): a spec's fields flat, with where its hit lands. A key it does
 *  not have is an error naming it. */
export interface SfxPlacement extends SfxSpecInput {
	/** Where the effect's hit point lands on the timeline, seconds: a whoosh's peak, an impact's first transient, a
	 *  riser's end. The clip starts that much earlier (cut off at 0 when that would be before the start). */
	at: number;
	/** The clip's gain, dB (-60 to 12; default 0). */
	volume_db?: number;
	/** The audio track to put it on; absent, the first `FX` lane with room (`FX 2`, … made when none fits). */
	track_id?: string;
}

/** How many effects to suggest (`kerf_core::SfxDensity`). */
export type SfxDensity = 'subtle' | 'balanced' | 'punchy';

/** The picture event a suggestion is for (`kerf_core::SfxSource`). */
export type SfxSource = 'cut' | 'transition' | 'title' | 'punch_in' | 'music_drop' | 'marker' | 'opening';

/** What to suggest (`kerf_core::SfxSuggestRequest`); `{}` is the whole cut at balanced density. A key it does not
 *  have is an error naming it. */
export interface SfxSuggestRequest {
	/** Only suggestions whose hit falls in `[start, end)`, timeline seconds. */
	range?: TimeRange;
	density?: SfxDensity;
	/** Only effects of these categories; with `kinds`, an effect of either list is kept. */
	categories?: SfxCategory[];
	kinds?: SfxKind[];
}

/** One effect worth adding (`kerf_core::SfxSuggestion`). */
export interface SfxSuggestion {
	/** Exactly what `addSoundEffects` takes: the kind, its seed, where its hit lands and the gain (lower over speech).
	 *  Never names a track. */
	placement: SfxPlacement;
	source: SfxSource;
	/** Why, as a sentence: `Cut from interview.mp4 to broll.mp4 at 0:12.5`. */
	reason: string;
	/** 0 to 1: how much the moment wants a sound. */
	score: number;
}

/** What `add_sound_effects` answers: the clips placed, in the order asked, and the refreshed timeline. */
export interface SfxAdded {
	clips: Clip[];
	timeline: Timeline;
}

/** What `update_sound_effect` answers: the clip as it stands and the refreshed timeline. */
export interface SfxUpdated {
	clip: Clip;
	timeline: Timeline;
}

// ---- band balance (kerf_core::BandBalance, `get_band_balance`) -----------------------------

export type BandName = 'sub' | 'bass' | 'low_mid' | 'mid' | 'high_mid' | 'air';

/** A stretch of the measured sound, seconds in the target's own time. */
export interface BandWindow {
	start: number;
	end: number;
	label?: string;
}

/** Exactly one of `asset_id`, `clip_id` or `range` names the target. */
export interface BandBalanceRequest {
	/** An asset's whole sound; times are its own seconds. */
	asset_id?: string;
	/** A clip's source range at the clip's volume; times are source seconds. */
	clip_id?: string;
	/** The finished mix of the working timeline over this range (what `get_levels` measures);
	 *  times are timeline seconds. */
	range?: TimeRange;
	/** The windows to compare. Absent: a composed asset's score sections, else the music's
	 *  phrases, else eight equal windows. */
	sections?: BandWindow[];
}

/** One band in one window. */
export interface BandLevel {
	name: BandName;
	low_hz: number;
	high_hz: number;
	/** The band's energy as a level, dBFS; null when there is none. */
	energy_dbfs: number | null;
	/** The band's share of the window's energy across the six bands, percent. */
	share_pct: number;
}

/** What one window sounds like. */
export interface WindowBalance {
	start: number;
	end: number;
	label: string | null;
	/** The whole window's RMS, dBFS; null for silence. */
	rms_dbfs: number | null;
	/** Its K-weighted loudness, LUFS, ungated; null for silence. */
	loudness_lufs: number | null;
	/** The six bands, low to high; empty when the window held no frame to measure. */
	bands: BandLevel[];
	/** The band with the most energy. */
	dominant: BandName | null;
}

export interface BandBalance {
	target: 'asset' | 'clip' | 'mix';
	name: string;
	/** The measured stretch, in the target's seconds. */
	start: number;
	end: number;
	windows: WindowBalance[];
	/** The whole stretch as one window. */
	overall: WindowBalance;
	/** Where the windows came from. */
	windows_from: 'sections' | 'score' | 'phrases' | 'equal';
	/** What stands out, in words. */
	notes: string[];
}

/** What stem separation has on disk (`kerf_core::StemsStatus`): whether its first use still
 *  downloads the ONNX runtime or the Demucs model, how big that model is and the stems it makes. */
export interface StemsStatus {
	runtime_ready: boolean;
	model_ready: boolean;
	model_bytes: number;
	/** In order: `drums`, `bass`, `other`, `vocals`. */
	stems: string[];
}

/** Payload of the `stems-progress` event, the same shape as `voiceover-progress`. */
/** `audio-sync-progress`: a sync by audio waiting for the heavy-job slot (`detail` says why). */
export interface AudioSyncProgress {
	stage: 'waiting';
	fraction: null;
	detail: string | null;
}

export interface StemsProgress {
	stage: 'download_runtime' | 'download_model' | 'separate' | 'encode';
	fraction: number | null;
	/** A short note, e.g. `42 MB / 170 MB` or the stem being encoded. */
	detail: string | null;
}

/** What `place_stems` did (`kerf_core::StemsPlaced`): the stem assets, in `stems` order, and the
 *  clips laid under the separated clip (empty when only the library was asked for). */
export interface StemsPlaced {
	assets: Asset[];
	clips: Clip[];
}

/** The `separate_stems` answer: what was placed and the refreshed timeline. */
export interface StemsResult {
	placed: StemsPlaced;
	timeline: Timeline;
}

/** What the app was launched asking to open (kerf-app's `LaunchProject`, externally
 *  tagged): a `.kerf` that exists, or one named on the command line that is not
 *  there — never created, so the page says so instead. */
export type LaunchRequest = { open: string } | { missing: string };

/** An untitled project a crashed Kerf left behind (kerf-core `RecoveryInfo`), offered at launch. */
export interface RecoveryInfo {
	id: string;
	/** When its file was last written (ISO 8601), if known. */
	modified_at: string | null;
	assets: number;
	clips: number;
	/** Edits in its history, the initial state not counted. */
	edits: number;
	last_edit: string | null;
}

/** A newer signed release found on GitHub by the updater. */
export interface UpdateInfo {
	/** The available version, e.g. `0.18.0`. */
	version: string;
	/** The version this build is running. */
	current_version: string;
	/** Release date as published in `latest.json`, when present. */
	date?: string | null;
	/** Release notes (the GitHub release body), when present. */
	notes?: string | null;
}

export type TimeDisplay = 'timecode' | 'seconds';

/** App preferences — machine settings, not project state, so they live in the
 *  platform config directory rather than in the `.kerf` file. */
export interface AppSettings {
	/** Share of the machine one heavy job (analysis, transcription, proxy,
	 *  stitch, export) may take. Kerf runs one such job at a time; this is how
	 *  much of the computer that job gets. */
	cpu_percent: number;
	/** Which analyses run by themselves when media is imported: a master switch and
	 *  one per kind. (Replaces the old `transcribe` flag; the backend carries an
	 *  older settings file forward.) */
	auto_analysis: AutoAnalysis;
	/** How wide preview proxies are — 720, 1080 or 1280 pixels across, or 0 for none. */
	proxy_size: ProxySize;
	/** Which file the preview decodes. Export always reads the originals. */
	preview_source: PreviewSource;
	/** Whether the preview shades the delivery safe areas — where a phone's
	 *  own UI covers a vertical or square cut. */
	safe_areas: boolean;
	/** Whether the Preview panel is drawn by the GPU compositor in a native
	 *  surface for the frames it draws exactly, instead of FFmpeg's JPEG for
	 *  every frame (experimental; off by default). Frames it cannot draw, and
	 *  every failure, are the JPEG as ever. */
	gpu_preview: boolean;
	/** How times are written: frame-accurate `timecode` (the default) or plain `seconds`. */
	time_display: TimeDisplay;
	/** Open the project that was open last when Kerf starts without one named on its command line. */
	reopen_last_project: boolean;
	/** Whether importing media offers to tag the new assets afterwards. On by default; the prompt's own "don't ask
	 *  again" switches it off. */
	ask_tags_on_import: boolean;
	/** The `.kerf` files opened or saved most recently, newest first, at most 10. Kept by the app
	 *  (`recentProjects` prunes the ones that are gone, `clearRecentProjects` empties it); a settings
	 *  patch cannot write it. */
	recent_projects: string[];
	/** The single dock arrangement saved before there were workspaces (dockview's
	 *  serialized layout). Read only to migrate it into the Edit workspace when
	 *  `workspaces` is absent; nothing writes it any more. */
	layout: unknown | null;
	/** The color theme, validated by `theme.ts` on the way back in. */
	theme: unknown | null;
	/** The workspaces: `{ active, layouts: { <workspace>: <dockview layout> },
	 *  library: { tab, collapsed } }`, validated by `workspaces.ts` on the way
	 *  back in. Opaque to the backend. */
	workspaces: unknown | null;
	/** What the user changed about the keyboard shortcuts: `{ version, bindings:
	 *  { <action id>: [<chord>, …] } }`, only the actions they touched, validated
	 *  and migrated by `keymap.ts` on the way back in. Opaque to the backend. */
	keybindings: unknown | null;
	/** Whether an agent's edits are held back for the user to review (the default). Changed on the
	 *  Agent settings page (`setMcpSettings`); the agent cannot turn it off. */
	agent_review: boolean;
	/** Whether the embedded MCP server listens at all. */
	mcp_enabled: boolean;
	/** Where it listens (`host:port`); `null` is `127.0.0.1:7777`. `KERF_MCP_ADDR` in the environment
	 *  still overrides it (`McpConnectionInfo.env_override`). */
	mcp_addr: string | null;
	/** Whether a request must carry `Authorization: Bearer <token>`. The token itself is never in the
	 *  settings view: ask `mcpConnectionInfo`. */
	mcp_require_token: boolean;
	/** The editing defaults — `{ snap, still_seconds, title_seconds }` — read by `editing-prefs.ts`, which
	 *  clamps what it finds. Opaque to the backend. */
	editing: unknown | null;
}

/** What `setSettings` may write: any preference but the recent-projects list, which the app keeps itself
 *  as projects open and save (the backend refuses a patch that names it). */
export type SettingsPatch = Partial<Omit<AppSettings, 'recent_projects'>>;

/** `AppSettings` resolved against the engine, which is what the settings
 *  surface renders: the effective percentage (an environment override may win
 *  over the stored one), the cores it works out to, and the machine it is a
 *  share of. */
export interface SettingsView extends AppSettings {
	cpu_cores: number;
	cpu_threads: number;
	cpu_min_percent: number;
}

/** The dialog's baseline options (a preset is applied over this). One deliberate
 *  departure from the bare Rust `Default`: source decode is `hwaccel: 'auto'`,
 *  so exports GPU-decode where the machine can (ffmpeg falls back to software
 *  at init, and the engine retries a failed hardware render in software). */
export const DEFAULT_EXPORT_OPTIONS: ExportOptions = {
	container: 'mp4',
	rate_control: 'crf',
	fit: 'contain',
	include_audio: true,
	faststart: false,
	gif_loop: true,
	hwaccel: 'auto'
};

export { clipDuration } from './speed-ramp';

/** What detaching sound from several clips did (`AudioDetached` in `crates/kerf-app/src/lib.rs`, the
 *  answer of `extract_audio` and `detach_audio_clips`): the refreshed timeline, how many clips were
 *  detached, and the ones left alone with the reason (a locked track, an asset without audio, a sound
 *  already detached). */
export interface AudioDetached {
	timeline: Timeline;
	detached: number;
	skipped: { clip_id: string; reason: string }[];
}

// ---- Motion presets (B8a) -------------------------------------------------------

/** What a motion preset does in a clip's life (`MotionKind`). */
export type MotionKind = 'entrance' | 'exit' | 'emphasis';

/** How a preset meets the keys the clip already has (`MotionMode`): `add` (the default) replaces the clip's keys of
 *  those numbers inside the window and keeps the ones outside; `replace` gives them exactly the preset's keys. */
export type MotionMode = 'replace' | 'add';

/** A motion preset's wire name (`MotionPreset`): a direction names where the picture comes from / goes to. */
export type MotionPreset =
	| 'fade_in'
	| 'slide_in_from_left'
	| 'slide_in_from_right'
	| 'slide_in_from_top'
	| 'slide_in_from_bottom'
	| 'zoom_in'
	| 'pop_in'
	| 'spin_in'
	| 'drop_in'
	| 'fade_out'
	| 'slide_out_to_left'
	| 'slide_out_to_right'
	| 'slide_out_to_top'
	| 'slide_out_to_bottom'
	| 'zoom_out'
	| 'pop_out'
	| 'spin_out'
	| 'drop_out'
	| 'pulse'
	| 'wiggle'
	| 'bounce'
	| 'push_in'
	| 'pull_out';

/** One preset as the picker lists it (`MotionPresetInfo`). */
export interface MotionPresetInfo {
	id: MotionPreset;
	label: string;
	kind: MotionKind;
	/** Seconds; absent for a preset that fills the rest of the clip (`push_in`, `pull_out`). */
	default_duration?: number;
	hint: string;
	/** The numbers it keys. */
	properties: Property[];
}

/** A clip a preset was written into and the window it covers, seconds from the clip's start (`MotionApplied`). The
 *  Tauri command answers the slimmer `MotionWindow`: the clip is already in the timeline it returns. */
export interface MotionApplied {
	clip: Clip;
	start: number;
	end: number;
}

/** The window a preset was written over on one clip, seconds from the clip's start (the clamped one). */
export interface MotionWindow {
	clip_id: string;
	start: number;
	end: number;
}

/** What applying a preset did: the refreshed timeline and the window each clip got (`MotionApplication`). */
export interface MotionApplication {
	timeline: Timeline;
	applied: MotionWindow[];
}

// ---- Live behaviours (B8b) ---------------------------------------------------

/** What a behaviour does (`BehaviorKind`, the `kind` tag). */
export type BehaviorKind = 'sway' | 'breathe' | 'spin' | 'drift' | 'shake';

/** One live behaviour of a clip (`Behavior`, tagged by `kind`). `phase` is clip-local seconds added to the clock
 *  (a head cut shifts it, so the motion carries on); `seed` and `angle` default to 0 when absent. */
export type Behavior =
	/** Rotation `+= amount * sin(2 pi speed u)`: `amount` degrees each way (0..180), `speed` Hz (0..30). */
	| { kind: 'sway'; amount: number; speed: number; phase?: number }
	/** Scale `*= 1 + amount * sin(2 pi speed u)`: `amount` a fraction of the zoom (0..0.9), `speed` Hz (0..30). */
	| { kind: 'breathe'; amount: number; speed: number; phase?: number }
	/** Rotation `+= speed * 360 * u`: `speed` turns a second, signed (-10..10). */
	| { kind: 'spin'; speed: number; phase?: number }
	/** Position `+= speed * u * (cos angle, sin angle)`: `speed` frame widths / heights a second, signed (-5..5);
	 *  `angle` degrees from the right, 90 is down. */
	| { kind: 'drift'; speed: number; angle?: number; phase?: number }
	/** Position `+=` three sines per axis: `amount` of the frame (0..0.25), `speed` Hz (0..30), the same `seed` the
	 *  same shake. */
	| { kind: 'shake'; amount: number; speed: number; seed?: number; phase?: number };

/** The limits of `Behavior`, the core's (`MAX_BEHAVIORS` and the rest): a write past one is refused, a stored
 *  number past one is read held to it. */
export const MAX_BEHAVIORS = 8;
export const MAX_BEHAVIOR_HZ = 30;
export const MAX_SWAY_DEGREES = 180;
export const MAX_BREATHE = 0.9;
export const MAX_SPIN_TURNS = 10;
export const MAX_DRIFT = 5;
export const MAX_SHAKE = 0.25;
export const MAX_BEHAVIOR_PHASE = 1_000_000;

/** What baking a clip's behaviours into keys wrote (`Baked`). */
export interface Baked {
	/** The numbers that now carry keys. */
	properties: Property[];
	/** Keys written to each. */
	keys: number;
	/** The frame rate they were sampled at. */
	fps: number;
}

/** A clip a bake wrote keys into and what it wrote (`BakedClip`). The Tauri command answers the slimmer
 *  `BakeReport`: the clip is already in the timeline it returns. */
export interface BakedClip {
	clip: Clip;
	baked: Baked;
}

/** What a bake did to one clip, as the Tauri command reports it (`BakeReport`). */
export interface BakeReport extends Baked {
	clip_id: string;
}

/** What baking did: the refreshed timeline and what was written per clip (`BakeApplication`). */
export interface BakeApplication {
	timeline: Timeline;
	baked: BakeReport[];
}

// ---- GPU preview (A2) -------------------------------------------------------

/** What the GPU preview is doing on this machine (`GpuPreviewStatus` in
 *  `crates/kerf-app/src/gpu_preview.rs`). */
export interface GpuPreviewStatus {
	/** The setting. */
	enabled: boolean;
	/** The platform has a surface technique (it may still fail to build one). */
	supported: boolean;
	/** `window`: the main window's own surface under a transparent webview.
	 *  `child`: a borderless child window over the panel (X11). */
	technique: 'window' | 'child' | null;
	/** A device and a surface are up. */
	ready: boolean;
	/** The page can draw over the picture (titles, guides, the trim monitor). */
	overlays: boolean;
	adapter: string | null;
	/** The adapter is a CPU rasterizer (lavapipe, WARP). */
	software: boolean;
	/** Why the GPU preview is not in use: no technique here, or the last failure. */
	reason: string | null;
}

/** One kind of reason a preview frame went through FFmpeg, counted over the session
 *  (`RefusalCount` in `gpu_preview/diagnostics.rs`). */
export interface GpuRefusalCount {
	/** A stable key (`mask`, `mixed_matrices`, `text_too_small`, `self_test_pending`, …). */
	kind: string;
	count: number;
	/** The first message of this kind seen this session. */
	example: string;
}

/** What this session's preview frames did (`GpuDiagnostics` in `gpu_preview.rs`): local,
 *  in memory, for finding out which refusals matter on real projects. */
export interface GpuPreviewDiagnostics {
	tally: {
		gpu_frames: number;
		ffmpeg_frames: number;
		/** Frame requests a newer one replaced before they were drawn. */
		superseded: number;
		/** Most frequent first. */
		refusals: GpuRefusalCount[];
	};
	/** How often the frames' inputs came from the held snapshot, and how often they were taken
	 *  from the project (its lock, its timeline parsed). */
	snapshots: { served: number; taken: number };
	requests: { posted: number; superseded: number };
	/** What the play sessions did (`PlayTally`): sessions the GPU played, first requests it
	 *  declined, sessions that ended in the stream, frames drawn / skipped / repeated. */
	play: { sessions: number; declined: number; handed_over: number; frames: number; skipped: number; repeats: number };
	/** The parity self-test against this machine's FFmpeg: running, passed or closed (and why). */
	self_test: string | null;
	adapter: string | null;
	software: boolean;
}

/** Where the Preview frame is in the window, in device pixels relative to the
 *  webview's top-left corner (`BoundsReport` in `gpu_preview.rs`). */
export interface PreviewBoundsReport {
	x: number;
	y: number;
	width: number;
	height: number;
	/** The webview's own size in device pixels. */
	viewport_width: number;
	viewport_height: number;
	/** Whether the native surface should be showing. */
	visible: boolean;
	/** The colour around the picture, `#rrggbb`. */
	matte: string | null;
	/** The colour of the rest of the surface, `#rrggbb` — what a transparent webview shows
	 *  where no element of the page paints. */
	backdrop: string | null;
	/** Which report this is, counting up across every Preview the page has had; the
	 *  backend ignores one older than the newest it has seen. */
	seq: number;
}

/** What one GPU frame took (`GpuTimings`). */
export interface GpuTimings {
	width: number;
	height: number;
	decode_ms: number;
	composite_ms: number;
	present_ms: number;
}

/** Which request of which pump a `get_preview_frame` is. IPC calls can reach the backend in the
 *  other order than they were made; the backend draws only the newest number of a `client`
 *  and answers `superseded` to an older one that arrives after it. */
export interface PreviewOrder {
	client: string;
	seq: number;
}

/** The first frame ahead that the GPU will not draw during playback (`AheadInfo` in
 *  `crates/kerf-app/src/gpu_preview/play.rs`), and where a stream that carries on from the compositor
 *  has to start. */
export interface PlayAhead {
	/** The time of that frame, timeline seconds. */
	at: number;
	/** The time to start the stream at: at or before `at`, with no transition window open. */
	handover: number;
	/** A stable key for the reason (`mask`, `reversed`, `moving_text`, `cursor`, …). */
	kind: string;
	why: string;
}

/** What a forward `get_preview_frame` says about its play session (`PlayInfo`). */
export interface PlayInfo {
	session: number;
	/** The timeline time of the session's first frame (a frame of the project's grid). */
	start: number;
	/** The frame drawn, counted from `start`. */
	frame: number;
	/** The request fell in the frame already on the screen: nothing was drawn. */
	repeat: boolean;
	ahead: PlayAhead | null;
	/** To a `ready` request: the time from which the GPU takes the play back from the stream (its play is made
	 *  ready there), or null while no such time is within reach (`play.ahead` says what is in the way). */
	resume?: number | null;
}

/** The answer of `get_preview_frame` (`PreviewFrameResult`): which renderer made the
 *  frame, and the JPEG when it was FFmpeg. */
export interface PreviewFrameResult {
	/** `gpu`: drawn in the native surface (the webview shows it by showing nothing
	 *  over it). `ffmpeg`: the `frame` data URL. `superseded`: a newer request took this
	 *  one's place before it was drawn (the backend keeps only the newest); nothing was
	 *  drawn and the answer is ignored. `stream` (to a forward request only): the GPU does
	 *  not play this session, or no longer, and the page plays the stream, from
	 *  `play.ahead.handover` when there is one. */
	renderer: 'gpu' | 'ffmpeg' | 'superseded' | 'stream' | 'ready';
	frame: string | null;
	/** Why the GPU did not draw it (empty when it did). */
	reasons: string[];
	timings: GpuTimings | null;
	/** To a forward request: the session's state. */
	play?: PlayInfo | null;
}

/** A clip as a gesture under way would leave it, on the track it would be on (`DraftClip` in
 *  `crates/kerf-app/src/gpu_preview/draft.rs`). */
export interface DraftClip {
	track_id: string;
	clip: Clip;
}

/** What a gesture under way would write, sent with its frames (`PreviewDraft`): the clips and titles it
 *  changes, each whole. The GPU preview draws the frame as the cut would be with it written; nothing is. */
export interface PreviewDraft {
	clips: DraftClip[];
	overlays: TextOverlay[];
}

/** How the page asks for a frame of GPU playback: the session it belongs to. */
export interface PlayAsk {
	session: number;
	/** Not a frame: where, from the time asked, the GPU can take the play back from the stream (`hint: ready`). */
	ready?: boolean;
}

/** The answer of `fit_music` (`MusicFitted` in `crates/kerf-app/src/lib.rs`): the refreshed
 *  timeline and the report. */
export interface MusicFitted {
	timeline: Timeline;
	report: MusicFitReport;
}

// ---- agent connection (MCP server): `crates/kerf-app/src/mcp_commands.rs` ---------

/** What the Agent settings page shows and the user copies into an MCP client
 *  (`McpConnectionInfo`). The token is a secret: show it on request, never log it. */
export interface McpConnectionInfo {
	/** Whether the server is switched on. */
	enabled: boolean;
	/** Where a client connects, e.g. `http://127.0.0.1:7777/mcp`. */
	url: string;
	/** Whether requests must carry `Authorization: Bearer <token>`. */
	require_token: boolean;
	/** The bearer token (made on the first start), even while it is not required. */
	token: string | null;
	/** Why the token is not what it should be: it could not be saved (it works until Kerf quits) or
	 *  could not be made (the server stays off while one is required). `null` when it is fine. */
	token_problem: string | null;
	/** Whether the listener is up now. */
	running: boolean;
	/** Why it is not, when it should be (the port is taken, the address is refused). */
	error: string | null;
	/** The stored address (`null` is the default `127.0.0.1:7777`); what the address field edits. */
	configured_addr: string | null;
	/** `KERF_MCP_ADDR` is set in the environment and wins over `configured_addr`. */
	env_override: boolean;
	/** Whether an agent's edits wait for the user's review (the default). */
	agent_review: boolean;
	/** The one-line command that adds Kerf to Claude Code. */
	claude_code_command: string;
	/** A `mcpServers` JSON snippet for any other MCP client. */
	generic_json_config: string;
	/** The tool sets a client may ask for with `?tools=core,titles` on the URL; without, it gets every tool. */
	tool_sets: McpToolSet[];
}

/** One MCP tool set (`crates/kerf-app/src/mcp/toolsets.rs`). */
export interface McpToolSet {
	name: string;
	description: string;
	/** How many tools it lists; the essentials (guide, summary, staged diff, job status) come with any selection. */
	tools: number;
}

/** The agent switches `set_mcp_settings` takes — any of them. The server restarts by itself when
 *  the address or the enabled switch moves; a token or requirement change applies at once. */
export interface McpSettingsPatch {
	mcp_enabled?: boolean;
	/** `host:port`; `null` goes back to the default. A network address needs the token. */
	mcp_addr?: string | null;
	mcp_require_token?: boolean;
	agent_review?: boolean;
}

/** What the editor window shows, reported for an agent's `get_editor_context`
 *  (`crates/kerf-app/src/editor_context.rs`). Timeline seconds; ids are the live timeline's. */
export interface UiContext {
	playhead: number;
	selected_clip_ids: string[];
	mark_in: number | null;
	mark_out: number | null;
	visible_range: { start: number; end: number } | null;
	focused_panel: string | null;
	selected_asset_id: string | null;
}

/** A scope the Scopes panel draws (`kerf_core::scopes::ScopeKind`). */
export type ScopeKind = 'waveform' | 'parade' | 'vectorscope' | 'histogram';
/** The Y'CbCr matrix the scopes read the frame with; `auto` is the delivery's. */
export type ScopeMatrix = 'auto' | 'bt601' | 'bt709' | 'bt2020';

export interface ScopeGridSize {
	columns: number;
	rows: number;
	vector: number;
}

/** One channel over every pixel of the frame: levels 0..1, pixels clipped at 0 and at 1. */
export interface ScopeChannel {
	min: number;
	max: number;
	mean: number;
	clipped_low: number;
	clipped_high: number;
	clipped_low_pct: number;
	clipped_high_pct: number;
}

/** Mean Cb / Cr (-0.5..0.5), the hue (degrees from +Cb) and saturation (|CbCr| / 0.5) of
 *  that mean, and the mean of every pixel's saturation. */
export interface ScopeChroma {
	cb: number;
	cr: number;
	hue_deg: number;
	saturation: number;
	mean_saturation: number;
}

export interface ScopePeak {
	cb: number;
	cr: number;
	hue_deg: number;
	saturation: number;
	share: number;
}

export interface ScopeSummary {
	width: number;
	height: number;
	matrix: 'bt601' | 'bt709' | 'bt2020';
	red: ScopeChannel;
	green: ScopeChannel;
	blue: ScopeChannel;
	luma: ScopeChannel;
	chroma: ScopeChroma;
	peaks: ScopePeak[];
}

/** Counts: `counts[row * columns + column]`, row 0 the lowest level (the most negative Cr on
 *  the vectorscope, Cb across). */
export interface ScopeGrid {
	columns: number;
	rows: number;
	counts: number[];
}

/** 256 bins per channel. */
export interface ScopeHistogram {
	red: number[];
	green: number[];
	blue: number[];
	luma: number[];
}

/** `kerf_core::scopes::Scopes`: the summary always, the grids that were asked for. */
export interface Scopes {
	summary: ScopeSummary;
	waveform?: ScopeGrid;
	/** Red, green, blue. */
	parade?: ScopeGrid[];
	vectorscope?: ScopeGrid;
	histogram?: ScopeHistogram;
}

/** How the Preview plays a span (`kerf_core::render_bar::BarState`). */
export type BarState = 'gpu' | 'stream' | 'stall';

export interface BarSpan {
	start: number;
	end: number;
	state: BarState;
	/** A stable key for the reason (`reversed`, `proxy_wait`, `mask`, `gpu`, …). */
	kind: string;
	reason: string;
}

/** `kerf_core::render_bar::RenderBar`: with the GPU preview off only the stalls are listed. */
export interface RenderBar {
	gpu_preview: boolean;
	spans: BarSpan[];
}
