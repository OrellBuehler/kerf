/* The inline style of the one-letter S / L flags in a track header. */

/** Shared look for the one-letter S / L track flags. */
export const flagBtn = (on: boolean, accent: string) =>
	`flex:none;font-family:var(--font-mono);font-size:12px;font-weight:600;line-height:1;min-width:26px;min-height:26px;padding:3px;border-radius:3px;cursor:pointer;` +
	`border:var(--line-width) solid ${on ? accent : 'var(--border-strong)'};background:${on ? accent : 'transparent'};color:${on ? 'var(--text-on-accent)' : 'var(--text-muted)'}`;
