/* What the Inspector says when a clip's LUT cannot be used. `inspect_lut` refuses with the engine's sentence
 * (`color_lut.rs`): "invalid argument: cannot open LUT /a/look.cube: No such file or directory (os error 2)", "… not a usable
 * .cube LUT (line 5): …". Those are for a log; this says what it means for the picture and what to do. The
 * engine leaves a LUT it cannot read out of a preview and a still, and refuses an export that has one
 * (`check_export_luts`), so that is what the sentence promises. The file's name is never in these words: the row above them shows it
 * (an underscored name with no break in it clipped a warning that repeated it). */

export type LutProblemKind = 'missing' | 'unreadable' | 'unusable';

export interface LutProblem {
	kind: LutProblemKind;
	/** The short form, for a section's summary: `File missing`. */
	short: string;
	/** The one line the section shows: what is wrong and the cost that matters most. */
	line: string;
	/** What is wrong and what it does, in full: the line's tooltip, and what a screen reader is told. */
	text: string;
	/** What the engine said, minus its prefix, for the tooltip; may be empty. */
	detail: string;
}

const COST = 'The preview leaves the look out, and an export will not start until the file is back or the LUT is removed.';
const COST_LINE = 'export will not start';

const PREFIX = /^(?:invalid argument:\s*)+/i;

/** The engine's refusal of a LUT, as the Inspector says it. */
export function lutProblem(message: string): LutProblem {
	const detail = message.replace(PREFIX, '').trim();
	const lower = detail.toLowerCase();
	// "No such file" (Unix), "cannot find the file / path specified" (Windows), os errors 2 and 3
	const gone = /no such file|cannot find the (?:file|path)|not found|os error (?:2|3)\b/.test(lower);
	if (lower.startsWith('cannot open lut') && gone) {
		return { kind: 'missing', short: 'File missing', line: `File not found — ${COST_LINE}.`, text: `Kerf can't find this file. ${COST}`, detail };
	}
	if (lower.startsWith('cannot open lut') || lower.startsWith('cannot read lut')) {
		return {
			kind: 'unreadable',
			short: 'Can’t open file',
			line: `Can’t open the file — ${COST_LINE}.`,
			text: `Kerf can't open this file (is it still readable?). ${COST}`,
			detail
		};
	}
	if (lower.startsWith('the lut path must be absolute')) {
		return { kind: 'missing', short: 'File missing', line: `File not found — ${COST_LINE}.`, text: `Kerf can't find this file: its place was not written in full. ${COST}`, detail };
	}
	if (/is larger than/.test(lower)) {
		return { kind: 'unusable', short: 'Too large', line: `Too large for a LUT — ${COST_LINE}.`, text: `This file is too large to be a LUT. ${COST}`, detail };
	}
	if (lower.startsWith('not a usable .cube lut')) {
		const at = /\(line (\d+)\):\s*(.*)$/.exec(detail);
		const why = at ? ` (line ${at[1]}: ${at[2]})` : '';
		return {
			kind: 'unusable',
			short: 'Not a usable LUT',
			line: `Not a usable LUT — ${COST_LINE}.`,
			text: `This is not a 3D .cube LUT Kerf can read${why}. ${COST}`,
			detail
		};
	}
	return { kind: 'unusable', short: 'Not applied', line: `Not applied — ${COST_LINE}.`, text: `This file cannot be used${detail ? `: ${detail}` : ''}. ${COST}`, detail };
}

/** A LUT's facts when it reads: `33³ · Teal and orange`. */
export function lutFacts(info: { size: number; title?: string | null }): string {
	return `${info.size}³${info.title ? ` · ${info.title}` : ''}`;
}
