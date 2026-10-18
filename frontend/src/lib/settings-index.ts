// The settings dialog as data: its sections and every setting in them, with the words a person might type
// to find one. The dialog filters on this (`filterSettings`), and the section components show a row only
// when `shows(id)` says so — `settings-index.test.ts` holds the two to each other, so a row that is not
// searchable (or an entry no row answers to) fails a test instead of being found missing.

import { ANALYSIS_KINDS } from './analysis-steps';
import { COLOR_GROUPS, SHAPE_TOKENS } from './theme';

export type SectionId =
	| 'general'
	| 'appearance'
	| 'editing'
	| 'preview'
	| 'performance'
	| 'analysis'
	| 'agent'
	| 'keyboard'
	| 'advanced';

export interface SectionInfo {
	id: SectionId;
	label: string;
	icon: string;
	/** One line under the heading. */
	blurb: string;
}

/** In the order the rail lists them; the dialog opens on the first. */
export const SECTIONS: readonly SectionInfo[] = [
	{ id: 'general', label: 'General', icon: 'settings', blurb: 'How times read, what opens at launch, recent projects.' },
	{ id: 'appearance', label: 'Appearance', icon: 'palette', blurb: 'The theme: colors, lines, shadows and sliders.' },
	{ id: 'editing', label: 'Editing', icon: 'Scissors', blurb: 'What a fresh session starts with, and how long the things Kerf places are.' },
	{ id: 'preview', label: 'Preview & playback', icon: 'eye', blurb: 'Proxies, which file the preview decodes, guides and the GPU preview.' },
	{ id: 'performance', label: 'Performance', icon: 'sliders-horizontal', blurb: 'How much of the machine a heavy job may take.' },
	{ id: 'analysis', label: 'Analysis', icon: 'scan-line', blurb: 'What runs on imported media, and the speech models.' },
	{ id: 'agent', label: 'Agent', icon: 'plug', blurb: 'Let an AI agent connect and edit — and review what it proposes.' },
	{ id: 'keyboard', label: 'Keyboard', icon: 'keyboard', blurb: 'Every shortcut, and how to change it.' },
	{ id: 'advanced', label: 'Advanced', icon: 'code-xml', blurb: 'Logs, the settings file and starting over.' }
];

export interface SettingEntry {
	/** What the row calls itself (`<SettingRow id>` / `shows(id)`). */
	id: string;
	section: SectionId;
	label: string;
	/** Other words that should find it. */
	keywords: readonly string[];
}

const e = (id: string, section: SectionId, label: string, ...keywords: string[]): SettingEntry => ({ id, section, label, keywords });

const slug = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

/** The id of a color group's block in Appearance. */
export const colorGroupId = (label: string) => `colors-${slug(label)}`;

