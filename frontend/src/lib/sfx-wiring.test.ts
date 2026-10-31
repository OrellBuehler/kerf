import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { buildMenus, flatten, type MenuState } from './menus';
import { ACTIONS, allowsRepeat } from './keymap';
import { PANELS } from './layout';

// How the Sound FX panel is wired into the components that have no test of their own: what it writes and when, the
// Inspector's section, the timeline's drop, the library's way in. Read off the source, as `svg-wiring.test.ts` does; the
// rules they call are tested where they live (`sfx-panel`, `sfx-audition`, `layout`, `workspaces`).

const read = (path: string) => readFileSync(join(import.meta.dir, path), 'utf8');
const panel = read('components/editor/SoundFx.svelte');
const shape = read('components/editor/SfxShape.svelte');
const switchBox = read('components/editor/Switch.svelte');
const auto = read('components/editor/SfxAuto.svelte');
const tile = read('components/editor/SfxTile.svelte');
const section = read('components/editor/SoundEffectSection.svelte');
const inspector = read('components/editor/Inspector.svelte');
const timeline = read('components/editor/Timeline.svelte');
const audioTab = read('components/editor/AudioTab.svelte');
const workspaceComponent = read('components/editor/Workspace.svelte');
const state = read('sfx-panel.svelte.ts');
const page = read('../routes/+page.svelte');

const calls = (src: string, name: string) => (src.match(new RegExp(`\\b${name}\\(`, 'g')) ?? []).length;

describe('the panel is a dock panel', () => {
	test('is mounted by the workspace under its id, with its own registry entry', () => {
		expect(workspaceComponent).toContain("import SoundFx from './SoundFx.svelte';");
		expect(workspaceComponent).toContain('sfx: SoundFx');
		expect(PANELS.sfx.title).toBe('Sound FX');
	});

	test('is in the Window menu, where a closed panel can be opened', () => {
		const s: MenuState = {
			saved: false,
			canUndo: false,
			canRedo: false,
			hasClips: true,
			exporting: false,
			hasSelection: false,
			rippleMode: false,
			snap: true,
			minimap: true,
			safeAreas: false,
			motionPath: true,
			hasFrame: true,
			tool: 'pointer',
			delivery: 'source',
			allHeight: 'medium',
			workspace: 'audio',
			openPanels: ['preview', 'timeline', 'mixer'],
			detachedPanels: [],
			desktop: true,
			previewing: false,
			recent: []
		};
		const window = buildMenus(s).find((m) => m.id === 'window')!;
		const entry = flatten(window.items).find((e) => e.kind === 'command' && e.label === 'Sound FX');
		expect(entry).toBeDefined();
		expect((entry as { command: unknown }).command).toEqual({ type: 'panel', panel: 'sfx' });
	});

	test('a closed one opens beside the preview, with the other panels the Audio workspace adds', () => {
		expect(read('workspace.svelte.ts')).toContain("sfx: 'right'");
	});
});

describe('the panel writes nothing until Add', () => {
	test('its sliders go to the draft, never to the cut', () => {
		expect(panel).toContain('sfxPanel.shape(patch)');
		expect(panel).not.toContain('updateSoundEffect');
		expect(panel).not.toContain('addSoundEffects');
		// The slider rows are the shared ones: a drag is one change on release, Escape drops it.
		expect(shape.match(/<SliderRow\b/g)?.length).toBeGreaterThanOrEqual(5);
		expect(shape).not.toContain('type="range"');
	});

	test('a slider released audits the draft; a drag in flight renders nothing', () => {
		expect(state).toContain('shape(patch: Partial<SfxSpec>');
		expect(state).toContain('void this.audition()');
		expect(state).toContain('previewSoundEffect(spec)');
	});

	test('adding goes through the one editor call, as one edit, and says what it did', () => {
		expect(calls(state, 'editor.addSoundEffects')).toBe(1);
		expect(state).toContain('toast.success(addedLine(');
		expect(state).toContain("label: 'Undo'");
	});

	test('add at the playhead lands the hit on it, and the keyboard and the double-click add the same way', () => {
		expect(state).toContain('addDraft(at: number = ui.time');
		expect(panel).toContain('sfxPanel.addDraft()');
		expect(panel).toContain('void sfxPanel.addKind(p.kind)');
		expect(panel).toContain("e.key === 'Enter'");
		expect(tile).toContain('ondblclick={onadd}');
	});

	test('the Add buttons are off for a proposal being previewed or an edit in flight, with the reason as their tooltip', () => {
		expect(panel).toContain('pending={editor.locked}');
		expect(auto).toContain('pending={why !== null || editor.locked}');
		expect(state).toContain("'Exit the proposal preview to edit'");
		expect(state).toContain('e instanceof PreviewReadOnly');
	});

	test('a failure is a toast, and the cancelled answer is nobody’s news', () => {
		expect(state).toContain("failure(\"Couldn't play the preview\"");
		expect(state).toContain("failure(\"Couldn't find moments for sound effects\"");
		expect(state).toContain('isSfxCancelled(e)');
	});
});

