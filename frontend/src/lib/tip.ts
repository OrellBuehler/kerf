// The themed tooltip that replaced the native `title=` (a guard test, `tip-guard.test.ts`, keeps it
// replaced). `use:tip={'Razor'}` on any element; `{ text, keys, detail }` for a label with its
// shortcut and a muted second line.
//
// What a native tooltip could not do: it was the OS's light-grey box on a dark editor, appeared
// after a second, could not show a key, and never appeared for the keyboard. This one is the
// menu's surface, shows after 400 ms (immediately once one has just shown, so a row of buttons
// reads as one sweep), on keyboard focus at once, and goes on Escape, a press, a scroll or
// leaving — but not when the pointer moves onto the tip itself (WCAG 1.4.13: dismissible,
// hoverable, persistent). It describes its element (`aria-describedby`) while it shows; an
// element with no name of its own — an icon button — takes the tip's text as its `aria-label`,
// since the `title` that used to name it is gone. No motion under `prefers-reduced-motion`.
//
// The pure parts (what to say, where to put it, when to show) are tested without a DOM; the glue
// below `createTipController` creates one tip element per *document*, so a detached panel window
// draws its own (see `realm.ts`).

import { documentOf, windowOf } from './realm';
import { accelParts } from './accel';

export interface TipSpec {
	/** The label: what the control is or does, with its state. */
	text: string;
	/** Its shortcut as the user's keymap prints it (`settings.shortcut(id)`). */
	keys?: string;
	/** A second, muted line. */
	detail?: string;
}

export type TipInput = string | TipSpec | null | undefined | false;

/** What to show for an input, or null when there is nothing to say. A string's first line is the
 *  label and the lines after it the detail. */
export function toTip(input: TipInput): TipSpec | null {
	if (!input) return null;
	if (typeof input === 'string') {
		const [first, ...rest] = input.split('\n');
		const text = first.trim();
		const detail = rest.map((l) => l.trim()).filter(Boolean).join('\n');
		return text ? { text, ...(detail ? { detail } : {}) } : null;
	}
	const text = input.text?.trim();
	if (!text) return null;
	const out: TipSpec = { text };
	if (input.keys?.trim()) out.keys = input.keys.trim();
	if (input.detail?.trim()) out.detail = input.detail.trim();
	return out;
}

/** The tip as one line of plain text — a screen reader's name for an unnamed control. */
export function tipText(input: TipInput): string {
	const spec = toTip(input);
	if (!spec) return '';
	return spec.keys ? `${spec.text} (${spec.keys})` : spec.text;
}

/** Whether an element should take the tip's text as its accessible name: it is something a user
 *  operates, and nothing else — text, `aria-label`, a `<label>` — names it. */
export function needsName(el: { interactive: boolean; text: string; ariaLabel: boolean; labelled: boolean }): boolean {
	return el.interactive && !el.ariaLabel && !el.labelled && el.text.trim() === '';
}

export interface Rect {
	left: number;
	top: number;
	width: number;
	height: number;
}

export interface Placement {
	left: number;
	top: number;
	side: 'below' | 'above';
}

/** Where a tip of `tip`'s size goes: centred under its element, or over it when there is no room
 *  beneath, kept `margin` inside the viewport. */
export function placeTip(
	anchor: Rect,
	tip: { width: number; height: number },
	viewport: { width: number; height: number },
	gap = 6,
	margin = 6
): Placement {
	const below = anchor.top + anchor.height + gap;
	const above = anchor.top - gap - tip.height;
	const fitsBelow = below + tip.height + margin <= viewport.height;
	const side = fitsBelow || above < margin ? 'below' : 'above';
	const top = side === 'below' ? Math.min(below, Math.max(margin, viewport.height - tip.height - margin)) : above;
	const centred = anchor.left + anchor.width / 2 - tip.width / 2;
	const left = Math.max(margin, Math.min(centred, viewport.width - tip.width - margin));
	return { left: Math.round(left), top: Math.round(top), side };
}

// ---- when to show ------------------------------------------------------------------

