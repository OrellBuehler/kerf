import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';
import {
	BATCHABLE_TOOLS,
	MAX_EDITS,
	batchLabel,
	checkEdits,
	compactAnswer,
	harnessEdits,
	resolve,
	runEdits,
	type BatchApi,
	type HarnessEdit,
	type HarnessHost
} from './batch-edits';
import type { Clip } from './types';

// The page-side half of `apply_edits` (`commands/editing.rs`, `mcp/batch.rs`): what is checked before anything runs,
// the `"$K.path"` references between ops, the answers cut down to their place and the name of the revision. The edits
// themselves run in the browser harness through `api.ts` (`api-batch.test.ts`).

const BATCH_RS = readFileSync(join(SRC, '../../crates/kerf-app/src/mcp/batch.rs'), 'utf8');

const clip = (over: Partial<Clip> = {}): Clip => ({
	id: 'c1',
	asset_id: 'a1',
	source_in: 2,
	source_out: 6,
	timeline_start: 10,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...over
});

/** A table of ops that only note they ran: the checks and the runner do not care what an op does. */
const probe = (ran: string[] = []): Record<string, HarnessEdit> => ({
	trim: { keys: ['clip_id', 'source_in', 'ripple'], run: async (a) => (ran.push(`trim ${JSON.stringify(a)}`), { id: 'x' }) },
	set_volume: { keys: ['clip_id', 'volume'], run: async (a) => (ran.push(`volume ${JSON.stringify(a)}`), 'ok') },
	set_grade: { keys: ['clip_id', 'lut_path'], run: async () => null },
	split_at: { keys: ['clip_id', 'at'], run: async () => ({ left: clip({ id: 'l' }), right: clip({ id: 'r' }) }) }
});

const host: HarnessHost = {
	last: () => undefined,
	clip: (id) => (id === 'nope' ? undefined : clip({ id })),
	withRipple: (_r, run) => run()
};

describe('the tools the page lists against the backend', () => {
	test('BATCHABLE_TOOLS is the list `apply_edits` runs, in its order', () => {
		const block = BATCH_RS.slice(BATCH_RS.indexOf('batchable! {', BATCH_RS.indexOf('macro_rules')), BATCH_RS.indexOf('/// Run one op'));
		const rust = [...block.matchAll(/^ {4}(\w+) =>/gm)].map((m) => m[1]);
		expect(rust.length).toBeGreaterThan(60);
		expect([...BATCHABLE_TOOLS]).toEqual(rust);
	});

	test('MAX_EDITS is the most the GUI command runs', () => {
		const rust = BATCH_RS.match(/pub\(crate\) const MAX_USER_OPS: usize = (\d+);/)?.[1];
		expect(MAX_EDITS).toBe(Number(rust));
	});

	test('the harness runs only tools the backend does, and each keeps to the arguments it names', () => {
		const table = harnessEdits(new Proxy({}, { get: () => async () => undefined }) as unknown as BatchApi);
		const names = Object.keys(table);
		expect(names.length).toBeGreaterThan(30);
		for (const name of names) {
			expect(BATCHABLE_TOOLS, name).toContain(name);
			expect(new Set(table[name].keys).size, `${name} lists a key twice`).toBe(table[name].keys.length);
		}
	});
});

describe('checkEdits', () => {
	const run = (edits: { tool: string; args?: Record<string, unknown> }[]) => () => checkEdits(edits, probe());

	test('refuses an empty list and more than the most', () => {
		expect(run([])).toThrow('at least one edit');
		const many = Array.from({ length: MAX_EDITS + 1 }, () => ({ tool: 'set_volume', args: { clip_id: 'c1', volume: 1 } }));
		expect(run(many)).toThrow(`lists ${MAX_EDITS + 1} edits; one call runs at most ${MAX_EDITS}`);
		expect(() => checkEdits(many.slice(0, MAX_EDITS), probe())).not.toThrow();
	});

	test('names the op, and refuses what is no edit, what the harness lacks and a key the tool does not take', () => {
		expect(run([{ tool: 'set_volume', args: { clip_id: 'c1', volume: 1 } }, { tool: 'export' }])).toThrow('op 1 (export): not an edit');
		expect(run([{ tool: 'frobnicate' }])).toThrow('op 0 (frobnicate): not an edit');
		expect(run([{ tool: 'add_marker', args: { time: 1, name: 'x' } }])).toThrow('op 0 (add_marker): not available in the browser harness');
		expect(run([{ tool: 'trim', args: { clip_id: 'c1', rippel: true } }])).toThrow(
			'op 0 (trim): unknown argument "rippel" for trim; expected: clip_id, ripple, source_in'
		);
		expect(run([{ tool: 'toString' }])).toThrow('not an edit');
	});

	test('a LUT file is read on its own; removing one is fine', () => {
		expect(run([{ tool: 'set_grade', args: { clip_id: 'c1', lut_path: '/a.cube' } }])).toThrow('lut_path');
		expect(run([{ tool: 'set_grade', args: { clip_id: 'c1', lut_path: '' } }])).not.toThrow();
	});
});

