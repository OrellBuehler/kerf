// The pure side of voiceover: how a script splits into sentences, when each one
// lands in the finished audio, and how the dialog phrases voices and progress.
// Kerf-core synthesizes the real thing; this is what the dialog estimates with
// and what the browser dev harness fakes it from, so both agree on one set of
// numbers.

import type { CaptionStyle, Delivery, TranscriptSegment, VoiceInfo, VoiceoverProgress } from './types';

/** How fast a script is read at speed 1, words per second. An estimate: the real
 *  pace is the voice's, and only the synthesized file knows it. */
export const WORDS_PER_SECOND = 2.6;
/** The pause between two sentences of a paragraph. */
export const SENTENCE_GAP = 0.25;
/** The longer one a blank line asks for. */
export const PARAGRAPH_GAP = 0.6;

export const MIN_SPEED = 0.5;
export const MAX_SPEED = 2;
export const DEFAULT_SPEED = 1;
export const DEFAULT_VOICE = 'af_heart';

/** The shipped Kokoro voices, for the browser harness. The app asks the backend. */
export const VOICE_IDS = [
	'af_heart', 'af_bella', 'af_nicole', 'af_sarah', 'af_sky', 'af_nova', 'af_alloy', 'af_aoede',
	'af_jessica', 'af_kore', 'af_river',
	'am_michael', 'am_fenrir', 'am_puck', 'am_adam', 'am_echo', 'am_eric', 'am_liam', 'am_onyx',
	'bf_emma', 'bf_isabella', 'bf_alice', 'bf_lily',
	'bm_george', 'bm_fable', 'bm_lewis', 'bm_daniel'
];

/** `af_heart` → `Heart`. */
export function voiceName(id: string): string {
	const name = id.slice(id.indexOf('_') + 1);
	return name ? name[0].toUpperCase() + name.slice(1) : id;
}

/** A voice read from its id: the first letter is the accent (`a` US, `b` GB) and
 *  the second the gender. */
export function voiceInfo(id: string, downloaded = false): VoiceInfo {
	return {
		id,
		name: voiceName(id),
		accent: id[0] === 'b' ? 'gb' : 'us',
		gender: id[1] === 'm' ? 'male' : 'female',
		downloaded
	};
}

export function clampSpeed(speed: number): number {
	if (!Number.isFinite(speed)) return DEFAULT_SPEED;
	return Math.min(MAX_SPEED, Math.max(MIN_SPEED, speed));
}

export function fmtSpeed(speed: number): string {
	return `${speed.toFixed(2).replace(/0$/, '')}×`;
}

export function wordCount(text: string): number {
	return text.split(/\s+/).filter(Boolean).length;
}

export interface Sentence {
	text: string;
	/** A blank line came before it, so the pause ahead of it is a paragraph's. */
	paragraph: boolean;
}

/** Split a script into sentences. A sentence ends at `.`, `!`, `?` or `…` (with
 *  any closing quote or bracket) followed by whitespace, so `3.5` stays whole;
 *  a blank line always ends one, whether or not it was punctuated. */
