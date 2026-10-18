import { describe, expect, test } from 'bun:test';
import { jobLine, type JobInputs } from './job-line';

const none: JobInputs = { smartCrop: null, cover: null, model: null, importing: null };

describe('the job in front', () => {
	test('nothing running says nothing', () => {
		expect(jobLine(none)).toBeNull();
	});

	test('each job names itself, with its percentage when it has one', () => {
		expect(jobLine({ ...none, smartCrop: { fraction: 0.42, stopping: false } })).toMatchObject({ id: 'smart-crop', label: 'Framing the shots 42%' });
		expect(jobLine({ ...none, smartCrop: { fraction: null, stopping: false } })?.label).toBe('Framing the shots');
		expect(jobLine({ ...none, cover: { stopping: false } })).toMatchObject({ id: 'cover', label: 'Saving the cover frame…' });
		expect(jobLine({ ...none, model: { name: 'base', fraction: 0.5, stopping: false } })?.label).toBe('Downloading the base speech model 50%');
		expect(jobLine({ ...none, importing: { fraction: null, stopping: false } })?.label).toBe('Importing media…');
		expect(jobLine({ ...none, importing: { fraction: 0.25, stopping: false } })?.label).toBe('Stitching the 360 pair 25%');
	});

	test('a fraction is held to 0..100', () => {
		expect(jobLine({ ...none, smartCrop: { fraction: 3, stopping: false } })?.label).toBe('Framing the shots 100%');
		expect(jobLine({ ...none, smartCrop: { fraction: -1, stopping: false } })?.label).toBe('Framing the shots 0%');
	});

	test('the one a person waits on comes first, and a stop in flight is carried', () => {
		const all: JobInputs = {
			smartCrop: { fraction: 0.1, stopping: true },
			cover: { stopping: false },
			model: { name: 'tiny', fraction: 0, stopping: false },
			importing: { fraction: null, stopping: false }
		};
		expect(jobLine(all)).toMatchObject({ id: 'smart-crop', stopping: true });
		expect(jobLine({ ...all, smartCrop: null })?.id).toBe('cover');
		expect(jobLine({ ...all, smartCrop: null, cover: null })?.id).toBe('model');
		expect(jobLine({ ...all, smartCrop: null, cover: null, model: null })?.id).toBe('import');
	});

	test('every Stop says what it does to the work', () => {
		for (const j of [
			jobLine({ ...none, smartCrop: { fraction: null, stopping: false } }),
			jobLine({ ...none, cover: { stopping: false } }),
			jobLine({ ...none, model: { name: 'x', fraction: null, stopping: false } }),
			jobLine({ ...none, importing: { fraction: null, stopping: false } })
		])
			expect(j?.tip.startsWith('Stop ')).toBe(true);
	});
});