describe('auto sound design', () => {
	test('adds the ticked moments as ONE call, and only on the cut they were found on', () => {
		expect(calls(state, 'this.add')).toBeGreaterThanOrEqual(2);
		const addPicked = state.slice(state.indexOf('async addPicked()'));
		expect(addPicked.slice(0, 700)).toContain('pickedPlacements(this.found, this.picks)');
		expect(addPicked.slice(0, 700)).toContain('this.stale');
		expect(addPicked.slice(0, 700).match(/await this\.add\(/g)).toHaveLength(1);
	});

	test('a find is a read: no edit is made and a newer one overtakes an older', () => {
		const find = state.slice(state.indexOf('async find()'), state.indexOf('toggle(index'));
		expect(find).toContain('suggestSoundEffects(');
		expect(find).not.toMatch(/editor\.(add|update|apply)/);
		expect(find).toContain('ticket !== this.#findTicket');
	});

	test('says what it is doing and what went wrong, with the design system’s own states', () => {
		expect(auto).toContain('<ProgressBar');
		expect(auto).toContain('<EmptyState');
		expect(auto).toContain('<ChoiceChips');
		expect(auto).toContain('sfxPanel.jump(r.at)');
	});
});

describe('dragging a tile onto a lane', () => {
	test('is the shared pointer drag, which gives up on Escape, a cancelled pointer and a blur', () => {
		expect(panel).toContain("import { beginDrag } from '#lib/drag';");
		expect(panel).toContain('abandon()');
		expect(panel).not.toContain('addEventListener');
	});

	test('the timeline draws where it would land, only over an audio lane, and drops it as the same add with the lane named', () => {
		expect(timeline).toContain('ui.sfxDropTarget = onSfxDrop');
		expect(timeline).toContain("hit.t.kind !== 'audio'");
		expect(timeline).toContain('sfxPanel.dropOn(hit.t.id,');
		expect(state).toContain('addDraft(at, trackId)');
	});

	test('the click that ends a drag is not a pick', () => {
		expect(panel).toContain('dragged = true;');
		expect(panel).toContain('if (dragged) {');
	});
});

describe('the Inspector’s Sound effect section', () => {
	test('shows for a clip whose asset is a synthesized effect, with the panel’s controls', () => {
		expect(inspector).toContain('{#if asset?.sound_effect}');
		expect(inspector).toContain('<SoundEffectSection {asset} {clip} />');
		expect(section).toContain('<SfxShape');
	});

	test('a change is ONE updateSoundEffect, from the slider’s release (or the switch, or the new take)', () => {
		expect(calls(section, 'editor.updateSoundEffect')).toBe(1);
		expect(section).toContain('onchange={reshape}');
		expect(shape).toContain('onCommit=');
	});

	test('is off while an edit is in flight or a proposal is previewed', () => {
		expect(section).toContain('disabled={editor.locked}');
	});

	test('the level is the Volume section’s, not a second one', () => {
		expect(section).not.toContain('gain=');
	});
});

describe('the library and the keyboard', () => {
	test('the Audio tab opens or shows the panel', () => {
		expect(audioTab).toContain("workspace.show('sfx')");
		expect(audioTab).toContain('Sound effects');
	});

	test('the registry has a key for adding the picked sound, unbound until the user binds one, and the page runs it', () => {
		const action = ACTIONS.find((a) => a.id === 'sfx.add')!;
		expect(action.label).toBe('Add sound effect at playhead');
		expect(action.defaults).toEqual([]);
		expect(allowsRepeat('sfx.add')).toBe(false);
		expect(page).toContain("'sfx.add':");
		expect(panel).toContain("settings.shortcut('sfx.add')");
	});
});

describe('what a failing, a slow or a late thing may not do', () => {
	test('the panel reads the catalogue when it appears, not from an effect that would ask again each time its state moved', () => {
		expect(panel).toContain('onMount(() => {');
		expect(panel).toContain('void sfxPanel.load();');
		expect(panel).not.toContain('$effect(');
		expect(state).toContain('return (this.#loading ??=');
	});

	test('the key waits for the catalogue and says so when there is nothing to add', () => {
		expect(page).toContain("'sfx.add': () => void sfxPanel.addFromShortcut()");
		const key = state.slice(state.indexOf('async addFromShortcut()'));
		expect(key.slice(0, 400)).toContain('await this.load();');
		expect(key.slice(0, 400)).toContain('toast.error(');
	});

	test('the audio output is asked for from the press, before the render is awaited, and a late answer is not played for a clip that is gone', () => {
		const audition = state.slice(state.indexOf('async auditionSpec('), state.indexOf('audition(): Promise<void>'));
		expect(audition.indexOf('this.prime()')).toBeGreaterThan(-1);
		expect(audition.indexOf('this.prime()')).toBeLessThan(audition.indexOf('await previewSoundEffect'));
		const reshape = section.slice(section.indexOf('async function reshape('));
		expect(reshape.indexOf('sfxPanel.prime()')).toBeGreaterThan(-1);
		expect(reshape.indexOf('sfxPanel.prime()')).toBeLessThan(reshape.indexOf('await editor.updateSoundEffect'));
		expect(reshape).toContain('!gone && owner === who && !ui.playing');
		expect(section).toContain('onDestroy(() => (gone = true));');
	});

	test('a switch or a button that is waiting on a write keeps the keyboard focus a disabled one would drop', () => {
		expect(shape).toContain('<Switch checked={reverse} label="Reverse" text="Reverse" pending={disabled}');
		expect(switchBox).toContain("aria-disabled={pending ? 'true' : undefined}");
		expect(switchBox).toContain('if (pending) e.preventDefault();');
	});

	test('after the ticked moments are added the keyboard goes to Find moments, not nowhere', () => {
		expect(auto).toContain("section?.querySelector<HTMLElement>('[data-testid=\"sfx-find\"]')?.focus();");
		expect(auto).toContain('await sfxPanel.addPicked()');
	});

	test('moments are not found on a proposal being previewed', () => {
		expect(auto).toContain('editor.previewingStaged');
		expect(auto).toContain('pending={sfxPanel.finding || findWhy !== null}');
		expect(state).toContain('if (this.finding || editor.previewingStaged) return;');
	});

	test('a panel in a window of its own does not drag to the main window’s lanes', () => {
		expect(panel).toContain("popout.isDetached('sfx')");
		expect(panel).toContain('if (e.button !== 0 || cannotDrag) return;');
		expect(panel).toContain("cannotDrag ? '' : ' · drag onto an audio track'");
	});

	test('the click behind a drag’s release is not a pick, and the flag does not outlive it', () => {
		expect(panel).toContain('setTimeout(() => (dragged = false), CLICK_AFTER_DRAG_MS)');
		const abandon = panel.slice(panel.indexOf('abandon() {'));
		expect(abandon.slice(0, 80)).not.toContain('dragged');
	});

	test('the timeline refuses a red drop in words, and with ripple on lets an overlap push instead', () => {
		expect(timeline).toContain('dropGhostFor(s, at, others, editor.rippleMode)');
		expect(timeline).toContain('const refusal = dropRefusal(g);');
		expect(timeline).toContain('toast.error(refusal);');
		expect(timeline).toContain('sfxDropHit(time, {');
	});

	test('a sound of a kind this build does not know is a sentence, not an error', () => {
		expect(section).toContain('data-testid="sound-effect-unknown"');
		expect(section).toContain('knownPreset(spec.kind)');
		expect(inspector).toContain('shapeSummary(asset.sound_effect)');
	});
});
