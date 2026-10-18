// Small shared pieces of the settings dialog's look.

/** A choice chip: on (kerf outline and wash) or off. */
export const chip = (active: boolean): string =>
	`padding:5px 10px;border-radius:var(--radius-sm);font-size:12px;cursor:pointer;white-space:nowrap;border:var(--line-width) solid ${
		active ? 'var(--kerf-500)' : 'var(--border-strong)'
	};background:${active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'};color:${
		active ? 'var(--text-primary)' : 'var(--text-secondary)'
	}`;

/** A small uppercase label over a block of rows. */
export const OVERLINE =
	'font:var(--type-label);color:var(--text-secondary);text-transform:uppercase;letter-spacing:.06em';