/** How long the pointer rests on a control before its tip shows. */
export const SHOW_DELAY = 400;
/** After one tip has gone, the next shows this fast for `WARM_WINDOW` ms. */
export const WARM_DELAY = 60;
export const WARM_WINDOW = 500;
/** Leaving a control keeps its tip this long, so moving to the next one swaps instead of blinking. */
export const LEAVE_GRACE = 80;

export type Via = 'pointer' | 'focus';

export interface TipEnv<E> {
	now(): number;
	after(fn: () => void, ms: number): unknown;
	cancel(handle: unknown): void;
	/** Draw (or redraw) the tip for `el`. */
	show(el: E, spec: TipSpec, via: Via): void;
	hide(): void;
}

export interface Dismissal {
	/** A tip was showing and has gone. */
	hidden: boolean;
	/** The key should not go on to anything else: the tip belonged to the focused control, and
	 *  Escape was said to it. A tip that only the pointer was holding up lets Escape through. */
	consume: boolean;
}

/** The tip's timing and who it is for, over an environment that can be faked — one controller is
 *  shared by every `use:tip`, so only one tip is ever up. */
export function createTipController<E>(env: TipEnv<E>) {
	const specs = new Map<E, TipSpec>();
	const muted = new Set<E>();
	let anchor: E | null = null;
	let shown = false;
	let via: Via = 'pointer';
	let showTimer: unknown = null;
	let hideTimer: unknown = null;
	let pending: E | null = null;
	let pointerOn: E | null = null;
	let focusOn: E | null = null;
	let hiddenAt = -Infinity;

	const clearShow = () => {
		if (showTimer !== null) env.cancel(showTimer);
		showTimer = null;
		pending = null;
	};
	const clearHide = () => {
		if (hideTimer !== null) env.cancel(hideTimer);
		hideTimer = null;
	};

	function present(el: E, v: Via) {
		const spec = specs.get(el);
		clearShow();
		clearHide();
		if (!spec) return;
		anchor = el;
		via = v;
		shown = true;
		env.show(el, spec, v);
	}

	function hideNow() {
		clearShow();
		clearHide();
		if (!shown) return;
		shown = false;
		anchor = null;
		hiddenAt = env.now();
		env.hide();
	}

	function request(el: E, v: Via) {
		if (muted.has(el) || !specs.has(el)) return;
		if (shown && anchor === el) {
			clearHide();
			via = v;
			return;
		}
		clearShow();
		clearHide();
		const delay =
			v === 'focus' || shown ? 0 : env.now() - hiddenAt < WARM_WINDOW ? WARM_DELAY : SHOW_DELAY;
		pending = el;
		showTimer = env.after(() => present(el, v), delay);
	}

	function releaseAfterGrace() {
		clearHide();
		hideTimer = env.after(() => {
			hideTimer = null;
			hideNow();
		}, LEAVE_GRACE);
	}

	function unmute(el: E) {
		if (pointerOn !== el && focusOn !== el) muted.delete(el);
	}

	return {
		/** Set (or change) what an element says. A tip already up for it is redrawn. */
		set(el: E, spec: TipSpec | null) {
			if (!spec) {
				specs.delete(el);
				if (anchor === el || pending === el) hideNow();
				return;
			}
			specs.set(el, spec);
			if (shown && anchor === el) env.show(el, spec, via);
		},
		/** The element is gone. */
		drop(el: E) {
			specs.delete(el);
			muted.delete(el);
			if (pointerOn === el) pointerOn = null;
			if (focusOn === el) focusOn = null;
			if (anchor === el || pending === el) hideNow();
		},
		enter(el: E) {
			pointerOn = el;
			request(el, 'pointer');
		},
		leave(el: E) {
			if (pointerOn === el) pointerOn = null;
			unmute(el);
			if (pending === el) clearShow();
			if (shown && anchor === el && focusOn !== el) releaseAfterGrace();
		},
		/** `visible` is `:focus-visible`: a click that focuses a button is not asking for its tip. */
		focus(el: E, visible: boolean) {
			focusOn = el;
			if (visible) request(el, 'focus');
		},
		blur(el: E) {
			if (focusOn === el) focusOn = null;
			unmute(el);
			if (shown && anchor === el && pointerOn !== el) hideNow();
			else if (pending === el && pointerOn !== el) clearShow();
		},
		/** A press on the control: it is being used, not read about. */
		press(el: E) {
			if (anchor === el || pending === el) {
				muted.add(el);
				hideNow();
			}
		},
		/** A press anywhere, a scroll, the window losing focus. */
		dismissAll() {
			hideNow();
		},
		escape(): Dismissal {
			if (!shown) return { hidden: false, consume: false };
			const consume = via === 'focus';
			if (anchor) muted.add(anchor);
			hideNow();
			return { hidden: true, consume };
		},
		get shown() {
			return shown;
		},
		get target(): E | null {
			return anchor;
		}
	};
}

