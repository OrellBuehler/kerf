import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// A licence notice that ships with the app is listed in `bundle.resources` of tauri.conf.json,
// and CI's bundle jobs (`pr-build.yml`, `release.yml`) pass a `--config` override whose
// `resources` *replaces* that array rather than extending it. A notice added to one and not the
// other is a build that ships without it (the PR build's Windows installer lacked the misaki-rs
// notice that way). Every override must carry all of the committed ones; the extras it adds are
// the ffmpeg sidecar's own licence files, which only the jobs that bundle ffmpeg have.

const REPO = join(SRC, '..', '..');
const COMMITTED: string[] = JSON.parse(readFileSync(join(REPO, 'crates/kerf-app/tauri.conf.json'), 'utf8')).bundle.resources;

/** Each `config: '{…}'` the workflow passes to the bundler, and what it overrides `resources` with. */
function overrides(workflow: string): { resources: string[] }[] {
	const text = readFileSync(join(REPO, '.github/workflows', workflow), 'utf8');
	return [...text.matchAll(/^\s*config:\s*'(\{.*\})'\s*$/gm)]
		.map((m) => JSON.parse(m[1]) as { bundle?: { resources?: string[] } })
		.filter((c) => c.bundle?.resources !== undefined)
		.map((c) => ({ resources: c.bundle!.resources! }));
}

describe('bundle resources', () => {
	test('the committed list names the notices that ship', () => {
		expect(COMMITTED.length).toBeGreaterThanOrEqual(3);
		for (const f of ['licenses/whisper.cpp-NOTICE.md', 'licenses/misaki-rs-NOTICE.md', 'licenses/fonts-NOTICE.md']) {
			expect(COMMITTED, f).toContain(f);
		}
	});

	for (const workflow of ['pr-build.yml', 'release.yml']) {
		test(`${workflow}: every resources override carries every committed notice`, () => {
			const found = overrides(workflow);
			expect(found.length, `${workflow} has a resources override`).toBeGreaterThan(0);
			for (const { resources } of found) {
				expect(COMMITTED.filter((f) => !resources.includes(f)), `${workflow} override ${JSON.stringify(resources)}`).toEqual([]);
				// What it adds beyond them is the ffmpeg sidecar's licence files, and nothing else.
				expect(resources.filter((f) => !COMMITTED.includes(f) && !/^licenses\/FFmpeg-/.test(f))).toEqual([]);
			}
		});
	}

	test('every listed file exists', () => {
		for (const f of COMMITTED) expect(() => readFileSync(join(REPO, 'crates/kerf-app', f)), f).not.toThrow();
	});
});
