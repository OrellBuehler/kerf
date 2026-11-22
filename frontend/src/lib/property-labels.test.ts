import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { PROPERTIES } from './channels';
import { PROPERTY_NAMES, inspectorLabel, rowName } from './property-labels';

const INSPECTOR = readFileSync(join(import.meta.dir, 'components/editor/Inspector.svelte'), 'utf8');

/** The rows the Inspector draws: label -> the quoted words of the `rangeRow(...)` call that follows it (the number it
 *  keys is one of them). */
function inspectorRows(): { label: string; words: Set<string> }[] {
	return INSPECTOR.split('{@render rangeRow(')
		.slice(1)
		.flatMap((chunk) => {
			const label = /^\s*'([^']+)'/.exec(chunk)?.[1];
			if (!label) return [];
			const body = chunk.split(/\n\s*\{@render |\{\/snippet\}|\n\s*<\//)[0];
			return [{ label, words: new Set([...body.matchAll(/'([a-z_]+)'/g)].map((m) => m[1])) }];
		});
}

describe('property labels', () => {
	test('every keyable number has a name', () => {
		expect(Object.keys(PROPERTY_NAMES).sort()).toEqual([...PROPERTIES].sort());
	});

	test('the Inspector calls each number what the table says', () => {
		const rows = inspectorRows();
		expect(rows.length).toBeGreaterThan(15);
		for (const p of PROPERTIES) {
			const want = inspectorLabel(p);
			const row = rows.find((r) => r.label === want && r.words.has(p));
			expect(row, `${p} should be the Inspector's "${want}" row`).toBeDefined();
		}
	});

	test('the panel says the section only where the label alone would not', () => {
		expect(rowName('scale')).toBe('Scale');
		expect(rowName('pos_x')).toBe('Position X');
		expect(rowName('temperature')).toBe('Warmth');
		expect(rowName('volume')).toBe('Volume');
		expect(rowName('mask_width')).toBe('Mask · Width');
		expect(rowName('crop_x')).toBe('Crop · Pan X');
	});

	test('no two rows share a name', () => {
		const names = PROPERTIES.map(rowName);
		expect(new Set(names).size).toBe(names.length);
	});
});
