import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { beginDrag, type DragEnv } from './drag';

// bun has no DOM: an element and a window are EventTargets that count their
// listeners, and a "pointer event" is an Event carrying the fields a drag reads.
// The gesture listens on the window only (an element can go while it is held), so
// the pointer's events are fired there, as the browser bubbles them from the element.

class Target extends EventTarget {
	listeners = 0;
	captured: number | null = null;
	addEventListener(type: string, cb: EventListenerOrEventListenerObject | null, opts?: boolean | AddEventListenerOptions) {
		this.listeners++;
		super.addEventListener(type, cb, opts);
	}
	removeEventListener(type: string, cb: EventListenerOrEventListenerObject | null, opts?: boolean | EventListenerOptions) {
		this.listeners--;
		super.removeEventListener(type, cb, opts);
	}
	setPointerCapture(id: number) {
		this.captured = id;
	}
	releasePointerCapture() {
		this.captured = null;
	}
}

function setup() {
	const el = new Target();
	const win = new Target();
	const log: string[] = [];
	const start = new Event('pointerdown') as unknown as PointerEvent;
	Object.assign(start, { pointerId: 7, button: 0 });
	// An Event's currentTarget is only set during dispatch; fake it for the call.
	Object.defineProperty(start, 'currentTarget', { value: el });
	const cancel = beginDrag(
		start,
		{
			move: (e) => log.push(`move ${e.clientX}`),
			commit: (e) => log.push(`commit ${e.clientX}`),
			abandon: () => log.push('abandon')
		},
		{ window: win } as unknown as DragEnv
	);
	const fire = (target: Target, type: string, fields: Record<string, unknown> = {}) =>
		target.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), { pointerId: 7, buttons: 1, ...fields }));
	return { el, win, log, fire, cancel };
}

describe('beginDrag', () => {
	test('captures the pointer and listens, on the window only, while the gesture lasts', () => {
		const { el, win } = setup();
		expect(el.captured).toBe(7);
		expect(el.listeners).toBe(0);
		expect(win.listeners).toBe(6);
	});

	test('moves are reported, and the release commits once', () => {
		const { el, win, log, fire } = setup();
		fire(win, 'pointermove', { clientX: 10 });
		fire(win, 'pointermove', { clientX: 20 });
		fire(win, 'pointerup', { clientX: 25 });
		expect(log).toEqual(['move 10', 'move 20', 'commit 25']);
		expect(el.captured).toBeNull();
		expect(el.listeners).toBe(0);
		expect(win.listeners).toBe(0);
		// the lostpointercapture that follows a release changes nothing
		fire(win, 'lostpointercapture');
		fire(win, 'pointerup', { clientX: 99 });
		expect(log).toHaveLength(3);
	});

	test('Escape abandons, and is not passed on', () => {
		const { el, win, log, fire } = setup();
		fire(win, 'pointermove', { clientX: 10 });
		const ev = Object.assign(new Event('keydown', { cancelable: true }), { key: 'Escape' });
		let propagated = true;
		ev.stopPropagation = () => {
			propagated = false;
		};
		win.dispatchEvent(ev);
		expect(log).toEqual(['move 10', 'abandon']);
		expect(propagated).toBe(false);
		fire(win, 'pointerup', { clientX: 30 }); // nothing commits afterwards
		expect(log).toHaveLength(2);
		expect(el.listeners + win.listeners).toBe(0);
	});

	test('another key does nothing', () => {
		const { win, log } = setup();
		win.dispatchEvent(Object.assign(new Event('keydown'), { key: 'a' }));
		expect(log).toEqual([]);
	});

	test('pointercancel, the window losing focus and a lost capture all abandon', () => {
		for (const how of ['cancel', 'blur', 'lost']) {
			const { el, win, log, fire } = setup();
			fire(win, 'pointermove', { clientX: 3 });
			if (how === 'cancel') fire(win, 'pointercancel');
			else if (how === 'blur') win.dispatchEvent(new Event('blur'));
			else fire(win, 'lostpointercapture');
			expect(log).toEqual(['move 3', 'abandon']);
			fire(win, 'pointerup');
			expect(log).toHaveLength(2);
			expect(el.listeners + win.listeners).toBe(0);
		}
	});

	test('a move with the button already up is a release that was missed', () => {
		const { win, log, fire } = setup();
		fire(win, 'pointermove', { clientX: 4, buttons: 1 });
		fire(win, 'pointermove', { clientX: 5, buttons: 0 });
		fire(win, 'pointerup', { clientX: 6 });
		expect(log).toEqual(['move 4', 'abandon']);
	});

	test('events from another pointer are not this gesture', () => {
		const { win, log, fire } = setup();
		fire(win, 'pointermove', { clientX: 1, pointerId: 99 });
		fire(win, 'pointerup', { clientX: 2, pointerId: 99 });
		fire(win, 'pointercancel', { pointerId: 99 });
		expect(log).toEqual([]);
		fire(win, 'pointerup', { clientX: 3 });
		expect(log).toEqual(['commit 3']);
	});

	test('the returned function abandons from outside, once', () => {
		const { el, win, log, cancel, fire } = setup();
		fire(win, 'pointermove', { clientX: 8 });
		cancel();
		cancel();
		expect(log).toEqual(['move 8', 'abandon']);
		expect(el.listeners + win.listeners).toBe(0);
	});

	test('a pointer that cannot be captured still drags', () => {
		const el = new Target();
		const win = new Target();
		el.setPointerCapture = () => {
			throw new Error('InvalidStateError');
		};
		const log: string[] = [];
		const start = new Event('pointerdown') as unknown as PointerEvent;
		Object.assign(start, { pointerId: 1 });
		Object.defineProperty(start, 'currentTarget', { value: el });
		beginDrag(start, { move: () => {}, commit: () => log.push('commit'), abandon: () => {} }, { window: win } as unknown as DragEnv);
		win.dispatchEvent(Object.assign(new Event('pointerup'), { pointerId: 1 }));
		expect(log).toEqual(['commit']);
	});

	test('an element taken out of the page mid-drag does not strand the gesture', () => {
		// A removed element gets no more events and loses its capture; the browser says so on the document.
		// Listeners on the element would never hear the release, and the window's Escape / blur hooks would stay.
		const { el, win, log, fire } = setup();
		fire(win, 'pointermove', { clientX: 5 });
		fire(win, 'lostpointercapture');
		expect(log).toEqual(['move 5', 'abandon']);
		expect(win.listeners).toBe(0);
		// ...and the next Escape is nobody's, so it is not swallowed
		let propagated = true;
		const ev = Object.assign(new Event('keydown', { cancelable: true }), { key: 'Escape' });
		ev.stopPropagation = () => {
			propagated = false;
		};
		win.dispatchEvent(ev);
		expect(propagated).toBe(true);
		expect(el.listeners).toBe(0);
	});

	test('the release reaches the gesture wherever the pointer is let go', () => {
		// The element was removed and the pointer is over something else: the release is dispatched there and bubbles to the window.
		const { win, log, fire } = setup();
		fire(win, 'pointerup', { clientX: 40 });
		expect(log).toEqual(['commit 40']);
		expect(win.listeners).toBe(0);
		fire(win, 'lostpointercapture');
		expect(log).toHaveLength(1);
	});

	test('a caller taken down mid-gesture gives it up with the returned function, once, with no listener left', () => {
		const { win, log, cancel, fire } = setup();
		fire(win, 'pointermove', { clientX: 2 });
		cancel();
		fire(win, 'pointerup', { clientX: 9 });
		expect(log).toEqual(['move 2', 'abandon']);
		expect(win.listeners).toBe(0);
	});
});

