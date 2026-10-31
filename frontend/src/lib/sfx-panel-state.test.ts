import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// The Sound FX panel's state over the browser harness's stand-in for the backend: how it reads the catalogue (once, and again
// only when asked), what the `sfx.add` key does while that is going on or has failed, and what finding moments refuses. The
// toasts stand in for what only a window shows.

const real = { ...(await import('./api')) };
const calls = { catalogue: 0, suggest: 0 };
let fail: Error | null = null;
let gate: Promise<void> | null = null;
let rows: Awaited<ReturnType<typeof real.sfxCatalogue>> | null = null;

mock.module('./api', () => ({
	...real,
	sfxCatalogue: async () => {
		calls.catalogue++;
		if (gate) await gate;
		if (fail) throw fail;
		return rows ?? real.sfxCatalogue();
	},
	suggestSoundEffects: async (...args: Parameters<typeof real.suggestSoundEffects>) => {
		calls.suggest++;
		return real.suggestSoundEffects(...args);
	}
}));
const shown: { kind: string; text: string }[] = [];
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	}),
	notifications: { record: () => {}, items: [] }
}));

afterAll(() => {
	mock.module('./api', () => real);
});

const { editor } = await import('./state.svelte');
const { sfxPanel } = await import('./sfx-panel.svelte');
const { revertTo } = real;

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.previewingStaged = false;
	calls.catalogue = 0;
	calls.suggest = 0;
	fail = null;
	gate = null;
	rows = null;
	shown.length = 0;
	sfxPanel.status = 'idle';
	sfxPanel.catalogue = [];
	sfxPanel.kind = null;
	sfxPanel.spec = null;
	sfxPanel.loadError = null;
	sfxPanel.found = null;
});

const fxClips = () => editor.timeline.tracks.filter((t) => t.name.startsWith('FX')).flatMap((t) => t.clips);

describe('reading the catalogue', () => {
	test('is one call, and the first sound is picked without a sound', async () => {
		await sfxPanel.load();
		expect(calls.catalogue).toBe(1);
		expect(sfxPanel.status).toBe('ready');
		expect(sfxPanel.kind).toBe('whoosh');
		expect(sfxPanel.spec?.kind).toBe('whoosh');
		await sfxPanel.load();
		expect(calls.catalogue).toBe(1);
	});

	test('a read that fails is one call and one answer: nothing asks again by itself', async () => {
		fail = new Error('invalid argument: the engine is gone');
		await sfxPanel.load();
		expect(calls.catalogue).toBe(1);
		expect(sfxPanel.status).toBe('error');
		expect(sfxPanel.loadError).toBe("Couldn't load the sound effects — The engine is gone");
		// Time passes, panels are built and thrown away: still the one call.
		await new Promise((r) => setTimeout(r, 30));
		expect(calls.catalogue).toBe(1);
		expect(shown).toEqual([]);
	});

	test('Try again is the call that reads it again, and then it is read', async () => {
		fail = new Error('boom');
		await sfxPanel.load();
		fail = null;
		await sfxPanel.load();
		expect(calls.catalogue).toBe(2);
		expect(sfxPanel.status).toBe('ready');
		expect(sfxPanel.loadError).toBeNull();
		expect(sfxPanel.catalogue.length).toBeGreaterThan(0);
	});

	test('two askers while it is being read share the one read', async () => {
		let open!: () => void;
		gate = new Promise<void>((r) => (open = r));
		const both = Promise.all([sfxPanel.load(), sfxPanel.load()]);
		expect(sfxPanel.status).toBe('loading');
		open();
		await both;
		expect(calls.catalogue).toBe(1);
		expect(sfxPanel.status).toBe('ready');
	});

	test('a kind this build cannot draw or shape is left out', async () => {
		const all = await real.sfxCatalogue();
		rows = [...all, { ...all[0], kind: 'laser_zap' as never, label: 'Laser zap' }];
		await sfxPanel.load();
		expect(sfxPanel.catalogue.map((p) => p.kind)).toEqual(all.map((p) => p.kind));
	});
});

describe('the sfx.add key', () => {
	test('waits for a catalogue that is still being read, and then adds the picked sound at the playhead', async () => {
		let open!: () => void;
		gate = new Promise<void>((r) => (open = r));
		const before = fxClips().length;
		const done = sfxPanel.addFromShortcut();
		await Promise.resolve();
		expect(fxClips().length).toBe(before);
		open();
		await done;
		expect(calls.catalogue).toBe(1);
		expect(fxClips().length).toBe(before + 1);
		expect(shown.some((n) => n.kind === 'error')).toBe(false);
	});

	test('after a failed read it reads again, and says so when that fails too — never nothing', async () => {
		fail = new Error('boom');
		await sfxPanel.load();
		const before = fxClips().length;
		await sfxPanel.addFromShortcut();
		expect(calls.catalogue).toBe(2);
		expect(fxClips().length).toBe(before);
		expect(shown).toEqual([{ kind: 'error', text: "Couldn't load the sound effects — Boom" }]);
	});

	test('after a failed read that the key itself recovers, it adds', async () => {
		fail = new Error('boom');
		await sfxPanel.load();
		fail = null;
		const before = fxClips().length;
		await sfxPanel.addFromShortcut();
		expect(fxClips().length).toBe(before + 1);
	});
});

describe('finding moments', () => {
	test('asks the engine, and keeps the answer', async () => {
		await sfxPanel.find();
		expect(calls.suggest).toBe(1);
		expect(sfxPanel.found).not.toBeNull();
	});

	test('is not asked while a proposal is being previewed: that is not the cut moments are read from', async () => {
		editor.previewingStaged = true;
		await sfxPanel.find();
		expect(calls.suggest).toBe(0);
		expect(sfxPanel.found).toBeNull();
		expect(sfxPanel.finding).toBe(false);
	});
});
