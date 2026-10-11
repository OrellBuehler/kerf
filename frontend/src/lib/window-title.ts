// What the OS says about the window: its title bar, the taskbar and the window switcher. Kerf draws its own
// title bar under the native caption, so until this the OS only ever heard "Kerf" — never the project's name,
// and never that it has work that is not saved.

/** `interview.kerf — Kerf`, or `• interview.kerf — Kerf` while there is unsaved work (the dot leads, so a
 *  taskbar that cuts the title short still shows it). */
export function windowTitle(projectName: string, unsaved: boolean): string {
	const name = projectName.trim();
	return `${unsaved ? '• ' : ''}${name ? `${name} — ` : ''}Kerf`;
}
