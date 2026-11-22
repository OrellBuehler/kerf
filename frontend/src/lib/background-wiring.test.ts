import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { PAIRS, READING, SURFACES_UNDER_TINTS, TINTED, PANELS } from './theme-contrast';

// How the background-work surfaces are wired where there is no component test: the Preview's chip, the queue's row, the
// Inspector's strip and the card (what a screen reader is told, what each button is called, where keyboard focus goes), and the
// colour pairs their text sits in. Read off the source, as `proxy-queue-wiring.test.ts` does; the words and rules are tested
// where they live (`background-work`, `analysis-steps`, `proxy-info`, `bin-status`).

const read = (path: string) => readFileSync(join(import.meta.dir, path), 'utf8');
const EDITOR = 'components/editor/';
const preview = read(`${EDITOR}Preview.svelte`);
const queue = read(`${EDITOR}ProxyQueue.svelte`);
const strip = read(`${EDITOR}AssetInspector.svelte`);
const card = read(`${EDITOR}PreviewEmpty.svelte`);

describe('the Preview’s analysis chip', () => {
	test('is amber, Kerf’s own work — cyan is the agent’s — and names the file being analyzed, whoever started the run', () => {
		expect(preview).toContain('data-testid="analysis-hud"');
		expect(preview).toContain('analysisStatusLine({ name: editor.assets.find((a) => a.id === run.id)?.name');
		expect(preview).toContain('{#if ui.activeRun}');
		expect(preview).not.toMatch(/analysis-hud[^\n]*agent-400/);
		expect(preview).not.toContain("{#if ui.analyzing}<span class=\"hud\"><i class=\"dot\" style=\"background:var(--agent-400)\"");
	});

	test('the waiting chip strips the word "preview" that the source note actually starts with, not one it no longer does', () => {
		expect(preview).toContain("waitingOnProxy.text.replace(/^preview /, '')");
		expect(preview).not.toContain("replace('preview: '");
	});
});

describe('the proxy queue’s building row', () => {
	test('is amber like every other running thing, not the agent’s cyan', () => {
		expect(queue).toMatch(/\.row\[data-state='building'\] \{\s*border-color: color-mix\(in srgb, var\(--kerf-500\)/);
		expect(queue).not.toContain('agent-border');
	});
});

describe('the Inspector’s strip and the card', () => {
	test('their text is not a live region; a hidden one beside it says only the state', () => {
		for (const [name, source] of [['strip', strip], ['card', card]] as const) {
			expect(source, name).toContain('class="sr-only" role="status"');
			expect(source, name).toContain('liveWords(work)');
			// the visible group is not itself `role="status"` (it would speak on every percentage)
			expect(source, name).not.toMatch(/<div[^>]*role="status"[^>]*data-testid="(asset-work|first-run-work)"/);
			expect(source, name).toMatch(/role="group"/);
		}
	});

	test('each button has a name of its own, and a line is keyed by its slot', () => {
		expect(strip).toContain('aria-label={retryLabel(w)}');
		expect(strip).toContain('aria-label="Stop analyzing"');
		expect(strip).toContain('aria-label={r.state === \'failed\' ? `Try again: ${r.label}` : `Run ${r.label}`}');
		expect(strip).toContain('{#each work as w (workSlot(w))}');
		expect(strip).toContain('{#each facts.rows as r (r.kind)}');
	});

	test('a press of Try again or Run puts focus on the line or row that stays, and falls back to the Inspector’s first control', () => {
		expect(strip).toContain('function holdFocus(e: Event)');
		expect(strip).toContain("closest<HTMLElement>('[data-slot], [data-testid=\"analysis-row\"]')");
		expect(strip).toContain('data-testid="asset-add"');
		expect(strip).toContain('onclick={(e: MouseEvent) => fix(w, e)}');
		expect((strip.match(/holdFocus\(e\)/g) ?? []).length).toBeGreaterThanOrEqual(2);
		// the line and the row can hold focus
		expect(strip).toMatch(/data-slot=\{workSlot\(w\)\}[^>]*tabindex="-1"/);
		expect(strip).toMatch(/data-testid="analysis-row"[^>]*tabindex="-1"/);
	});

	test('the retry of failed steps goes through the gate, and every run button follows the one busy rule', () => {
		expect(strip).toContain('ui.retryAnalysis(asset.id, failedKinds)');
		expect(strip).toContain('busy: ui.analysisBusy');
		expect(read('analyze-menu.ts')).toContain('busy: ui.analysisBusy');
	});
});

describe('the colours the work lines are drawn in', () => {
	test('amber, red and green text is held to 4.5:1 on every surface it sits on, the inset one the card’s block uses included', () => {
		for (const fg of ['kerf-300', 'red-400', 'green-400'] as const) {
			const held = PAIRS.filter((p) => p.fg === fg && p.min === READING).map((p) => p.bg);
			for (const bg of [...PANELS, 'surface-inset', 'surface-hover'] as const) expect(held, `${fg} on ${bg}`).toContain(bg);
		}
	});

	test('the tints are held over surface-inset as well, and the labels say what is amber now', () => {
		expect(SURFACES_UNDER_TINTS).toContain('surface-inset');
		const whats = TINTED.map((t) => t.what);
		expect(whats).toContain('agent badge');
		expect(whats.join('|')).not.toContain('running analysis chip');
		expect(whats.join('|')).toContain('a running proxy or analysis');
	});
});