// ---- the element ---------------------------------------------------------------------

interface Host {
	root: HTMLElement;
	text: HTMLElement;
	keys: HTMLElement;
	detail: HTMLElement;
}

const hosts = new WeakMap<Document, Host>();
let hostSeq = 0;

const HOST_CSS =
	'position:fixed;left:0;top:0;z-index:2000;box-sizing:border-box;max-width:min(300px,calc(100vw - 12px));padding:5px 8px;border-radius:var(--radius-sm);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-md);color:var(--text-primary);font:var(--type-ui);font-weight:var(--fw-regular);overflow-wrap:anywhere;opacity:0;visibility:hidden;pointer-events:none';

function hostFor(doc: Document): Host {
	const known = hosts.get(doc);
	if (known && known.root.isConnected) return known;
	const root = doc.createElement('div');
	root.id = `kerf-tip-${++hostSeq}`;
	root.setAttribute('role', 'tooltip');
	root.className = 'k-tip';
	root.style.cssText = HOST_CSS;
	const line = doc.createElement('div');
	line.style.cssText = 'display:flex;align-items:baseline;gap:10px';
	const text = doc.createElement('span');
	text.style.cssText = 'flex:1;min-width:0';
	const keys = doc.createElement('span');
	keys.style.cssText = 'flex:none';
	line.append(text, keys);
	const detail = doc.createElement('div');
	detail.style.cssText = 'margin-top:2px;color:var(--text-muted);white-space:pre-line';
	root.append(line, detail);
	doc.body.appendChild(root);
	const host = { root, text, keys, detail };
	hosts.set(doc, host);
	return host;
}

/** The shortcut as keycaps: `Ctrl`+`Z`, with a punctuation key in a cap of its own. */
function fillKeys(doc: Document, into: HTMLElement, keys: string | undefined) {
	into.replaceChildren();
	if (!keys) return;
	const { head, key, cap, arrow } = accelParts(keys);
	const kbd = doc.createElement('kbd');
	kbd.className = 'k-kbd';
	if (head) kbd.append(doc.createTextNode(head));
	const k = doc.createElement('span');
	k.textContent = key;
	if (cap) k.className = 'k-kcap';
	else if (arrow) k.className = 'k-arrow';
	kbd.append(k);
	into.append(kbd);
}

let shownIn: { doc: Document; el: HTMLElement } | null = null;
let stopShownListeners: (() => void) | null = null;

function describe(el: HTMLElement, id: string | null) {
	const tokens = new Set((el.getAttribute('aria-describedby') ?? '').split(/\s+/).filter((t) => t && !t.startsWith('kerf-tip-')));
	if (id) tokens.add(id);
	if (tokens.size) el.setAttribute('aria-describedby', [...tokens].join(' '));
	else el.removeAttribute('aria-describedby');
}

