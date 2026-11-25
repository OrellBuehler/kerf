// The Inspector follows the task. Each workspace names the blocks of a clip's Inspector it opens on and which
// sections are open until the user says otherwise; what the user opens or closes, and how far they had scrolled,
// is remembered per workspace for as long as the app runs (the Inspector's panel is rebuilt with every workspace
// switch, so a `<details>` that held that state itself lost it on the way out). Pure: no runes, no DOM; the
// component reads the active workspace and hands it in.
//
// A *block* is what the Inspector places: one section, or a few that stay together (`grade` is Curves, Color
// wheels and LUT). A *section id* is what open / closed is kept under: the lower-case, hyphenated title of its
// `InspectorSection` (`sectionId`), so a component that draws its own section needs no id of its own.

/** The five arrangements the Inspector has a view of. A saved workspace of the user's own (an id the app made up)
 *  is none of them and reads as Edit. */
export const WORKSPACE_KEYS = ['edit', 'color', 'audio', 'motion', 'deliver'] as const;
export type WorkspaceKey = (typeof WORKSPACE_KEYS)[number];

/** Every block a clip's Inspector can place, in the order Edit shows them (what the panel always showed). */
export const BLOCKS = [
	'timing',
	'sound-effect',
	'composed-music',
	'volume',
	'music',
	'fades',
	'title',
	'speed',
	'360-reframe',
	'transform',
	'behaviours',
	'framing',
	'color',
	'grade',
	'stabilize',
	'mask',
	'transition',
	'audio-effects',
	'video-effects',
	'animation',
	'advanced'
] as const;
export type BlockId = (typeof BLOCKS)[number];

/** The sections a block that is more than one section is made of. */
export const BLOCK_SECTIONS: Partial<Record<BlockId, readonly string[]>> = {
	title: ['title', 'template', 'text'],
	grade: ['curves', 'color-wheels', 'lut']
};

/** What a section is kept under: its title, lower-cased, words joined by a hyphen (`Color wheels` is `color-wheels`). */
export function sectionId(title: string): string {
	return title
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
}

/** What a workspace asks of the Inspector: the blocks it puts first (in this order; the rest follow in Edit's), and
 *  the sections it opens and closes where the section itself would have been the other way (`open` on an
 *  `InspectorSection`). */
export interface Profile {
	first: readonly BlockId[];
	open: readonly string[];
	close: readonly string[];
	/** The order of the grade block's sections, when the workspace wants other than `BLOCK_SECTIONS.grade` (Curves, Color
	 *  wheels, LUT): every one of them, once. */
	gradeOrder?: readonly GradeSection[];
}

/** The three sections of the `grade` block. */
export type GradeSection = 'curves' | 'color-wheels' | 'lut';

/** Sections that are open by their own account but belong to another task's workspace: a picture's level and its
 *  timing are not what a colourist, an animator or a delivery is about, so they fold away there. */
const OFF_TASK = ['timing', 'volume', 'sound-effect', 'composed-music'];

export const PROFILES: Readonly<Record<WorkspaceKey, Profile>> = {
	// Today's panel, untouched: Timing and the level first, the rest as it always came.
	edit: { first: [], open: [], close: [] },
	// The grade first: the wheels and the LUT, then the curves, and under them the numbers (a colourist's panel at 1366×768
	// has 468 px: with the wheels first all three, their boxes and the LUT row are in view); a mask and the effects chain
	// beside them. The wheels, the LUT and the numbers are open, the curves folded.
	color: {
		first: ['grade', 'color', 'mask', 'video-effects'],
		open: ['color-wheels', 'lut', 'color'],
		close: OFF_TASK,
		gradeOrder: ['color-wheels', 'lut', 'curves']
	},
	// The sound: its level, its fades, its effects, and the music's fit.
	audio: {
		first: ['sound-effect', 'composed-music', 'volume', 'fades', 'audio-effects', 'music'],
		open: ['volume', 'fades', 'audio-effects'],
		close: ['timing']
	},
	// The movement: the pose, its keys, the speed, what moves it live, the crop that pushes in and the 360 camera.
	motion: {
		first: ['transform', 'animation', 'speed', 'behaviours', 'framing', '360-reframe'],
		open: ['transform', 'animation'],
		close: OFF_TASK
	},
	// The frame the cut is delivered in: how the picture is cropped and framed.
	deliver: {
		first: ['framing', '360-reframe', 'transform'],
		open: ['framing'],
		close: OFF_TASK
	}
};

/** The workspace whose view an id gets: itself when it is one of the five, else Edit's (a workspace the user saved
 *  under an id of their own has no view of its own). */
export function profileKey(workspace: string | null | undefined): WorkspaceKey {
	return (WORKSPACE_KEYS as readonly string[]).includes(workspace ?? '') ? (workspace as WorkspaceKey) : 'edit';
}