describe('"$K.path" references', () => {
	const answers = [{ right: { id: 'r1' }, list: [{ id: 'p0' }, { id: 'p1' }] }, 'ok'];

	test('become the value at the path of an earlier answer, wherever they sit in the arguments', () => {
		expect(resolve({ clip_id: '$0.right.id' }, answers)).toEqual({ clip_id: 'r1' });
		expect(resolve({ clip_ids: ['$0.list.1.id', '$0.list.0.id'], nested: { x: '$1' } }, answers)).toEqual({
			clip_ids: ['p1', 'p0'],
			nested: { x: 'ok' }
		});
		expect(resolve({ right: '$0.right' }, answers)).toEqual({ right: { id: 'r1' } });
	});

	test('$$ is a literal dollar and a string that is no reference stays', () => {
		expect(resolve({ name: '$$5 shot', cost: '$5 shot', word: '$abc', odd: '$0. x', plain: 'a.b' }, answers)).toEqual({
			name: '$5 shot',
			cost: '$5 shot',
			word: '$abc',
			odd: '$0. x',
			plain: 'a.b'
		});
	});

	test('an op that has not run, or a path its answer lacks, is refused', () => {
		expect(() => resolve('$2.id', answers)).toThrow('"$2.id" refers to op 2, which has not run yet');
		expect(() => resolve('$0.left.id', answers)).toThrow("\"$0.left.id\": op 0's answer has nothing at `left.id`");
		expect(() => resolve('$1.id', answers)).toThrow("has nothing at `id`");
	});

	test('what it hands back is a copy', () => {
		const taken = resolve('$0.right', answers) as { id: string };
		taken.id = 'changed';
		expect((answers[0] as { right: { id: string } }).right.id).toBe('r1');
	});
});

describe('answers', () => {
	test('one clip is answered whole, the clips inside a larger answer are cut down to their place', () => {
		const whole = clip({ link_id: 'g', transform: { scale: 2 } as Clip['transform'] });
		expect(compactAnswer(whole)).toBe(whole);
		expect(compactAnswer({ left: whole, right: clip({ id: 'c2', timeline_start: 14 }), n: 3 })).toEqual({
			left: { id: 'c1', asset_id: 'a1', timeline_start: 10, timeline_end: 14, source_in: 2, source_out: 6, link_id: 'g' },
			right: { id: 'c2', asset_id: 'a1', timeline_start: 14, timeline_end: 18, source_in: 2, source_out: 6 },
			n: 3
		});
		expect(compactAnswer([whole])).toEqual([
			{ id: 'c1', asset_id: 'a1', timeline_start: 10, timeline_end: 14, source_in: 2, source_out: 6, link_id: 'g' }
		]);
		expect(compactAnswer('ok')).toBe('ok');
		expect(compactAnswer(null)).toBeNull();
	});

	test('the speed of a clip is in its end, as in the backend', () => {
		expect((compactAnswer([clip({ speed: 2 })]) as { timeline_end: number }[])[0].timeline_end).toBe(12);
	});
});

describe('the name of a batch given none', () => {
	test('one edit keeps its own name, several are counted by kind', () => {
		expect(batchLabel(['Trim clip'])).toBe('Trim clip');
		expect(batchLabel(['Trim clip', 'Trim clip', 'Set volume'])).toBe('3 edits: Trim clip, Set volume');
		expect(batchLabel(['a', 'b', 'c', 'd', 'a'])).toBe('5 edits: a, b, c, …');
	});

	test('the same words as Project::batch', () => {
		const core = readFileSync(join(SRC, '../../crates/kerf-core/src/project/batch.rs'), 'utf8');
		expect(core).toContain('"{} edits: {}{more}"');
		expect(core).toContain('", …"');
		expect(core).toContain('kinds.len() > 3');
	});
});

describe('runEdits', () => {
	test('runs the ops in order, each seeing what the ones before it answered', async () => {
		const ran: string[] = [];
		const table = probe(ran);
		const answers = await runEdits(
			[
				{ tool: 'split_at', args: { clip_id: 'c1', at: 1 } },
				{ tool: 'set_volume', args: { clip_id: '$0.right.id', volume: 0.5 } },
				{ tool: 'trim', args: { clip_id: '$0.left.id', source_in: 1 } }
			],
			table,
			host
		);
		expect(ran).toEqual(['volume {"clip_id":"r","volume":0.5}', 'trim {"clip_id":"l","source_in":1}']);
		expect(answers).toHaveLength(3);
		expect(answers[1]).toBe('ok');
	});

	test('a clip the cut does not have is refused by name, whichever argument holds it', async () => {
		const table = probe();
		for (const args of [{ clip_id: 'nope' }, { clip_ids: ['c1', 'nope'] }, { cuts: [{ clip_id: 'nope', at: 1 }] }, { moves: [{ clip_id: 'nope' }] }]) {
			await expect(runEdits([{ tool: 'set_volume', args }], table, host)).rejects.toThrow('op 0 (set_volume): clip not found: nope');
		}
		await expect(runEdits([{ tool: 'set_volume', args: { clip_id: 'c1', volume: 1 } }], table, host)).resolves.toEqual(['ok']);
	});

	test('an op with no arguments gets an empty set, and a failing one is named', async () => {
		const table: Record<string, HarnessEdit> = {
			...probe(),
			trim: {
				keys: [],
				run: async (a) => {
					if (Object.keys(a).length === 0) throw new Error('boom');
				}
			}
		};
		await expect(runEdits([{ tool: 'set_volume', args: { clip_id: 'c1', volume: 1 } }, { tool: 'trim' }], table, host)).rejects.toThrow(
			'op 1 (trim): boom'
		);
		await expect(runEdits([{ tool: 'set_volume', args: { clip_id: '$4.id', volume: 1 } }], table, host)).rejects.toThrow(
			'op 0 (set_volume): "$4.id" refers to op 4'
		);
	});

	test('an answer that is undefined is null, and an answer is a copy nobody else holds', async () => {
		const shared = { id: 'x' };
		const table: Record<string, HarnessEdit> = { trim: { keys: [], run: async () => undefined }, set_volume: { keys: [], run: async () => shared } };
		const answers = await runEdits([{ tool: 'trim' }, { tool: 'set_volume' }], table, host);
		expect(answers[0]).toBeNull();
		expect(answers[1]).toEqual(shared);
		expect(answers[1]).not.toBe(shared);
	});
});
