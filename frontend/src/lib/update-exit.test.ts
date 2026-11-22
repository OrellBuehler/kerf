import { describe, expect, test } from 'bun:test';
import { installAfterDownload } from './update-exit';

function steps(over: Partial<Parameters<typeof installAfterDownload>[0]> = {}) {
	const events: string[] = [];
	return {
		events,
		steps: {
			download: async () => void events.push('download'),
			install: async () => (events.push('install'), 'done'),
			announce: () => true,
			leaving: async () => void events.push('leaving'),
			staying: async () => void events.push('staying'),
			limit: 40,
			...over
		} as Parameters<typeof installAfterDownload>[0]
	};
}

describe('installing an update with the backend told', () => {
	test('says it is leaving after the download and before the install, and nothing after a success', async () => {
		const { events, steps: s } = steps();
		expect(await installAfterDownload(s)).toBe('done');
		expect(events).toEqual(['download', 'leaving', 'install']);
	});

	test('says nothing at all when no backend is up, however the install ends', async () => {
		const ok = steps({ announce: () => false });
		await installAfterDownload(ok.steps);
		expect(ok.events).toEqual(['download', 'install']);
		const failure = new Error('the installer could not run');
		const bad = steps({
			announce: () => false,
			install: async () => {
				bad.events.push('install');
				throw failure;
			}
		});
		await expect(installAfterDownload(bad.steps)).rejects.toBe(failure);
		expect(bad.events).toEqual(['download', 'install']);
	});

	test('asks whether a backend is up after the download, not before it', async () => {
		let up = false;
		const { events, steps: s } = steps({
			download: async () => {
				up = true; // a backend came up while the download ran
			},
			announce: () => up
		});
		await installAfterDownload(s);
		expect(events).toEqual(['leaving', 'install']);
	});

	test('an install that fails says it is staying, and the failure is still the caller\'s', async () => {
		const failure = new Error('the installer could not run');
		const held = steps({
			install: async () => {
				held.events.push('install');
				throw failure;
			}
		});
		await expect(installAfterDownload(held.steps)).rejects.toBe(failure);
		expect(held.events).toEqual(['download', 'leaving', 'install', 'staying']);
	});

	test('a download that fails says nothing: nothing was announced', async () => {
		const failure = new Error('the server said no');
		const held = steps({
			download: async () => {
				throw failure;
			}
		});
		await expect(installAfterDownload(held.steps)).rejects.toBe(failure);
		expect(held.events).toEqual([]);
	});

	test('a backend that cannot be told is no reason not to install, or to hide why the install failed', async () => {
		const broken = async () => {
			throw new Error('no such command');
		};
		const ok = steps({ leaving: broken, staying: broken });
		expect(await installAfterDownload(ok.steps)).toBe('done');
		expect(ok.events).toEqual(['download', 'install']);
		const failure = new Error('disk full');
		const bad = steps({
			leaving: broken,
			staying: broken,
			install: async () => {
				throw failure;
			}
		});
		await expect(installAfterDownload(bad.steps)).rejects.toBe(failure);
	});

	test('a backend that does not answer holds neither the install nor its error back for more than the limit', async () => {
		const never = () => new Promise<void>(() => {});
		const started = performance.now();
		const ok = steps({ leaving: never, staying: never });
		expect(await installAfterDownload(ok.steps)).toBe('done');
		expect(ok.events).toEqual(['download', 'install']);
		const failure = new Error('the installer could not run');
		const bad = steps({
			leaving: never,
			staying: never,
			install: async () => {
				throw failure;
			}
		});
		await expect(installAfterDownload(bad.steps)).rejects.toBe(failure);
		// Two limits for the failed one (leaving, then staying), one for the other: well under a second at 40 ms each.
		expect(performance.now() - started).toBeLessThan(1000);
	});

	test('the install is waited for before it is said to have failed or succeeded (the process may end inside it)', async () => {
		let finish: () => void = () => {};
		const held = steps({ install: () => new Promise<string>((resolve) => (finish = () => resolve('done'))) });
		const running = installAfterDownload(held.steps);
		await new Promise((r) => setTimeout(r, 5));
		expect(held.events).toEqual(['download', 'leaving']);
		finish();
		expect(await running).toBe('done');
		expect(held.events).toEqual(['download', 'leaving']);
	});
});
