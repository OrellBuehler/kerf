import { describe, expect, test } from 'bun:test';
import { beginDrag, type DragEnv } from './drag';

// bun has no DOM: an element and a window are EventTargets that count their
// listeners, and a "pointer event" is an Event carrying the fields a drag reads.

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
	test('captures the pointer and listens only while the gesture lasts', () => {
		const { el, win } = setup();
		expect(el.captured).toBe(7);
		expect(el.listeners).toBe(4);
		expect(win.listeners).toBe(2);
	});

	test('moves are reported, and the release commits once', () => {
		const { el, win, log, fire } = setup();
		fire(el, 'pointermove', { clientX: 10 });
		fire(el, 'pointermove', { clientX: 20 });
		fire(el, 'pointerup', { clientX: 25 });
		expect(log).toEqual(['move 10', 'move 20', 'commit 25']);
		expect(el.captured).toBeNull();
		expect(el.listeners).toBe(0);
		expect(win.listeners).toBe(0);
		// the lostpointercapture that follows a release changes nothing
		fire(el, 'lostpointercapture');
		fire(el, 'pointerup', { clientX: 99 });
		expect(log).toHaveLength(3);
	});

	test('Escape abandons, and is not passed on', () => {
		const { el, win, log, fire } = setup();
		fire(el, 'pointermove', { clientX: 10 });
		const ev = Object.assign(new Event('keydown', { cancelable: true }), { key: 'Escape' });
		let propagated = true;
		ev.stopPropagation = () => {
			propagated = false;
		};
		win.dispatchEvent(ev);
		expect(log).toEqual(['move 10', 'abandon']);
		expect(propagated).toBe(false);
		fire(el, 'pointerup', { clientX: 30 }); // nothing commits afterwards
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
			fire(el, 'pointermove', { clientX: 3 });
			if (how === 'cancel') fire(el, 'pointercancel');
			else if (how === 'blur') win.dispatchEvent(new Event('blur'));
			else fire(el, 'lostpointercapture');
			expect(log).toEqual(['move 3', 'abandon']);
			fire(el, 'pointerup');
			expect(log).toHaveLength(2);
			expect(el.listeners + win.listeners).toBe(0);
		}
	});

	test('a move with the button already up is a release that was missed', () => {
		const { el, log, fire } = setup();
		fire(el, 'pointermove', { clientX: 4, buttons: 1 });
		fire(el, 'pointermove', { clientX: 5, buttons: 0 });
		fire(el, 'pointerup', { clientX: 6 });
		expect(log).toEqual(['move 4', 'abandon']);
	});

	test('events from another pointer are not this gesture', () => {
		const { el, log, fire } = setup();
		fire(el, 'pointermove', { clientX: 1, pointerId: 99 });
		fire(el, 'pointerup', { clientX: 2, pointerId: 99 });
		fire(el, 'pointercancel', { pointerId: 99 });
		expect(log).toEqual([]);
		fire(el, 'pointerup', { clientX: 3 });
		expect(log).toEqual(['commit 3']);
	});

	test('the returned function abandons from outside, once', () => {
		const { el, win, log, cancel, fire } = setup();
		fire(el, 'pointermove', { clientX: 8 });
		cancel();
		cancel();
		expect(log).toEqual(['move 8', 'abandon']);
		expect(el.listeners + win.listeners).toBe(0);
	});

	test('a pointer that cannot be captured still drags', () => {
		const el = new Target();
		el.setPointerCapture = () => {
			throw new Error('InvalidStateError');
		};
		const log: string[] = [];
		const start = new Event('pointerdown') as unknown as PointerEvent;
		Object.assign(start, { pointerId: 1 });
		Object.defineProperty(start, 'currentTarget', { value: el });
		beginDrag(start, { move: () => {}, commit: () => log.push('commit'), abandon: () => {} }, { window: new Target() } as unknown as DragEnv);
		el.dispatchEvent(Object.assign(new Event('pointerup'), { pointerId: 1 }));
		expect(log).toEqual(['commit']);
	});
});
