// The inline style of a toggle chip: outlined and quiet at rest, the accent when
// it is the active choice. One definition for the controls that sit beside the
// Inspector's own copies (the library's tabs, the titles controls).

export function chip(active: boolean): string {
	return `padding:4px 9px;font-size:12px;cursor:pointer;border-radius:var(--radius-sm);border:var(--line-width) solid ${
		active ? 'var(--kerf-500)' : 'var(--border-strong)'
	};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
		active ? 'var(--text-primary)' : 'var(--text-secondary)'
	}`;
}
