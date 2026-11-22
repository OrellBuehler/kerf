import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// A second press of Install while the first download runs must not start another over it: the
// dialog is locked during a download, but the state machine is what the title bar chip and an
// agent-free path can reach too.

const real = { ...(await import('./api')) };
let installs = 0;
let finish: () => void = () => {};
let gpuUp: (() => boolean | Promise<boolean>) | undefined;
let statusAsked = 0;

mock.module('./api', () => ({
	...real,
	// The backend's status as the page reads it: up, but only once it is asked for again.
	gpuPreviewStatus: async () => {
		statusAsked++;
		return { enabled: true, supported: true, technique: 'child', ready: true, overlays: false, adapter: 'a gpu', software: false, reason: null };
	},
	installUpdate: async (_progress: unknown, up?: () => boolean | Promise<boolean>) => {
		gpuUp = up;
		installs++;
		await new Promise<void>((resolve) => (finish = resolve));
	}
}));
afterAll(() => {
	mock.module('./api', () => real);
});

const { updater } = await import('./updater.svelte');

describe('Updater.install', () => {
	test('is one download at a time', async () => {
		updater.update = { version: '0.99.0' } as never;
		const first = updater.install();
		expect(updater.phase).toBe('downloading');
		await updater.install();
		await updater.install();
		expect(installs).toBe(1);
		finish();
		await first;
		expect(updater.phase).toBe('ready');
	});

	test('with nothing on offer it does nothing', async () => {
		updater.update = null;
		updater.phase = 'idle';
		await updater.install();
		expect(installs).toBe(1);
		expect(updater.phase).toBe('idle');
	});
});

describe('what the updater tells the install about the GPU preview', () => {
	test('asks the backend again before it says whether one is up', async () => {
		const { gpuPreview } = await import('./gpu-preview.svelte');
		// The page's copy is from before the backend came up: stale, and not up.
		gpuPreview.status = { enabled: true, supported: true, technique: 'child', ready: false, overlays: false, adapter: null, software: false, reason: null };
		updater.update = { version: '0.99.0' } as never;
		updater.phase = 'available';
		const running = updater.install();
		await new Promise((r) => setTimeout(r, 0));
		expect(gpuUp).toBeDefined();
		const before = statusAsked;
		expect(await gpuUp!()).toBe(true);
		expect(statusAsked).toBe(before + 1);
		finish();
		await running;
	});
});
