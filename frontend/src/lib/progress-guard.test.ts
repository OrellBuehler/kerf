import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { posix, SRC, uiSources } from './test-sources';

// A bar with no known size used to sit at a made-up 30 %, which reads as "a third done" for as long
// as the wait lasts; and only one of the five was a `progressbar` for assistive tech. They are all
// `ProgressBar` now: known progress fills, unknown progress sweeps, reduced motion tints.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';
const CONTROLS = read('lib/styles/controls.css');

describe('progress bars', () => {
	test('nothing draws a bar of its own, and nothing parks an unknown one at 30 %', () => {
		const offenders = uiSources(SRC)
			.map(posix)
			.filter((f) => f.endsWith('.svelte') && f !== `${EDITOR}ProgressBar.svelte`)
			.flatMap((f) =>
				read(f)
					.split('\n')
					.map((line, i) => ({ line, at: `${f}:${i + 1}` }))
					.filter(({ line }) => /\?\? 30\}%|width:\{[^}]*\}%;background:var\(--kerf-500\);transition:width/.test(line))
					.map(({ at }) => at)
			);
		expect(offenders).toEqual([]);
	});

	test('the run dialogs, the download and the panel all use it', () => {
		for (const f of ['UpdateDialog', 'VoiceoverDialog', 'StemsDialog', 'SettingsAnalysis', 'ExportDialog', 'DeliverPanel'])
			expect(read(`${EDITOR}${f}.svelte`), f).toContain('<ProgressBar');
	});

	test('a bar is a progressbar with a name, and a value only when it has one', () => {
		const bar = read(`${EDITOR}ProgressBar.svelte`);
		expect(bar).toContain('role="progressbar"');
		expect(bar).toContain('aria-label={label}');
		expect(bar).toContain('aria-valuenow={pct ?? undefined}');
		expect(bar).toContain("data-indeterminate={pct === null ? '' : undefined}");
	});

	test('with no value it sweeps, and under reduced motion it does not move', () => {
		expect(CONTROLS).toMatch(/\.k-progress\[data-indeterminate\] > i \{[^}]*animation: k-progress-sweep/);
		expect(CONTROLS).toMatch(/@keyframes k-progress-sweep/);
		const reduced = /@media \(prefers-reduced-motion: reduce\) \{\s*\.k-progress\[data-indeterminate\] > i \{([^}]*)\}/.exec(CONTROLS);
		expect(reduced, 'a reduced-motion rule for the sweep').not.toBeNull();
		expect(reduced![1]).toContain('animation: none');
		// not a bar parked at a fraction: the whole track, tinted
		expect(reduced![1]).toContain('width: 100%');
	});

	test('an export that is waiting on its proxy is indeterminate, not 0 %', () => {
		for (const f of ['ExportDialog', 'DeliverPanel']) expect(read(`${EDITOR}${f}.svelte`), f).toContain('progress?.waiting ? null :');
	});
});
