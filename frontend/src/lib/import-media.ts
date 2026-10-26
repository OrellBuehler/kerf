// Importing media: the one place that says what landed and analyzes it. The File menu and its key, the
// library header's button and the empty bin's drop zone call `importMedia`; the page's file drop calls
// `finishImport` with the paths it kept.

import { inTauri } from './api';
import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { settings } from './settings.svelte';
import { toast } from './notifications.svelte';

/** An import in flight: `editor.importMedia()` (a picker) or `editor.importPaths(paths)`. */
export type ImportJob = ReturnType<typeof editor.importMedia>;

/** Report what landed, offer to tag it (unless Settings › Editing turned that off), then analyze it by the user's own
 *  rules — one asset at a time, because each pass is ffmpeg-bound, and stoppable, because the transcription at the end
 *  of each one runs for minutes. */
export async function finishImport(job: ImportJob): Promise<void> {
	try {
		const { imported, failed, cancelled } = await job;
		for (const f of failed) toast.error(`Couldn't import ${f.name}: ${f.message}`);
		if (cancelled > 0) toast.info(imported.length > 0 ? `Import stopped — ${cancelled} file${cancelled === 1 ? '' : 's'} not imported` : 'Import stopped');
		if (imported.length === 0) return;
		toast.success(imported.length === 1 ? `Imported ${imported[0].name}` : `Imported ${imported.length} files`);
		// Offered before the analysis starts: that runs for minutes, and the prompt is about the files, not the passes.
		if (settings.askTagsOnImport) ui.openTagDialog([...new Set(imported.map((a) => a.id))], 'import');
		await ui.analyzeImported(
			imported.map((a) => a.id),
			settings.autoAnalysis
		);
	} catch (e) {
		toast.error(e instanceof Error ? e.message : String(e));
	}
}

/** Pick files and import them. */
export async function importMedia(): Promise<void> {
	if (!inTauri()) {
		toast.info('Importing media is available in the desktop app.');
		return;
	}
	await finishImport(editor.importMedia());
}