export function splitSentences(text: string): Sentence[] {
	const out: Sentence[] = [];
	const paragraphs = text.replace(/\r\n?/g, '\n').split(/\n[ \t]*\n/);
	for (const para of paragraphs) {
		const flat = para.replace(/\s+/g, ' ').trim();
		if (!flat) continue;
		const parts = flat.match(/\S.*?(?:[.!?…]+["')\]”’]*(?=\s|$)|$)/g) ?? [];
		parts.forEach((part, i) => {
			const t = part.trim();
			if (t) out.push({ text: t, paragraph: i === 0 && out.length > 0 });
		});
	}
	return out;
}

/** When each sentence is spoken, in seconds from the start of the file: read at
 *  `WORDS_PER_SECOND × speed`, with a sentence gap between sentences and a
 *  paragraph gap where a blank line sat. */
export function scriptSegments(text: string, speed = DEFAULT_SPEED): TranscriptSegment[] {
	const pace = WORDS_PER_SECOND * clampSpeed(speed);
	const out: TranscriptSegment[] = [];
	let cursor = 0;
	for (const s of splitSentences(text)) {
		if (out.length > 0) cursor += s.paragraph ? PARAGRAPH_GAP : SENTENCE_GAP;
		const end = cursor + Math.max(wordCount(s.text), 1) / pace;
		out.push({ start: round3(cursor), end: round3(end), text: s.text });
		cursor = end;
	}
	return out;
}

/** How long the script should run, seconds; `0` for an empty one. */
export function estimateSeconds(text: string, speed = DEFAULT_SPEED): number {
	return scriptSegments(text, speed).at(-1)?.end ?? 0;
}

/** What the backend rejects with when a prepare or generate is stopped. */
export const VOICEOVER_CANCELLED = 'voiceover cancelled';

/** Whether a rejection is the user's stop rather than a failure. Tauri rejects
 *  with a bare string, the harness with an `Error`. */
export function isVoiceoverCancelled(e: unknown): boolean {
	return (e instanceof Error ? e.message : String(e)) === VOICEOVER_CANCELLED;
}

const round3 = (n: number) => Math.round(n * 1000) / 1000;

/** `~100 MB`, from the status's byte count. */
export function approxMB(bytes: number): string {
	return `~${Math.max(1, Math.round(bytes / (1024 * 1024)))} MB`;
}

const STAGE_LABELS: Record<VoiceoverProgress['stage'], string> = {
	download_runtime: 'Downloading voice runtime…',
	download_model: 'Downloading voice model…',
	download_voice: 'Downloading voice…',
	synthesize: 'Reading the script…'
};

export function stageLabel(stage: VoiceoverProgress['stage']): string {
	return STAGE_LABELS[stage] ?? 'Working…';
}

/** A vertical cut reads captions one word at a time; anything else as lines. */
export function defaultCaptionStyle(format: Pick<Delivery, 'width' | 'height'> | null | undefined): CaptionStyle {
	return format && format.height > format.width ? 'word_punch' : 'lines';
}

/** What the dialog opens with instead of its remembered choices — how the bin
 *  reopens it to regenerate a voiceover from the script that made it. */
export interface VoiceoverPrefill {
	text?: string;
	voice?: string;
	speed?: number;
}

/** What the dialog remembers between uses. */
export interface VoiceoverPrefs {
	voice: string;
	speed: number;
	caption: boolean;
	/** `null` follows the project's delivery frame. */
	captionStyle: CaptionStyle | null;
}

export const DEFAULT_PREFS: VoiceoverPrefs = {
	voice: DEFAULT_VOICE,
	speed: DEFAULT_SPEED,
	caption: true,
	captionStyle: null
};

/** A stored preference, or the defaults for anything missing or malformed. */
export function parsePrefs(raw: string | null): VoiceoverPrefs {
	if (!raw) return { ...DEFAULT_PREFS };
	try {
		const v = JSON.parse(raw) as Partial<VoiceoverPrefs> | null;
		return {
			voice: typeof v?.voice === 'string' && v.voice ? v.voice : DEFAULT_PREFS.voice,
			speed: typeof v?.speed === 'number' ? clampSpeed(v.speed) : DEFAULT_PREFS.speed,
			caption: typeof v?.caption === 'boolean' ? v.caption : DEFAULT_PREFS.caption,
			captionStyle: v?.captionStyle === 'lines' || v?.captionStyle === 'word_punch' ? v.captionStyle : null
		};
	} catch {
		return { ...DEFAULT_PREFS };
	}
}

const PREFS_KEY = 'kerf.voiceover';

/** The remembered choices; a blocked store just means the defaults. */
export function loadPrefs(): VoiceoverPrefs {
	try {
		return parsePrefs(localStorage.getItem(PREFS_KEY));
	} catch {
		return parsePrefs(null);
	}
}

export function savePrefs(prefs: VoiceoverPrefs) {
	try {
		localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
	} catch {
		/* a convenience, not state — ignore a blocked store */
	}
}
