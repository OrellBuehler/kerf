import { describe, expect, mock, test } from 'bun:test';
import './test-runes';

// svelte-sonner cannot load under bun; the wrapper only needs something that records what it was
// asked to show.
const shown: { type: string; message: string; opts?: Record<string, unknown> }[] = [];
const sonner = Object.assign((message: string, opts?: Record<string, unknown>) => shown.push({ type: 'default', message, opts }), {
	success: (message: string, opts?: Record<string, unknown>) => shown.push({ type: 'success', message, opts }),
	error: (message: string, opts?: Record<string, unknown>) => shown.push({ type: 'error', message, opts }),
	warning: (message: string, opts?: Record<string, unknown>) => shown.push({ type: 'warning', message, opts }),
	info: (message: string, opts?: Record<string, unknown>) => shown.push({ type: 'info', message, opts }),
	dismiss: () => {}
});
mock.module('svelte-sonner', () => ({ toast: sonner }));

// The query keeps this the real module: other test files `mock.module('./notifications.svelte')`,
// and a mock outlives its file in bun.
const { notifications, toast } = (await import('./notifications.svelte?actual' as string)) as typeof import('./notifications.svelte');

describe('the notification log keeps the reason', () => {
	test('a toast’s description is recorded with its line', () => {
		notifications.clear();
		toast.error("Couldn't save the theme", { description: 'permission denied (os error 13)' });
		expect(notifications.items[0]).toMatchObject({
			kind: 'error',
			text: "Couldn't save the theme",
			description: 'permission denied (os error 13)',
			read: false
		});
		expect(shown.at(-1)).toMatchObject({ type: 'error', message: "Couldn't save the theme" });
	});

	test('a toast with no description records none', () => {
		notifications.clear();
		toast.success('Saved my-cut.kerf');
		expect('description' in notifications.items[0]).toBe(false);
	});

	test('a description that is not text is not kept, and does not break the toast', () => {
		notifications.clear();
		toast.info('Kerf 0.99.0', { description: (() => {}) as never });
		expect(notifications.items[0].description).toBeUndefined();
		expect(shown.at(-1)).toMatchObject({ type: 'info' });
	});

	test('every kind is recorded under its own name, newest first', () => {
		notifications.clear();
		toast('a');
		toast.success('b');
		toast.error('c');
		toast.warning('d');
		toast.info('e');
		expect(notifications.items.map((i) => i.kind)).toEqual(['info', 'warning', 'error', 'success', 'note']);
	});

	test('only a failure that is unread makes the badge shout', () => {
		notifications.clear();
		toast.success('fine');
		expect(notifications.unreadProblem).toBe(false);
		toast.warning('hm');
		expect(notifications.unreadProblem).toBe(true);
		notifications.markAllRead();
		expect(notifications.unreadProblem).toBe(false);
	});

	test('errors and warnings linger on screen longer than the default', () => {
		toast.error('slow');
		expect((shown.at(-1)!.opts as { duration: number }).duration).toBeGreaterThan(8_000);
		toast.success('quick');
		expect(shown.at(-1)!.opts?.duration).toBeUndefined();
	});
});