const controller = createTipController<HTMLElement>({
	now: () => Date.now(),
	after: (fn, ms) => setTimeout(fn, ms),
	cancel: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
	show(el, spec) {
		const doc = documentOf(el);
		const win = windowOf(el);
		const host = hostFor(doc);
		if (shownIn && shownIn.el !== el) describe(shownIn.el, null);
		host.text.textContent = spec.text;
		fillKeys(doc, host.keys, spec.keys);
		host.detail.textContent = spec.detail ?? '';
		host.detail.style.display = spec.detail ? 'block' : 'none';
		const root = host.root;
		const reduced = win.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
		root.style.transition = reduced ? 'none' : 'opacity var(--dur-fast) var(--ease-out)';
		// Measure with it laid out but unseen.
		root.style.visibility = 'hidden';
		root.style.opacity = '0';
		const r = el.getBoundingClientRect();
		const size = root.getBoundingClientRect();
		const view = { width: doc.documentElement.clientWidth, height: doc.documentElement.clientHeight };
		const at = placeTip({ left: r.left, top: r.top, width: r.width, height: r.height }, size, view);
		root.style.transform = `translate(${at.left}px,${at.top}px)`;
		root.style.visibility = 'visible';
		root.style.opacity = '1';
		describe(el, root.id);
		if (!stopShownListeners || shownIn?.doc !== doc) {
			stopShownListeners?.();
			const away = () => controller.dismissAll();
			const key = (e: KeyboardEvent) => {
				if (e.key !== 'Escape') return;
				const d = controller.escape();
				if (d.consume) {
					e.preventDefault();
					e.stopPropagation();
				}
			};
			win.addEventListener('keydown', key, true);
			win.addEventListener('pointerdown', away, true);
			win.addEventListener('scroll', away, true);
			win.addEventListener('blur', away);
			win.addEventListener('resize', away);
			stopShownListeners = () => {
				win.removeEventListener('keydown', key, true);
				win.removeEventListener('pointerdown', away, true);
				win.removeEventListener('scroll', away, true);
				win.removeEventListener('blur', away);
				win.removeEventListener('resize', away);
			};
		}
		shownIn = { doc, el };
	},
	hide() {
		stopShownListeners?.();
		stopShownListeners = null;
		if (!shownIn) return;
		const { doc, el } = shownIn;
		shownIn = null;
		describe(el, null);
		const host = hosts.get(doc);
		if (host) {
			host.root.style.opacity = '0';
			host.root.style.visibility = 'hidden';
		}
	}
});

const INTERACTIVE = /^(button|a|input|select|textarea|summary)$/i;
const INTERACTIVE_ROLES = new Set(['button', 'tab', 'menuitem', 'switch', 'checkbox', 'radio', 'slider', 'link']);

/** Whether the element has no name of its own for the tip's text to stand in for. */
function unnamed(el: HTMLElement): boolean {
	return needsName({
		interactive: INTERACTIVE.test(el.tagName) || INTERACTIVE_ROLES.has(el.getAttribute('role') ?? ''),
		text: el.textContent ?? '',
		ariaLabel: el.hasAttribute('aria-label'),
		labelled: el.hasAttribute('aria-labelledby') || ((el as HTMLInputElement).labels?.length ?? 0) > 0
	});
}

/** `<button use:tip={'Razor'}>` — see the top of this file. */
export function tip(node: HTMLElement, input: TipInput) {
	const enter = (e: PointerEvent) => {
		if (e.pointerType !== 'touch') controller.enter(node);
	};
	const leave = () => controller.leave(node);
	const focus = () => controller.focus(node, node.matches(':focus-visible'));
	const blur = () => controller.blur(node);
	const press = () => controller.press(node);
	node.addEventListener('pointerenter', enter);
	node.addEventListener('pointerleave', leave);
	node.addEventListener('focus', focus);
	node.addEventListener('blur', blur);
	node.addEventListener('pointerdown', press);

	function apply(next: TipInput) {
		const spec = toTip(next);
		// The name this action gave is the action's to keep current; one the author wrote is not.
		const gave = node.dataset.tipNamed !== undefined;
		if (spec && (gave || unnamed(node))) {
			node.setAttribute('aria-label', tipText(next));
			node.dataset.tipNamed = '';
		} else if (gave) {
			node.removeAttribute('aria-label');
			delete node.dataset.tipNamed;
		}
		controller.set(node, spec);
	}
	apply(input);

	return {
		update: apply,
		destroy() {
			node.removeEventListener('pointerenter', enter);
			node.removeEventListener('pointerleave', leave);
			node.removeEventListener('focus', focus);
			node.removeEventListener('blur', blur);
			node.removeEventListener('pointerdown', press);
			controller.drop(node);
		}
	};
}
