import { beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load: record what is toasted instead.
const toasts: string[] = [];
const note = (message: string) => void toasts.push(message);
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note, { success: note, info: note, warning: note, error: note, dismiss: () => {} }),
	notifications: {}
}));

const real = { ...(await import('./api')) };
let writes: unknown[] = [];
mock.module('./api', () => ({
	...real,
	setDeliveryFormat: async (...args: Parameters<typeof real.setDeliveryFormat>) => {
		writes.push(args[0]);
		return real.setDeliveryFormat(...args);
	}
}));

const { editor } = await import('./state.svelte');
const { setDeliveryFit, setDeliveryFps, setDeliveryPreset } = await import('./menu-commands');

beforeEach(async () => {
	await editor.setDeliveryFormat(null);
	writes = [];
	toasts.length = 0;
});

describe('choosing the project frame', () => {
	test('a shape writes one edit and says so once', async () => {
		await setDeliveryPreset('vertical');
		expect(writes).toHaveLength(1);
		expect(editor.timeline.format).toMatchObject({ width: 1080, height: 1920, fit: 'cover' });
		expect(toasts).toHaveLength(1);
		expect(toasts[0]).toContain('Project frame: 9:16');
	});

	test('the shape it already is writes nothing: no edit, no notice', async () => {
		await setDeliveryPreset('vertical');
		writes = [];
		toasts.length = 0;
		await setDeliveryPreset('vertical');
		expect(writes).toHaveLength(0);
		expect(toasts).toHaveLength(0);
	});

	test('nor does Footage while the project already follows the footage', async () => {
		await setDeliveryPreset('source');
		expect(writes).toHaveLength(0);
		expect(toasts).toHaveLength(0);
	});

	test('picking the shape again does not put a fit the user chose back to the preset’s', async () => {
		await setDeliveryPreset('vertical');
		await setDeliveryFit('blur');
		writes = [];
		await setDeliveryPreset('vertical');
		expect(writes).toHaveLength(0);
		expect(editor.timeline.format?.fit).toBe('blur');
	});

	test('a run of arrows through the chips is a run of writes only when each lands on another shape', async () => {
		for (const id of ['landscape', 'vertical', 'square']) await setDeliveryPreset(id);
		expect(writes).toHaveLength(3);
	});

	test('the same fit and the same rate write nothing either', async () => {
		await setDeliveryPreset('vertical');
		await setDeliveryFps(50);
		writes = [];
		toasts.length = 0;
		await setDeliveryFit('cover');
		await setDeliveryFps(50);
		expect(writes).toHaveLength(0);
		expect(toasts).toHaveLength(0);
	});
});
