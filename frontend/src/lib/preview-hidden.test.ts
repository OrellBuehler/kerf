import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// The Preview is a panel like the others, so it can be closed, or sit behind another tab. Closed, it is unmounted and its
// effects end what it started (the stream, the GPU session, the surface: the bounds effect's cleanup reports the panel
// gone); behind a tab it is still mounted, with no content in the document, and must not make frames for a picture nobody
// sees. The clock and the sound are `ui`'s and are not the panel's to stop. Read off the source, as the other wiring tests
// are; what it was checked against is the browser harness with `?gpusurface=1` (see the doc's account of the dock).

const read = (path: string) => readFileSync(join(import.meta.dir, path), 'utf8');
const preview = read('components/editor/Preview.svelte');
const workspace = read('components/editor/Workspace.svelte');
const ui = read('editor-ui.svelte.ts');

describe('the Preview behind another tab or closed', () => {
	test('the dock tells the Preview when it is not on show', () => {
		expect(workspace).toContain("o.name === 'preview'");
		expect(workspace).toContain('{ panelApi: params.api }');
		expect(preview).toContain('panelApi.onDidVisibilityChange');
		expect(preview).toContain('shown = panelApi.isVisible');
	});

	test('a panel that is not on show asks for no frame', () => {
		expect(preview).toContain('width: () => (shown ? frameWidth : null)');
	});

	test('a panel that is not on show starts no stream or GPU session, and a hidden one ends its own', () => {
		expect(preview).toContain('const play = ui.playing && ui.rate === 1 && shown;');
	});

	test('the status bar stops naming a renderer when no frame is on screen', () => {
		expect(preview).toMatch(/if \(!shown\) return;\s+return \(\) =>\s+untrack\(\(\) => \{\s+frames\.surfaceGone\(\);\s+gpuPreview\.clear\(\);/);
	});

	test('the clock and the sound are the editor state’s, not the Preview’s', () => {
		expect(ui).toContain('audio.start(');
		expect(ui).toContain('audio.stop()');
		expect(preview).not.toContain('audio.start(');
		expect(preview).not.toContain('audio.stop(');
	});

	test('the surface is hidden by the panel going: the bounds effect reports it', () => {
		expect(preview).toContain("rect: null,\n\t\t\t\tdpr: 1,\n\t\t\t\tviewport: { width: 0, height: 0 },\n\t\t\t\tvisible: false,");
	});
});
