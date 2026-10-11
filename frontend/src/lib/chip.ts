// The inline style of a toggle chip for the dialogs that still draw one by hand: outlined and quiet at
// rest, the accent when it is the active choice. The panels use `Chip` (`.k-chip` in `styles/controls.css`),
// which is the same look with hover and pressed states of its own; this matches its height, weight and
// radius so a dialog's chip and a panel's are one size.

export function chip(active: boolean = false): string {
	return `box-sizing:border-box;display:inline-flex;align-items:center;height:var(--control-sm);padding:0 10px;font-size:var(--text-sm);font-weight:500;line-height:1;white-space:nowrap;cursor:pointer;border-radius:var(--radius-sm);border:var(--line-width) solid ${
		active ? 'var(--kerf-500)' : 'var(--border-strong)'
	};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
		active ? 'var(--text-primary)' : 'var(--text-secondary)'
	}`;
}
