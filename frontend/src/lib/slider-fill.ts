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

export function installSliderFill(root: Document = document) {
	const all = () => root.querySelectorAll<HTMLInputElement>('input[type="range"]').forEach(sync);

	const desc = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
	if (desc?.set && desc.get) {
		Object.defineProperty(HTMLInputElement.prototype, 'value', {
			...desc,
			set(this: HTMLInputElement, v: string) {
				desc.set!.call(this, v);
				if (isRange(this)) sync(this);
			}
		});
	}

	root.addEventListener('input', (e) => isRange(e.target) && sync(e.target), true);
	new MutationObserver((records) => {
		for (const r of records) {
			if (r.type === 'attributes' && isRange(r.target)) sync(r.target);
			for (const n of r.addedNodes) {
				if (isRange(n)) sync(n);
				else if (n instanceof Element) n.querySelectorAll<HTMLInputElement>('input[type="range"]').forEach(sync);
			}
		}
	}).observe(root.documentElement, {
		subtree: true,
		childList: true,
		attributes: true,
		attributeFilter: ['min', 'max', 'value', 'type']
	});
	all();
}
