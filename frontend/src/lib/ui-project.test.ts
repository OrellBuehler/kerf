import { afterEach, describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import './test-runes';

const { ui, REVEAL_MS } = await import('./editor-ui.svelte');

afterEach(() => {
	ui.markIn = null;
	ui.markOut = null;
	ui.reveal = null;
	ui.gap = null;
	ui.activeLane = null;
});

describe('another project does not inherit the last one’s marks', () => {
	test('the in and out marks, the lane the keyboard was on, a lit gap and a pending reveal are forgotten', () => {
		ui.markIn = 3;
		ui.markOut = 5;
		ui.activeLane = 'v1';
		ui.gap = { trackId: 'v1', from: 1, to: 2 };
		ui.revealClip('c1');
		ui.projectReplaced();
		expect([ui.markIn, ui.markOut, ui.activeLane, ui.gap, ui.reveal]).toEqual([null, null, null, null, null]);
	});
});

describe('a reveal is one shot', () => {
	test('asked for, it is there for the timeline that is listening', () => {
		ui.revealClip('c1');
		expect(ui.reveal?.clipId).toBe('c1');
	});

	test('the same clip asked for twice is a new request each time', () => {
		ui.revealClip('c1');
		const first = ui.reveal!.epoch;
		ui.revealClip('c1');
		expect(ui.reveal!.epoch).toBeGreaterThan(first);
	});

	test('one nobody took lapses, so a timeline that mounts later does not replay it', async () => {
		ui.revealClip('c1');
		await new Promise((r) => setTimeout(r, REVEAL_MS + 80));
		expect(ui.reveal).toBeNull();
	});

	test('a lapse that belongs to an earlier request does not take a newer one with it', async () => {
		ui.revealClip('c1');
		await new Promise((r) => setTimeout(r, REVEAL_MS - 100));
		ui.revealClip('c2');
		await new Promise((r) => setTimeout(r, 150));
		expect(ui.reveal?.clipId).toBe('c2');
	});

	test('the timeline clears the request it handles', () => {
		const timeline = readFileSync(join(import.meta.dir, 'components/editor/Timeline.svelte'), 'utf8');
		const effect = timeline.slice(timeline.indexOf('const r = ui.reveal;'));
		expect(effect.slice(0, 400)).toContain('ui.reveal = null;');
	});
});
