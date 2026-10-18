import { describe, expect, test } from 'bun:test';
import { barColor, barTip, harnessRenderBar, spansIn } from './render-bar';
import type { BarSpan, Clip, Timeline, Track } from './types';

const clip = (start: number, len: number, speed = 1): Clip =>
	({ id: `c${start}`, asset_id: 'a', source_in: 0, source_out: len * Math.abs(speed), timeline_start: start, speed }) as Clip;
const track = (kind: 'video' | 'audio', clips: Clip[], muted = false): Track =>
	({ id: kind, name: kind, kind, clips, muted }) as Track;

describe('render bar', () => {
	test('each state has its themed colour', () => {
		expect(barColor('gpu')).toBe('var(--success)');
		expect(barColor('stream')).toBe('var(--warning)');
		expect(barColor('stall')).toBe('var(--danger)');
	});

	test('the tooltip says who plays the span, where and why', () => {
		const span: BarSpan = { start: 1, end: 2, state: 'stream', kind: 'mask', reason: 'a mask on a rotated layer' };
		expect(barTip(span, (t) => `${t}s`)).toBe('FFmpeg stream 1s–2s: a mask on a rotated layer');
	});

	test('only the spans in view are drawn', () => {
		const spans: BarSpan[] = [0, 1, 2].map((i) => ({ start: i, end: i + 1, state: 'gpu', kind: 'gpu', reason: '' }));
		expect(spansIn(spans, 1.5, 1.7).map((s) => s.start)).toEqual([1]);
		expect(spansIn(spans, 1, 2).map((s) => s.start)).toEqual([1]);
	});

	test('the harness stalls the reversed clips of the video tracks it plays, merged', () => {
		const timeline = {
			tracks: [
				track('video', [clip(0, 1), clip(1, 1, -1), clip(2, 1, -2)]),
				track('video', [clip(5, 1, -1)], true),
				track('audio', [clip(7, 1, -1)])
			]
		} as Timeline;
		const bar = harnessRenderBar(timeline);
		expect(bar.gpu_preview).toBe(false);
		expect(bar.spans.map((s) => [s.start, s.end, s.state, s.kind])).toEqual([[1, 3, 'stall', 'reversed']]);
	});
});