// A gesture outlives nothing it does not own: a component that is taken down while one is held must
// give it up (no commit later from a torn-down component, no stuck drag state), so the caller keeps
// what `beginDrag` returns and calls it as it goes.
describe('every drag a component starts is released as the component goes', () => {
	const dir = join(import.meta.dir, 'components');
	const svelte = (d: string): string[] =>
		readdirSync(d, { withFileTypes: true }).flatMap((f) =>
			f.isDirectory() ? svelte(join(d, f.name)) : f.name.endsWith('.svelte') ? [join(d, f.name)] : []
		);
	const users = svelte(dir)
		.map((f) => ({ f, src: readFileSync(f, 'utf8') }))
		.filter(({ src }) => /beginDrag\(/.test(src.replace(/^\s*(\/\/|\*|\/\*).*$/gm, '')));

	test('there are components that drag', () => {
		expect(users.length).toBeGreaterThanOrEqual(8);
	});

	test('the result of beginDrag is always kept', () => {
		for (const { f, src } of users) {
			const code = src.split('\n').filter((l) => !/^\s*(\/\/|\*|\/\*)/.test(l) && !l.includes('import '));
			for (const line of code.filter((l) => /beginDrag\(/.test(l))) {
				expect(line, f).toMatch(/\b[A-Za-z_]\w*\s*=\s*beginDrag\(/);
			}
		}
	});

	test('what is kept is called from a cleanup that runs as the component is destroyed', () => {
		for (const { f, src } of users) {
			const names = [...src.matchAll(/\b([A-Za-z_]\w*)\s*=\s*beginDrag\(/g)].map((m) => m[1]);
			for (const name of new Set(names)) {
				expect(src, `${f}: ${name}`).toMatch(new RegExp(`(\\$effect\\(\\(\\) => \\(\\) =>|return \\(\\) =>)\\s*\\{?[^}]*?\\b${name}\\?\\.\\(\\)`, 's'));
			}
		}
	});
});

describe('the overview strip', () => {
	const read = (f: string) => readFileSync(join(import.meta.dir, 'components/editor', f), 'utf8');

	test('stays mounted while its own gesture holds it, however the view it makes ends up', () => {
		const tl = read('Timeline.svelte');
		expect(tl).toContain('bind:held={minimapHeld}');
		expect(tl).toMatch(/\{#if ui\.minimap && hasClips && \(minimapHeld \|\| !viewCoversCut\(/);
	});

	test('going away mid-gesture stops it without moving the view', () => {
		const src = read('Minimap.svelte');
		expect(src).toContain('if (!leaving) onview(home);');
		expect(src).toMatch(/\$effect\(\(\) => \(\) => \{\s*leaving = true;\s*release\?\.\(\);/);
	});
});
