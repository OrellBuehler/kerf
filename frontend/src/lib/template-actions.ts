// What the template controls do, shared by the library's Templates section, the Inspector's Template section and
// the save dialog so they cannot drift: add one at the playhead, import and export a file, delete one (asking
// first), keep a title as a template, change a template title's values. The library is app-level (not undone,
// not staged); adding a template and changing values are timeline edits.

import { editor } from './state.svelte';
import { ui } from './editor-ui.svelte';
import { toast } from './notifications.svelte';
import {
	confirmAction,
	deleteTemplate,
	exportTemplate,
	importTemplate,
	pickImageFile,
	pickTemplateExportPath,
	pickTemplateFile,
	saveTemplate,
	templateImageValue,
	templatePoster
} from './api';
import { CONFIRM } from './confirm-copy';
import { failure, fileName } from './failure-copy';
import { attempt } from './ops';
import {
	appliedLine,
	deleteMessage,
	exportFileName,
	importLine,
	normalizeValue,
	savedLine,
	valueRefusal,
	type Line
} from './template-ui';
import type { TemplateControl, TemplateInfo, TemplateSaved } from './types';

/** Say a line as a toast of its kind, the engine's notes in the description. */
export function say(line: Line): void {
	const opts = line.detail ? { description: line.detail } : undefined;
	toast[line.kind](line.text, opts);
}

// ---- posters ----------------------------------------------------------------------------------

const posters = new Map<string, Promise<string | null>>();

/** A template's poster as an `<img>` data URL, asked for once per template; `null` when it has none. A failed read is
 *  asked again next time. */
export function posterOf(id: string): Promise<string | null> {
	let poster = posters.get(id);
	if (!poster) {
		poster = templatePoster(id);
		poster.catch(() => posters.delete(id));
		posters.set(id, poster);
	}
	return poster;
}

/** Forget a poster: its template was deleted, or replaced by an import under the same name. */
export function forgetPoster(id: string): void {
	posters.delete(id);
}

// ---- the library ------------------------------------------------------------------------------

/** A library template as an animated title at the playhead, as long as the template runs, selected. */
export function addTemplateHere(info: TemplateInfo): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(async () => {
		const edit = await editor.applyTemplate(info.id, {}, at);
		const line = appliedLine(info.name, edit.warnings);
		if (line) say(line);
	});
}

/** A library template as a title clip on a video track at the playhead (the lowest track free there, else a new one on
 *  top), as long as the template runs, selected: it then takes a transition, fades and keys like any clip. */
export function addTemplateClipHere(info: TemplateInfo): Promise<void> {
	const at = Math.max(0, ui.time);
	return attempt(async () => {
		const edit = await editor.applyTemplateClip(info.id, {}, at);
		const line = appliedLine(info.name, edit.warnings);
		if (line) say(line);
	});
}

/** Pick a `.kerftemplate` and bring it into the library. Never throws; resolves `true` when a file was read. */
export async function importTemplateFile(): Promise<boolean> {
	let path: string | null;
	try {
		path = await pickTemplateFile();
	} catch (e) {
		toast.error(failure("Couldn't open the file picker", e));
		return false;
	}
	if (!path) return false;
	try {
		say(importLine(await importTemplate(path)));
		return true;
	} catch (e) {
		toast.error(failure(`Couldn't import ${fileName(path)}`, e));
		return false;
	}
}

/** Ask where, then write the template there. Never throws; `true` when a file was written. */
export async function exportTemplateFile(info: TemplateInfo): Promise<boolean> {
	try {
		const path = await pickTemplateExportPath(exportFileName(info.name));
		if (!path) return false;
		toast.success(`Exported ${fileName(await exportTemplate(info.id, path))}`);
		return true;
	} catch (e) {
		toast.error(failure(`Couldn't export “${info.name}”`, e));
		return false;
	}
}

/** Ask, then remove the template from the library. Titles already made from it keep working: they carry their own
 *  artwork and fonts. Never throws; `true` when it was removed. */
export async function deleteTemplateFile(info: TemplateInfo): Promise<boolean> {
	try {
		if (!(await confirmAction(deleteMessage(info.name), CONFIRM.deleteTemplate.title, CONFIRM.deleteTemplate))) return false;
		await deleteTemplate(info.id);
		forgetPoster(info.id);
		toast.info(`Deleted “${info.name}” from your templates`);
		return true;
	} catch (e) {
		toast.error(failure(`Couldn't delete “${info.name}”`, e));
		return false;
	}
}

/** Keep an animated title in the library as a template. Rejects with the engine's reason (the dialog shows it where the
 *  fields are); says what it did and what it could not carry over. */
export async function saveTitleAsTemplate(
	overlayId: string,
	name: string,
	description: string,
	controls: TemplateControl[]
): Promise<TemplateSaved> {
	const saved = await saveTemplate(overlayId, name.trim(), controls, description.trim() || undefined);
	forgetPoster(saved.info.id);
	say(savedLine(saved));
	return saved;
}

// ---- a template title's values ----------------------------------------------------------------

/** Set one control of a template title: the value checked and put in the form the engine takes first (a refusal rejects
 *  with the field's name and the reason, and nothing is written), then one edit. The engine's notes (a font that is not
 *  installed) are said. */
export async function setTemplateValue(overlayId: string, control: TemplateControl, raw: string): Promise<void> {
	const why = valueRefusal(control, raw);
	if (why) throw new Error(`${control.label}: ${why}`);
	const edit = await editor.setTemplateValues(overlayId, { [control.id]: normalizeValue(control, raw) });
	const title = editor.titleById(overlayId);
	const line = appliedLine(title?.template?.name ?? control.label, edit.warnings);
	if (line) say(line);
}

/** Pick a picture and set an `image` control to it. Resolves `false` for a cancelled picker. A picture the engine will not
 *  keep (the wrong format, too big) rejects with its reason. */
export async function setTemplateImage(overlayId: string, control: TemplateControl): Promise<boolean> {
	const path = await pickImageFile();
	if (!path) return false;
	await setTemplateValue(overlayId, control, await templateImageValue(path));
	return true;
}
