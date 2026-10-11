// "Import media…" from inside the library: the header's button and the empty bin's drop zone are this.
// (The File menu and the shortcut run the page's own handler, which also takes dropped files.)

import { inTauri } from './api';
import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { settings } from './settings.svelte';
import { toast } from './notifications.svelte';

/** Pick files, import them, say what landed and analyze it by the user's own rules. */
export async function importMedia(): Promise<void> {
	if (!inTauri()) {
		toast.info('Importing media is available in the desktop app.');
		return;
	}
	try {
		const { imported, failed } = await editor.importMedia();
		for (const f of failed) toast.error(`Couldn't import ${f.name}: ${f.message}`);
		if (imported.length > 0) {
			toast.success(imported.length === 1 ? `Imported ${imported[0].name}` : `Imported ${imported.length} files`);
			await ui.analyzeImported(
				imported.map((a) => a.id),
				settings.autoAnalysis
			);
		}
	} catch (e) {
		toast.error(e instanceof Error ? e.message : String(e));
	}
}
