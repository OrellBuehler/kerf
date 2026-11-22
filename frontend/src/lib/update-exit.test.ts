import { describe, expect, test } from 'bun:test';
import { installLeaving } from './update-exit';

describe('installing an update with the backend told', () => {
	test('says it is leaving before the install starts, and nothing after a success', async () => {
		const events: string[] = [];
		const result = await installLeaving(
			async () => {
				events.push('install');
				return 'done';
			},
			async () => void events.push('leaving'),
			async () => void events.push('staying')
		);
		expect(result).toBe('done');
		expect(events).toEqual(['leaving', 'install']);
	});

	test('an install that fails says it is staying, and the failure is still the caller\'s', async () => {
		const events: string[] = [];
		const failure = new Error('the installer could not run');
		await expect(
			installLeaving(
				async () => {
					events.push('install');
					throw failure;
				},
				async () => void events.push('leaving'),
				async () => void events.push('staying')
			)
		).rejects.toBe(failure);
		expect(events).toEqual(['leaving', 'install', 'staying']);
	});

	test('a backend that cannot be told is no reason not to install, or to hide why the install failed', async () => {
		const events: string[] = [];
		const broken = async () => {
			throw new Error('no such command');
		};
		expect(await installLeaving(async () => (events.push('install'), 1), broken, broken)).toBe(1);
		const failure = new Error('disk full');
		await expect(
			installLeaving(
				async () => {
					throw failure;
				},
				broken,
				broken
			)
		).rejects.toBe(failure);
		expect(events).toEqual(['install']);
	});

	test('the install is waited for before it is said to have failed or succeeded (the process may end inside it)', async () => {
		const events: string[] = [];
		let finish: () => void = () => {};
		const running = installLeaving(
			() => new Promise<void>((resolve) => (finish = resolve)),
			async () => void events.push('leaving'),
			async () => void events.push('staying')
		);
		await Promise.resolve();
		await Promise.resolve();
		expect(events).toEqual(['leaving']);
		finish();
		await running;
		expect(events).toEqual(['leaving']);
	});
});