export const profileOf = (workspace: string | null | undefined): Profile => PROFILES[profileKey(workspace)];

/** The grade block's sections in the order `workspace` draws them. */
export const gradeSectionOrder = (workspace: string | null | undefined): readonly GradeSection[] =>
	profileOf(workspace).gradeOrder ?? (BLOCK_SECTIONS.grade as readonly GradeSection[]);

/** What is true of the selected clip, as far as which blocks it has. */
export interface ClipFacts {
	/** Something is drawn from the clip: footage with a picture, on a picture track. */
	picture: boolean;
	hasAudio: boolean;
	/** The picture's sound was detached: the level and effects live on the audio clip that carries it now. */
	soundDetached: boolean;
	/** A generated title on a track. */
	titleClip: boolean;
	soundEffect: boolean;
	composedMusic: boolean;
	/** Music with a fitted bar grid: *Fit to video* has something to say. */
	musicOffer: boolean;
	/** The footage is 360, or the clip is already reframed. */
	has360: boolean;
}

/** The blocks a clip has, in Edit's order. */
export function clipBlocks(f: ClipFacts): BlockId[] {
	const has: Record<BlockId, boolean> = {
		timing: true,
		'sound-effect': f.soundEffect,
		'composed-music': f.composedMusic,
		volume: f.hasAudio,
		music: f.musicOffer,
		fades: true,
		title: f.titleClip,
		speed: !f.titleClip,
		'360-reframe': f.picture && f.has360,
		transform: f.picture,
		behaviours: f.picture,
		framing: f.picture,
		color: f.picture,
		grade: f.picture,
		stabilize: f.picture,
		mask: f.picture,
		transition: true,
		'audio-effects': f.hasAudio && !f.soundDetached,
		'video-effects': f.picture,
		animation: f.picture,
		advanced: f.picture && !f.has360
	};
	return BLOCKS.filter((b) => has[b]);
}

/** `present` (in Edit's order) as `workspace` places it: the blocks it puts first that are there, then the rest in
 *  the order they came. */
export function orderBlocks(workspace: string | null | undefined, present: readonly BlockId[]): BlockId[] {
	const first = profileOf(workspace).first.filter((b) => present.includes(b));
	return [...first, ...present.filter((b) => !first.includes(b))];
}

/** Whether a section is open in `workspace` before the user has chosen: the workspace's say, else the section's own
 *  (`intrinsic`, the `open` of its `InspectorSection`). */
export function defaultOpen(workspace: string | null | undefined, id: string, intrinsic: boolean): boolean {
	const p = profileOf(workspace);
	if (p.close.includes(id)) return false;
	if (p.open.includes(id)) return true;
	return intrinsic;
}

/** What the Inspector is showing, which is what a scroll position is kept for: a clip's sections and an asset's
 *  have nothing to do with each other's height. */
export type Subject = 'clip' | 'title' | 'animated-title' | 'template' | 'asset' | 'empty';

/** What the user has opened, closed and scrolled to, by workspace. Only a choice that differs from what the
 *  workspace would have done is kept, so a default that moves later is not held back by a section nobody touched. */
export class InspectorMemory {
	#open = new Map<string, Map<string, boolean>>();
	#scroll = new Map<string, number>();

	/** Whether the section is open now: the user's choice in this workspace, else the default. */
	isOpen(workspace: string | null | undefined, id: string, intrinsic: boolean): boolean {
		return this.#open.get(workspace ?? '')?.get(id) ?? defaultOpen(workspace, id, intrinsic);
	}

	/** The user opened or closed a section in this workspace. */
	set(workspace: string | null | undefined, id: string, open: boolean, intrinsic: boolean): void {
		const key = workspace ?? '';
		const mine = this.#open.get(key);
		if (open === defaultOpen(workspace, id, intrinsic)) {
			mine?.delete(id);
			if (mine?.size === 0) this.#open.delete(key);
			return;
		}
		if (mine) mine.set(id, open);
		else this.#open.set(key, new Map([[id, open]]));
	}

	scroll(workspace: string | null | undefined, subject: Subject): number {
		return this.#scroll.get(`${workspace ?? ''}\u0000${subject}`) ?? 0;
	}

	setScroll(workspace: string | null | undefined, subject: Subject, top: number): void {
		const key = `${workspace ?? ''}\u0000${subject}`;
		if (top > 0) this.#scroll.set(key, Math.round(top));
		else this.#scroll.delete(key);
	}

	/** Forget everything (a test; the app keeps it until it quits). */
	clear(): void {
		this.#open.clear();
		this.#scroll.clear();
	}
}

/** The one memory: the Inspector's panel is rebuilt on every workspace switch, and this outlives it. */
export const inspectorMemory = new InspectorMemory();
