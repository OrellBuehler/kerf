// Chromium and WebKit have no pseudo-element for the part of a range track
// left of the thumb, so the stylesheet paints it from `--slider-fill`. One
// mechanism keeps that property current for every slider: a document `input`
// listener for drags, the `value` setter (which is how Svelte writes a bound
// or reactive value, and fires no event) and a MutationObserver for sliders
// that appear or change their range.

function sync(el: HTMLInputElement) {
	const min = parseFloat(el.min || '0');
	const max = parseFloat(el.max || '100');
	const v = parseFloat(el.value);
	const pct = max > min && Number.isFinite(v) ? Math.min(1, Math.max(0, (v - min) / (max - min))) * 100 : 0;
	el.style.setProperty('--slider-fill', `${pct}%`);
}

const isRange = (n: unknown): n is HTMLInputElement => n instanceof HTMLInputElement && n.type === 'range';

/** The `value` setter is patched once for the page: elements are made by the editor's
 *  realm whichever window they end up in, so one patch covers them all. */
let patched = false;

/** Keep `--slider-fill` current for every range input in `root`. The editor window's
 *  document is installed once at start-up; a detached window's document is installed
 *  when it opens, because its listeners and observer are its own. Returns what takes
 *  them down again. */
export function installSliderFill(root: Document = document): () => void {
	const all = () => root.querySelectorAll<HTMLInputElement>('input[type="range"]').forEach(sync);

	const desc = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
	if (!patched && desc?.set && desc.get) {
		patched = true;
		Object.defineProperty(HTMLInputElement.prototype, 'value', {
			...desc,
			set(this: HTMLInputElement, v: string) {
				desc.set!.call(this, v);
				if (isRange(this)) sync(this);
			}
		});
	}

	const onInput = (e: Event) => isRange(e.target) && sync(e.target);
	root.addEventListener('input', onInput, true);
	// The window's own constructor: an observer made by another window's is not told
	// what happens in this one's document on every engine.
	const Observer = (root.defaultView as (Window & typeof globalThis) | null)?.MutationObserver ?? MutationObserver;
	const observer = new Observer((records) => {
		for (const r of records) {
			if (r.type === 'attributes' && isRange(r.target)) sync(r.target);
			for (const n of r.addedNodes) {
				if (isRange(n)) sync(n);
				else if (n instanceof Element) n.querySelectorAll<HTMLInputElement>('input[type="range"]').forEach(sync);
			}
		}
	});
	observer.observe(root.documentElement, {
		subtree: true,
		childList: true,
		attributes: true,
		attributeFilter: ['min', 'max', 'value', 'type']
	});
	all();
	return () => {
		root.removeEventListener('input', onInput, true);
		observer.disconnect();
	};
}
