import { describe, expect, test } from 'bun:test';
import { firstRunSteps } from './first-run';

const states = (assets: number, clips: number) => firstRunSteps({ assets, clips }).map((s) => s.state);

describe('the first-run steps', () => {
	test('an empty project starts at the import', () => {
		expect(states(0, 0)).toEqual(['current', 'todo', 'todo']);
	});

	test('with media in the library the cut is next', () => {
		expect(states(3, 0)).toEqual(['done', 'current', 'todo']);
	});

	test('with a clip on the timeline the export is what is left', () => {
		expect(states(3, 2)).toEqual(['done', 'done', 'current']);
	});

	test('it is three steps in the order import, cut, export', () => {
		expect(firstRunSteps({ assets: 0, clips: 0 }).map((s) => s.id)).toEqual(['import', 'cut', 'export']);
	});

	test('a step names its key as the user’s keymap prints it, and says nothing when there is none', () => {
		const [imp, , exp] = firstRunSteps({ assets: 0, clips: 0, importKeys: 'Ctrl+I', exportKeys: 'Ctrl+E' });
		expect(imp.body).toBe('Drop files on the window, or choose them (Ctrl+I).');
		expect(exp.body).toBe('Render the cut to a file for the platform you are making it for (Ctrl+E).');
		const [bare] = firstRunSteps({ assets: 0, clips: 0 });
		expect(bare.body).toBe('Drop files on the window, or choose them.');
	});
});
