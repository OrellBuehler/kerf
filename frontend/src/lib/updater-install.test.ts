import { afterAll, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// A second press of Install while the first download runs must not start another over it: the
// dialog is locked during a download, but the state machine is what the title bar chip and an
// agent-free path can reach too.

const real = { ...(await import('./api')) };
let installs = 0;
let finish: () => void = () => {};

mock.module('./api', () => ({
	...real,
	installUpdate: async () => {
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