export const ENTRIES: readonly SettingEntry[] = [
	e('time-display', 'general', 'Time display', 'timecode', 'seconds', 'frames', 'clock', 'format', 'smpte'),
	e('reopen-last', 'general', 'Reopen the last project', 'launch', 'startup', 'start', 'open', 'restore'),
	e('recent-projects', 'general', 'Recent projects', 'history', 'open recent', 'clear', 'forget'),

	e('theme', 'appearance', 'Theme', 'preset', 'dark', 'light', 'high contrast', 'scheme', 'name', 'custom'),
	e('theme-file', 'appearance', 'Import and export a theme', 'import', 'export', 'json', 'share', 'file'),
	e('shapes', 'appearance', 'Lines, shadows and sliders', 'line', 'border', 'shadow', 'radius', 'corner', 'slider', 'thumb', ...SHAPE_TOKENS.map((t) => t.label)),
	...COLOR_GROUPS.map((g) => e(colorGroupId(g.label), 'appearance', `${g.label} colors`, 'color', 'colour', 'picker', ...g.tokens.map((t) => t.label))),

	e('snap-default', 'editing', 'Snapping at launch', 'snap', 'magnet', 'magnetic', 'edges', 'playhead', 'default'),
	e('still-length', 'editing', 'Length of a still image', 'image', 'photo', 'picture', 'seconds', 'duration', 'default'),
	e('title-length', 'editing', 'Length of a new title', 'text', 'caption', 'seconds', 'duration', 'default'),

	e('proxy-size', 'preview', 'Preview proxies', 'proxy', 'size', '720', '1080', '1280', 'scrub', '4k', 'smooth', 'lightweight'),
	e('preview-source', 'preview', 'Preview source', 'original', 'proxy', 'decode', 'auto'),
	e('safe-areas', 'preview', 'Safe areas', 'guides', 'vertical', 'reels', 'tiktok', 'shorts', 'title safe', 'overlay'),
	e('gpu-preview', 'preview', 'GPU preview', 'gpu', 'wgpu', 'compositor', 'experimental', 'hardware', 'renderer'),

	e('cpu-limit', 'performance', 'CPU limit', 'cores', 'threads', 'background', 'balanced', 'full speed', 'priority', 'ffmpeg', 'render', 'speed'),

	e('analyze-on-import', 'analysis', 'Analyze new media on import', 'automatic', 'auto', 'import', 'master'),
	...ANALYSIS_KINDS.map((k) => e(`analyze-${k.kind}`, 'analysis', k.label, k.action, 'analysis', 'on import')),
	e('speech-model', 'analysis', 'Speech-to-text model', 'whisper', 'transcribe', 'transcription', 'download', 'model', 'language'),
	e('voiceover-model', 'analysis', 'Voiceover model', 'voice', 'tts', 'text to speech', 'kokoro', 'download', 'narration'),

	e('agent-review', 'agent', 'Review the agent’s edits', 'approve', 'proposal', 'staged', 'apply', 'safety', 'auto-apply'),
	e('mcp-enabled', 'agent', 'Agent connections', 'mcp', 'server', 'enable', 'disable', 'on', 'off'),
	e('mcp-address', 'agent', 'Address', 'mcp', 'port', 'host', 'url', 'localhost', '127.0.0.1', '7777', 'endpoint'),
	e('mcp-token', 'agent', 'Access token', 'mcp', 'secret', 'bearer', 'authorization', 'password', 'regenerate', 'copy', 'require'),
	e('mcp-connect', 'agent', 'Connect an agent', 'mcp', 'claude code', 'claude', 'cursor', 'client', 'json', 'config', 'command', 'instructions', 'cli'),

	e('keyboard', 'keyboard', 'Keyboard shortcuts', 'shortcut', 'keys', 'hotkey', 'binding', 'rebind', 'chord'),

	e('open-logs', 'advanced', 'Open the log folder', 'log', 'logs', 'debug', 'diagnostics', 'support'),
	e('reveal-settings', 'advanced', 'Show the settings file', 'file', 'json', 'config', 'folder', 'reveal'),
	e('reset-layout', 'advanced', 'Reset the window layout', 'workspace', 'panels', 'dock', 'arrangement', 'layout'),
	e('reset-all', 'advanced', 'Reset all settings', 'defaults', 'factory', 'start over', 'restore', 'clear')
];

/** Whether `entry` answers to `query`: every word of the query is in its label, its section's name or a keyword. */
export function matches(entry: SettingEntry, query: string): boolean {
	const words = query.toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length === 0) return true;
	const section = SECTIONS.find((s) => s.id === entry.section)?.label ?? '';
	const hay = [entry.label, section, ...entry.keywords].join(' ').toLowerCase();
	return words.every((w) => hay.includes(w));
}

export interface Filtered {
	/** The query, trimmed. Empty: no filter, everything shows. */
	query: string;
	/** The ids of the entries that match (all of them for an empty query). */
	ids: ReadonlySet<string>;
	/** How many entries of each section match. */
	counts: Readonly<Record<SectionId, number>>;
	/** The sections with at least one match, in rail order. */
	sections: readonly SectionId[];
}

export function filterSettings(query: string, entries: readonly SettingEntry[] = ENTRIES): Filtered {
	const q = query.trim();
	const ids = new Set<string>();
	const counts = Object.fromEntries(SECTIONS.map((s) => [s.id, 0])) as Record<SectionId, number>;
	for (const entry of entries) {
		if (!matches(entry, q)) continue;
		ids.add(entry.id);
		counts[entry.section]++;
	}
	return { query: q, ids, counts, sections: SECTIONS.map((s) => s.id).filter((id) => counts[id] > 0) };
}

/** The section a stored/requested name means, else the first. */
export function sectionOrFirst(id: string | null | undefined): SectionId {
	return SECTIONS.find((s) => s.id === id)?.id ?? SECTIONS[0].id;
}
