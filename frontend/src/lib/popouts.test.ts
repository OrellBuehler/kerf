import { describe, expect, test } from 'bun:test';
import {
	DETACH_MAX,
	DETACH_MIN_HEIGHT,
	DETACH_MIN_WIDTH,
	LabelBook,
	describeFailure,
	detachBlocked,
	detachSize,
	mirrorRoot,
	positionCorrection,
	windowTitle
} from './popouts';
import { PANEL_IDS, PANELS } from './layout';

describe('the size of a detached window', () => {
	test('is what the panel had, floored at what it needs', () => {
		expect(detachSize('timeline', 1400, 304)).toEqual({ width: 1400, height: 304 });
		// A narrow rail of a panel still gets a window worth the name.
		expect(detachSize('library', 40, 490)).toEqual({ width: DETACH_MIN_WIDTH, height: 490 });
		expect(detachSize('preview', 800, 100)).toEqual({ width: 800, height: DETACH_MIN_HEIGHT });
	});

	test('never undercuts the panel\'s own minimum', () => {
		for (const id of PANEL_IDS) {
			const s = detachSize(id, 1, 1);
			expect(s.width).toBeGreaterThanOrEqual(PANELS[id].minimumWidth ?? 0);
			expect(s.height).toBeGreaterThanOrEqual(PANELS[id].minimumHeight ?? 0);
		}
	});

	test('is bounded, and a size that is not a number is the floor', () => {
		expect(detachSize('timeline', 90000, 90000)).toEqual({ width: DETACH_MAX, height: DETACH_MAX });
		expect(detachSize('inspector', NaN, Infinity).width).toBe(DETACH_MIN_WIDTH);
		expect(Number.isFinite(detachSize('inspector', NaN, NaN).height)).toBe(true);
	});
});

describe('a detached window\'s title', () => {
	test('names the panels in it', () => {
		expect(windowTitle(['library'])).toBe('Library — Kerf');
		expect(windowTitle(['inspector', 'agent'])).toBe('Inspector + Agent — Kerf');
		expect(windowTitle([])).toBe('Kerf');
	});
});

describe('which panels may be detached', () => {
	const where = { library: 'grid', preview: 'grid', timeline: 'grid', inspector: 'grid', agent: 'grid' } as const;

	test('any panel of the editor window while another stays', () => {
		for (const id of ['library', 'preview', 'timeline', 'inspector', 'agent'] as const) {
			expect(detachBlocked(id, where), id).toBeNull();
		}
	});

	test('a panel that is not open has to be opened first', () => {
		expect(detachBlocked('mixer', where)).toMatch(/not open/);
	});

	test('the last panel of the editor window stays', () => {
		expect(detachBlocked('timeline', { timeline: 'grid' })).toMatch(/at least one panel/);
		// Panels in windows of their own do not count as the editor window's.
		expect(detachBlocked('timeline', { timeline: 'grid', preview: 'popout' })).toMatch(/at least one panel/);
		expect(detachBlocked('timeline', { timeline: 'grid', preview: 'grid' })).toBeNull();
	});

	test('a panel already in a window may always be given back', () => {
		expect(detachBlocked('preview', { timeline: 'grid', preview: 'popout' })).toBeNull();
		expect(detachBlocked('preview', { preview: 'popout' })).toBeNull();
	});
});

describe('every failure dockview can report is a sentence', () => {
	test('known and unknown', () => {
		for (const r of ['blocked', 'url-refused', 'unscriptable', 'closed', 'something new']) {
			expect(describeFailure(r).length).toBeGreaterThan(10);
		}
		expect(describeFailure('blocked')).not.toBe(describeFailure('closed'));
	});
});

describe('LabelBook', () => {
	test('hands labels out in the order they were announced', () => {
		const b = new LabelBook();
		b.expect('popout-0');
		b.expect('popout-1');
		expect(b.size).toBe(2);
		expect(b.claim()).toBe('popout-0');
		expect(b.claim()).toBe('popout-1');
		expect(b.claim()).toBeNull();
	});

	test('a window that never opened is dropped, and does not take the next one\'s label', () => {
		const b = new LabelBook();
		b.expect('popout-0');
		b.expect('popout-1');
		expect(b.drop('popout-0')).toBe(true);
		expect(b.drop('popout-0')).toBe(false);
		expect(b.claim()).toBe('popout-1');
	});

	test('an announcement is not taken twice, and the book can be emptied', () => {
		const b = new LabelBook();
		b.expect('popout-0');
		b.expect('popout-0');
		expect(b.size).toBe(1);
		b.expect('popout-1');
		expect(b.clear()).toEqual(['popout-0', 'popout-1']);
		expect(b.size).toBe(0);
	});
});

describe('mirroring the theme onto a window', () => {
	const root = (className: string, cssText: string, lang = 'en') => ({ className, lang, style: { cssText } });

	test('copies the class, the inline properties and the language', () => {
		const from = root('dark', '--surface-app: #123456; color-scheme: dark;');
		const to = root('', '', '');
		expect(mirrorRoot(from, to)).toBe(true);
		expect(to).toEqual(from);
	});

	test('says when there was nothing to do', () => {
		const from = root('light', '--a: 1;');
		const to = root('light', '--a: 1;');
		expect(mirrorRoot(from, to)).toBe(false);
	});

	test('follows a later change', () => {
		const from = root('dark', '--a: 1;');
		const to = root('dark', '--a: 1;');
		from.className = 'light';
		from.style.cssText = '--a: 2;';
		expect(mirrorRoot(from, to)).toBe(true);
		expect(to.className).toBe('light');
		expect(to.style.cssText).toBe('--a: 2;');
	});
});

describe('a window that opened a little off from where it was asked to', () => {
	test('is moved by the error, so the position it is read at is the one it was asked for', () => {
		// Asked for (166, 811); the platform put it at (134, 779): 32 off each way.
		expect(positionCorrection([166, 811], [134, 779])).toEqual([198, 843]);
		// Applying the correction lands the read position on the request, if the offset is steady.
		const offset = [-32, -32];
		const fix = positionCorrection([166, 811], [166 + offset[0], 811 + offset[1]])!;
		expect([fix[0] + offset[0], fix[1] + offset[1]]).toEqual([166, 811]);
	});

	test('is left alone when it is where it was asked to be, give or take a pixel or two', () => {
		expect(positionCorrection([100, 100], [100, 100])).toBeNull();
		expect(positionCorrection([100, 100], [102, 98])).toBeNull();
	});

	test('is left alone when the error is too large to be an offset — it was put somewhere on purpose', () => {
		expect(positionCorrection([100, 100], [400, 100])).toBeNull();
		expect(positionCorrection([100, 100], [100, 300])).toBeNull();
		// One axis small and one large is still elsewhere.
		expect(positionCorrection([100, 100], [130, 500])).toBeNull();
	});

	test('is left alone for numbers that are not', () => {
		expect(positionCorrection([NaN, 0], [1, 1])).toBeNull();
		expect(positionCorrection([0, 0], [Infinity, 1])).toBeNull();
	});
});
