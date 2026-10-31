// The Sound FX panel's state (Svelte 5 runes): which sound is selected and how it is shaped, the audition, and the suggestions
// of auto sound design. It is a singleton because a workspace switch rebuilds the dock's panels and none of this may be lost
// with them — the pick, the shape being tried, a list of moments someone has just looked through. The panel and the
// Inspector's Sound effect section read and act through it; the pure rules are `sfx-panel.ts`.
//
// Nothing here writes to the cut until Add: a slider moved in the panel changes only the draft, and the audition renders the
// draft (`preview_sound_effect`: no edit, no asset).

import { previewSoundEffect, sfxCatalogue, suggestSoundEffects } from './api';
import { editor, PreviewReadOnly } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import { failure, tidyReason } from './failure-copy';
import { report } from './ops';
import { Auditioner } from './sfx-audition';
import { SFX_KINDS, defaultSfxSpec } from './sfx';
import {
	addedLine,
	dragOf,
	filterPresets,
	isSfxCancelled,
	nextSeed,
	patchSpec,
	pickAll,
	pickNone,
	pickedPlacements,
	placementOf,
	stampOf,
	suggestRequest,
	togglePick,
	type CategoryFilter,
	type Picks
} from './sfx-panel';
import type { SfxDensity, SfxKind, SfxPlacement, SfxPreset, SfxSpec, SfxSpecInput, SfxSuggestion } from './types';

/** What Audition says in the browser harness, which synthesizes nothing. */
export const AUDITION_UNAVAILABLE = 'Audition plays in the desktop app — the browser preview makes no sound.';
/** How long that notice stays: it is a quiet word, not a state. */
const NOTE_MS = 6000;

/** Who is auditioning: the panel's draft, or the Inspector's section for a clip. */
export type AuditionOwner = 'panel' | `clip:${string}`;

class SfxPanelState {
	catalogue = $state.raw<SfxPreset[]>([]);
	status = $state<'idle' | 'loading' | 'ready' | 'error'>('idle');
	loadError = $state<string | null>(null);

	query = $state('');
	category = $state<CategoryFilter>('all');
	/** The preset picked in the grid, and the spec being tried (the kind's defaults until a slider moves). */
	kind = $state<SfxKind | null>(null);
	spec = $state.raw<SfxSpec | null>(null);
	gainDb = $state(0);

	// ---- audition
	owner = $state<AuditionOwner | null>(null);
	rendering = $state(false);
	playing = $state(false);
	note = $state<string | null>(null);
	#noteFor = $state<AuditionOwner | null>(null);

	// ---- auto sound design
	density = $state<SfxDensity>('balanced');
	autoCategory = $state<CategoryFilter>('all');
	finding = $state(false);
	found = $state.raw<SfxSuggestion[] | null>(null);
	foundStamp = $state('');
	picks = $state.raw<Picks>(pickNone());
	findError = $state<string | null>(null);
	/** What the last add said, shown where the list was. */
	lastAdded = $state<number | null>(null);

	#player = new Auditioner();
	#noteTimer: ReturnType<typeof setTimeout> | null = null;
	#ticket = 0;
	#findTicket = 0;
	#loading: Promise<void> | null = null;

