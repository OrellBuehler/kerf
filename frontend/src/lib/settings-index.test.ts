import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ENTRIES, SECTIONS, colorGroupId, filterSettings, matches, sectionOrFirst, type SectionId } from './settings-index';
import { ANALYSIS_KINDS } from './analysis-steps';
import { COLOR_GROUPS } from './theme';

const EDITOR = join(import.meta.dir, 'components', 'editor');
const FILES: Record<Exclude<SectionId, 'keyboard'>, string> = {
	general: 'SettingsGeneral.svelte',
	appearance: 'SettingsAppearance.svelte',
	editing: 'SettingsEditing.svelte',
	preview: 'SettingsPreview.svelte',
	performance: 'SettingsPerformance.svelte',
	analysis: 'SettingsAnalysis.svelte',
	agent: 'SettingsAgent.svelte',
	advanced: 'SettingsAdvanced.svelte'
};

/** The ids a section's file answers to: `<SettingRow id="x">` and `filter.shows('x')`. */
function idsIn(file: string): Set<string> {
	const src = readFileSync(join(EDITOR, file), 'utf8');
	const ids = new Set<string>();
	for (const m of src.matchAll(/<SettingRow[^>]*?\bid="([^"{]+)"/g)) ids.add(m[1]);
	for (const m of src.matchAll(/filter\.shows\('([^']+)'\)/g)) ids.add(m[1]);
	for (const m of src.matchAll(/data-setting="([^"{]+)"/g)) ids.add(m[1]);
	return ids;
}

describe('the index and the rows say the same thing', () => {
	for (const [section, file] of Object.entries(FILES)) {
		test(`${section}: every row is searchable and every entry has a row`, () => {
			const found = idsIn(file);
			// Rows that are made in a loop name themselves from data.
			const dynamic = new Set<string>([
				...ANALYSIS_KINDS.map((k) => `analyze-${k.kind}`),
				...COLOR_GROUPS.map((g) => colorGroupId(g.label))
			]);
			const entries = new Set(ENTRIES.filter((e) => e.section === section && !dynamic.has(e.id)).map((e) => e.id));
			const rows = new Set([...found].filter((id) => !dynamic.has(id)));
			expect([...rows].sort()).toEqual([...entries].sort());
		});
	}

	test('the looped rows are in the index too', () => {
		const src = readFileSync(join(EDITOR, FILES.analysis), 'utf8');
		expect(src).toContain('id="analyze-{k.kind}"');
		for (const k of ANALYSIS_KINDS) expect(ENTRIES.some((e) => e.id === `analyze-${k.kind}`)).toBe(true);
		expect(readFileSync(join(EDITOR, FILES.appearance), 'utf8')).toContain('colorGroupId(g.label)');
		for (const g of COLOR_GROUPS) expect(ENTRIES.some((e) => e.id === colorGroupId(g.label))).toBe(true);
	});

	test('ids are unique and every entry is in a known section', () => {
		const ids = ENTRIES.map((e) => e.id);
		expect(new Set(ids).size).toBe(ids.length);
		const known = new Set(SECTIONS.map((s) => s.id));
		for (const e of ENTRIES) expect(known.has(e.section), e.id).toBe(true);
	});

	test('the sections are the nine the rail lists, General first', () => {
		expect(SECTIONS.map((s) => s.label)).toEqual([
			'General',
			'Appearance',
			'Editing',
			'Preview & playback',
			'Performance',
			'Analysis',
			'Agent',
			'Keyboard',
			'Advanced'
		]);
		expect(sectionOrFirst(undefined)).toBe('general');
		expect(sectionOrFirst('agent')).toBe('agent');
		expect(sectionOrFirst('speech')).toBe('general');
	});
});

describe('searching', () => {
	test('an empty query shows everything', () => {
		const f = filterSettings('   ');
		expect(f.query).toBe('');
		expect(f.ids.size).toBe(ENTRIES.length);
		expect(f.sections).toEqual(SECTIONS.map((s) => s.id));
	});

	test('a word finds the setting by its label, a keyword or the section it is in', () => {
		expect(filterSettings('timecode').ids.has('time-display')).toBe(true);
		expect(filterSettings('loudness').ids.has('analyze-loudness')).toBe(true);
		expect(filterSettings('whisper').ids.has('speech-model')).toBe(true);
		expect(filterSettings('token').ids.has('mcp-token')).toBe(true);
		// a section's name finds all of it
		const advanced = filterSettings('advanced');
		expect(advanced.sections).toEqual(['advanced']);
		expect(advanced.counts.advanced).toBe(ENTRIES.filter((e) => e.section === 'advanced').length);
	});

	test('every word has to match, in any order, in any case', () => {
		expect(filterSettings('RESET layout').ids.has('reset-layout')).toBe(true);
		expect(filterSettings('layout reset').ids.has('reset-layout')).toBe(true);
		expect(filterSettings('reset zebra').ids.size).toBe(0);
		expect(filterSettings('qwertyuiop').sections).toEqual([]);
	});

	test('the counts and the section list agree', () => {
		const f = filterSettings('snap');
		expect(f.ids.has('snap-default')).toBe(true);
		for (const s of SECTIONS) expect(f.counts[s.id] > 0).toBe(f.sections.includes(s.id));
	});

	test('a color name finds its group', () => {
		const f = filterSettings('scrim');
		expect(f.ids.has(colorGroupId('Surfaces'))).toBe(true);
		expect(matches(ENTRIES[0], '')).toBe(true);
	});
});
