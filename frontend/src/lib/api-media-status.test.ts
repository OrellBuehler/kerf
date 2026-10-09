import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
	analyzeAsset,
	deleteProxy,
	getAnalysisStatuses,
	getProxyStatuses,
	getSettings,
	listAssets,
	onAnalysisStatus,
	onProxyProgress,
	rebuildProxy,
	setSettings,
	transcriptionStatus
} from './api';
import type { AnalysisStatus, ProxyStatus } from './types';

// The browser harness stands in for kerf-app's settings, proxy and analysis commands. What it
// answers must keep the shape the backend's does (`SettingsView`, `ProxyStatus`, `AnalysisStatus`),
// or the bin, the settings dialog and the quick edits would be built against behaviour the desktop
// app never shows.

const store = new Map<string, string>();
const g = globalThis as unknown as { localStorage?: Storage };
const original = g.localStorage;

beforeEach(() => {
	store.clear();
	g.localStorage = {
		getItem: (k: string) => store.get(k) ?? null,
		setItem: (k: string, v: string) => void store.set(k, String(v)),
		removeItem: (k: string) => void store.delete(k)
	} as unknown as Storage;
});

afterEach(() => {
	g.localStorage = original;
});

describe('settings (browser harness)', () => {
	test('the defaults: every analysis on, a 1280 px proxy, the Auto source', async () => {
		const view = await getSettings();
		expect(view.auto_analysis).toEqual({ enabled: true, silence: true, scenes: true, loudness: true, rhythm: true, transcript: true });
		expect(view.proxy_size).toBe(1280);
		expect(view.preview_source).toBe('auto');
		expect('transcribe' in view).toBe(false);
	});

	test('the old transcription flag becomes the transcript switch, and the new set wins over it', async () => {
		store.set('kerf.settings.transcribe', '0');
		const migrated = (await getSettings()).auto_analysis;
		expect(migrated.transcript).toBe(false);
		expect(migrated.silence && migrated.scenes && migrated.loudness && migrated.rhythm && migrated.enabled).toBe(true);
		// Once the user has chosen under the new key, the old one is moot.
		await setSettings({ auto_analysis: { ...migrated, transcript: true } });
		expect((await getSettings()).auto_analysis.transcript).toBe(true);
	});

	test('a patch is stored and read back; an unreadable value falls back to the default', async () => {
		await setSettings({ proxy_size: 720, preview_source: 'proxy_only' });
		const view = await getSettings();
		expect(view.proxy_size).toBe(720);
		expect(view.preview_source).toBe('proxy_only');
		await setSettings({ proxy_size: 0 });
		expect((await getSettings()).proxy_size).toBe(0);
		store.set('kerf.settings.proxySize', '900');
		store.set('kerf.settings.previewSource', 'somewhere');
		store.set('kerf.settings.autoAnalysis', '{not json');
		const view2 = await getSettings();
		expect(view2.proxy_size).toBe(1280);
		expect(view2.preview_source).toBe('auto');
		expect(view2.auto_analysis.enabled).toBe(true);
	});

	test('the transcription status follows the transcript switch', async () => {
		expect((await transcriptionStatus()).enabled).toBe(true);
		await setSettings({ auto_analysis: { enabled: true, silence: true, scenes: true, loudness: true, rhythm: true, transcript: false } });
		expect((await transcriptionStatus()).enabled).toBe(false);
	});
});

describe('proxies (browser harness)', () => {
	test('one status per asset: the first is building, the second ready', async () => {
		const [interview, broll] = await listAssets();
		const byId = new Map((await getProxyStatuses()).map((p) => [p.asset_id, p]));
		expect(byId.get(interview.id)).toMatchObject({ state: 'building', fraction: 0.42 });
		expect(byId.get(broll.id)).toMatchObject({ state: 'ready', width: 1280 });
		expect(byId.get(broll.id)?.bytes).toBeGreaterThan(0);
	});

	test('rebuild starts a build and tells listeners; delete stops it and says why there is none', async () => {
		const [, broll] = await listAssets();
		const seen: ProxyStatus[] = [];
		const stop = await onProxyProgress((s) => seen.push(s));
		const started = await rebuildProxy(broll.id);
		expect(started).toMatchObject({ state: 'building', fraction: 0 });
		expect(seen.at(-1)?.state).toBe('building');
		const gone = await deleteProxy(broll.id);
		expect(gone.state).toBe('off');
		expect(gone.reason).toContain('deleted');
		expect(seen.at(-1)?.state).toBe('off');
		stop();
		// Status reads back what the last event said.
		expect((await getProxyStatuses()).find((p) => p.asset_id === broll.id)?.state).toBe('off');
		await rebuildProxy(broll.id).then(() => deleteProxy(broll.id));
	});
});

describe('analysis (browser harness)', () => {
	const stateOf = (statuses: AnalysisStatus[], id: string, kind: string) =>
		statuses.find((s) => s.asset_id === id)?.kinds.find((k) => k.kind === kind)?.state;

	test('the status is one entry per kind, from what the sample analysis holds', async () => {
		const [interview, broll] = await listAssets();
		const statuses = await getAnalysisStatuses();
		for (const s of statuses) expect(s.kinds.map((k) => k.kind)).toEqual(['silence', 'scenes', 'loudness', 'rhythm', 'transcript']);
		expect(statuses.find((s) => s.asset_id === interview.id)?.kinds.every((k) => k.state === 'done')).toBe(true);
		// The b-roll has no speech: its transcript has not run, and there is no backend to run it.
		expect(stateOf(statuses, broll.id, 'silence')).toBe('not_run');
		expect(stateOf(statuses, broll.id, 'scenes')).toBe('done');
		expect(stateOf(statuses, broll.id, 'transcript')).toBe('off');
	});

	test('only the named steps run, announced as they go, and the others are left as they were', async () => {
		const [, broll] = await listAssets();
		const seen: AnalysisStatus[] = [];
		const stop = await onAnalysisStatus((s) => seen.push(s));
		const analysis = await analyzeAsset(broll.id, ['silence']);
		stop();
		expect(analysis.ran).toContain('silence');
		expect(analysis.scene_changes.length).toBeGreaterThan(0);
		expect(analysis.loudness).not.toBeNull();
		expect(seen.some((s) => s.kinds.find((k) => k.kind === 'silence')?.state === 'running')).toBe(true);
		expect(seen.at(-1)?.kinds.find((k) => k.kind === 'silence')?.state).toBe('done');
		// Nothing but silence was touched: a kind that had not run still has not.
		const after = await getAnalysisStatuses();
		expect(stateOf(after, broll.id, 'silence')).toBe('done');
		expect(stateOf(after, broll.id, 'transcript')).toBe('off');
	});

	test('a done kind stays done whatever the switches say', async () => {
		const [, broll] = await listAssets();
		const before = await getAnalysisStatuses();
		expect(stateOf(before, broll.id, 'loudness')).toBe('done');
		await setSettings({ auto_analysis: { enabled: false, silence: true, scenes: true, loudness: true, rhythm: true, transcript: true } });
		const off = await getAnalysisStatuses();
		// Done stays done whatever the switches say.
		expect(stateOf(off, broll.id, 'loudness')).toBe('done');
	});
});