	constructor() {
		this.#player.onchange = (s) => {
			this.playing = s === 'playing';
			if (s === 'idle' && !this.rendering) this.owner = null;
		};
	}

	/** The presets the search and the category show. */
	get visible(): SfxPreset[] {
		return filterPresets(this.catalogue, this.query, this.category);
	}

	get preset(): SfxPreset | null {
		return this.catalogue.find((p) => p.kind === this.kind) ?? null;
	}

	/** The cut as it is now, for telling a list of moments found on an older one. */
	get stamp(): string {
		const seq = editor.history.find((r) => r.current)?.seq ?? null;
		return `${stampOf(seq, editor.currentPath)}@${editor.loadEpoch}`;
	}

	get stale(): boolean {
		return this.found !== null && this.foundStamp !== this.stamp;
	}

	/** Why an edit is off right now, or null (the same words the Compose dialog uses). */
	get blocked(): string | null {
		if (editor.previewingStaged) return 'Exit the proposal preview to edit';
		if (editor.busy) return 'Another edit is running';
		return null;
	}

	// ---- the catalogue

	/** Read the catalogue, once: a second call while it is being read answers when that one does, and one after it has been read
	 *  answers at once. A read that failed is tried again by the next call — which is the user's (Try again, a key), never a
	 *  panel's own effect, so a failing backend is asked once and not for ever. */
	load(): Promise<void> {
		if (this.status === 'ready') return Promise.resolve();
		return (this.#loading ??= this.#read().finally(() => {
			this.#loading = null;
		}));
	}

	async #read(): Promise<void> {
		this.status = 'loading';
		this.loadError = null;
		try {
			// Only kinds this build can draw, shape and place: its table is pinned to the engine's, so this drops nothing it ships.
			this.catalogue = (await sfxCatalogue()).filter((p) => (SFX_KINDS as readonly string[]).includes(p.kind));
			this.status = 'ready';
			if (this.kind === null && this.catalogue.length > 0) this.select(this.catalogue[0].kind, { audition: false });
		} catch (e) {
			this.status = 'error';
			this.loadError = failure("Couldn't load the sound effects", e);
		}
	}

	// ---- the pick and its shape

	/** Pick a preset: its defaults become the draft (the gain is kept — it is how loud the user wants effects, not part of a
	 *  sound), and it is auditioned. */
	select(kind: SfxKind, o: { audition?: boolean } = {}): void {
		if (this.kind !== kind || !this.spec) {
			this.kind = kind;
			this.spec = defaultSfxSpec(kind);
		}
		if (o.audition ?? true) void this.audition();
	}

	/** Write `patch` over the draft, and play the result: a slider has been let go, so hear what it did. */
	shape(patch: Partial<SfxSpec>, o: { audition?: boolean } = {}): void {
		if (!this.spec) return;
		this.spec = patchSpec(this.spec, patch);
		if (o.audition ?? true) void this.audition();
	}

	/** A new seed: the same sound with different details. */
	variation(): void {
		if (this.spec) this.shape({ seed: nextSeed(this.spec.seed) });
	}

	setGain(db: number): void {
		this.gainDb = db;
	}

	// ---- audition

	/** Render `spec` and play it, as `owner`. Anything playing is replaced; a render that a newer press overtook is dropped. */
	async auditionSpec(spec: SfxSpecInput, owner: AuditionOwner): Promise<void> {
		// Before anything is awaited: the press that asked is the gesture a webview lets the audio start from.
		this.prime();
		const ticket = ++this.#ticket;
		this.owner = owner;
		this.rendering = true;
		this.note = null;
		if (this.#noteTimer) clearTimeout(this.#noteTimer);
		try {
			const bytes = await previewSoundEffect(spec);
			if (ticket !== this.#ticket) return;
			if (!bytes) {
				this.#player.stop();
				this.note = AUDITION_UNAVAILABLE;
				this.#noteFor = owner;
				if (this.#noteTimer) clearTimeout(this.#noteTimer);
				this.#noteTimer = setTimeout(() => {
					this.note = null;
					this.#noteFor = null;
				}, NOTE_MS);
				return;
			}
			await this.#player.play(bytes);
		} catch (e) {
			if (ticket === this.#ticket && !isSfxCancelled(e)) toast.error(failure("Couldn't play the preview", e));
		} finally {
			if (ticket === this.#ticket) {
				this.rendering = false;
				if (!this.playing) this.owner = null;
			}
		}
	}

	audition(): Promise<void> {
		return this.spec ? this.auditionSpec(this.spec, 'panel') : Promise.resolve();
	}

	/** Get the audio output ready from a press (see `Auditioner.prime`); the Inspector calls it before its own await. */
	prime(): void {
		this.#player.prime();
	}

	/** What the last audition by `owner` has to say (the harness cannot make a sound), or null. */
	noteOf(owner: AuditionOwner): string | null {
		return this.#noteFor === owner ? this.note : null;
	}

	/** Whether `owner`'s sound is rendering or playing. */
	auditioning(owner: AuditionOwner): boolean {
		return this.owner === owner && (this.rendering || this.playing);
	}

	/** The Audition button: stop what `owner` has going, else play `spec`. */
	toggleAudition(spec: SfxSpecInput, owner: AuditionOwner): void {
		if (this.auditioning(owner)) this.stop();
		else void this.auditionSpec(spec, owner);
	}

	stop(): void {
		this.#ticket++;
		this.#player.stop();
		this.rendering = false;
		this.owner = null;
	}

	// ---- adding

	/** Put `placements` on the timeline as one edit and say so. False when nothing was added (a refusal, which is said, or a
	 *  project replaced meanwhile, which is not). */
	async add(placements: SfxPlacement[]): Promise<boolean> {
		if (placements.length === 0) return false;
		this.stop();
		try {
			const done = await editor.addSoundEffects(placements);
			const only = placements.length === 1 ? this.catalogue.find((p) => p.kind === placements[0].kind)?.label : undefined;
			toast.success(addedLine(done.clips.length, only), { action: { label: 'Undo', onClick: () => void editor.undo() } });
			return true;
		} catch (e) {
			if (isSfxCancelled(e)) return false;
			if (e instanceof PreviewReadOnly) report(e);
			else toast.error(failure(placements.length === 1 ? "Couldn't add the sound effect" : "Couldn't add the sound effects", e));
			return false;
		}
	}

	/** The draft, with its hit on the playhead (or `at`) and, when the drop names one, on that track. */
	addDraft(at: number = ui.time, trackId?: string): Promise<boolean> {
		return this.spec ? this.add([placementOf(this.spec, at, this.gainDb, trackId)]) : Promise.resolve(false);
	}

	/** The preset `kind`, picked, with its hit on `at` (the playhead): the grid's double-click and Enter. A kind that was not the
	 *  pick comes at its defaults; the one that was comes as it has been shaped. */
	async addKind(kind: SfxKind, at: number = ui.time): Promise<boolean> {
		this.select(kind, { audition: false });
		return this.addDraft(at);
	}

	/** What a drag of the selected sound carries. */
	dragPayload() {
		return this.spec ? dragOf(this.spec, this.gainDb, this.preset?.label ?? this.spec.kind) : null;
	}

	/** Dropped on the timeline: the sound placed with its hit at `at` on `trackId`. */
	dropOn(trackId: string, at: number): Promise<boolean> {
		return this.addDraft(at, trackId);
	}

	// ---- auto sound design

	async find(): Promise<void> {
		// A proposal being previewed is not the cut moments are read from.
		if (this.finding || editor.previewingStaged) return;
		const ticket = ++this.#findTicket;
		this.finding = true;
		this.findError = null;
		this.lastAdded = null;
		try {
			const stamp = this.stamp;
			const list = await suggestSoundEffects(suggestRequest(this.density, this.autoCategory));
			if (ticket !== this.#findTicket) return;
			this.found = list;
			this.foundStamp = stamp;
			this.picks = pickAll(list.length);
		} catch (e) {
			if (ticket !== this.#findTicket || isSfxCancelled(e)) return;
			this.findError = tidyReason(e instanceof Error ? e.message : String(e));
			toast.error(failure("Couldn't find moments for sound effects", e));
		} finally {
			if (ticket === this.#findTicket) this.finding = false;
		}
	}

	toggle(index: number): void {
		this.picks = togglePick(this.picks, index);
	}

	pickAll(): void {
		this.picks = pickAll(this.found?.length ?? 0);
	}

	pickNone(): void {
		this.picks = pickNone();
	}

	get pickedCount(): number {
		return this.picks.size;
	}

	/** Add the ticked moments as one edit. The list was made on the cut as it was, so it goes; the notice stays. */
	async addPicked(): Promise<boolean> {
		if (!this.found || this.stale) return false;
		const placements = pickedPlacements(this.found, this.picks);
		if (!(await this.add(placements))) return false;
		this.lastAdded = placements.length;
		this.found = null;
		this.picks = pickNone();
		return true;
	}

	/** The `sfx.add` key and menu entry: add the picked sound at the playhead, waiting for the catalogue if it is still being read
	 *  (or reading it again if that failed), and saying so if there is no sound to add. */
	async addFromShortcut(): Promise<void> {
		await this.load();
		if (!this.spec) {
			toast.error(this.loadError ?? "Couldn't load the sound effects");
			return;
		}
		await this.addDraft();
	}

	/** Seek the playhead to a moment. */
	jump(at: number): void {
		ui.seek(at);
	}
}

export const sfxPanel = new SfxPanelState();
