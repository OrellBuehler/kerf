import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';
import {
	BATCHABLE_TOOLS,
	MAX_EDITS,
	MAX_LABEL_CHARS,
	MAX_REFERENCE_BYTES,
	batchLabel,
	capLabel,
	checkEdits,
	compactAnswer,
	harnessEdits,
	literal,
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

	test('the caps on references and on a name are the backend’s', () => {
		const rust = (name: string) => {
			const text = BATCH_RS.match(new RegExp('const ' + name + ': usize = ([^;]+);'))?.[1] ?? '';
			return text === '1 << 20' ? 1 << 20 : Number(text);
		};
		expect(MAX_REFERENCE_BYTES).toBe(rust('MAX_REFERENCE_BYTES'));
		expect(MAX_LABEL_CHARS).toBe(rust('MAX_LABEL_CHARS'));
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

	test('what the backend’s deserializer refuses is refused: `args` that is no map, a field that is neither tool nor args', () => {
		expect(run([{ tool: 'set_volume', args: null as never }])).toThrow('op 0 (set_volume): invalid type for `args`: expected a map');
		expect(run([{ tool: 'set_volume', args: [] as never }])).toThrow('expected a map');
		expect(run([{ tool: 'set_volume', arg: {} } as never])).toThrow('op 0 (set_volume): unknown field `arg`, expected `tool` or `args`');
		expect(run([{ tool: 'set_volume' }, { tool: 'set_volume', args: {} }])).not.toThrow();
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

	// The same vectors are in `mcp/batch.rs` (`a_text_that_starts_with_a_dollar_is_a_reference_unless_it_is_escaped`): a
	// user's text that starts with `$` means the same whichever side runs the batch.
	const SAME_AS_RUST: [string, string][] = [
		['$$100', '$100'],
		['$$0', '$0'],
		['$$1.5M', '$1.5M'],
		['$$', '$'],
		['$$$5', '$$5'],
		['$$0.id', '$0.id'],
		['$5 shot', '$5 shot'],
		['$abc', '$abc'],
		['$0. x', '$0. x'],
		['$+0', '$+0'],
		['$-1', '$-1'],
		['a$0', 'a$0'],
		['$', '$'],
		['100$', '100$']
	];

	test('read the way the backend reads them', () => {
		const here = [{ id: 'x1' }];
		for (const [given, read] of SAME_AS_RUST) expect(resolve(given, here), given).toBe(read);
		expect(resolve('$0.id', here)).toBe('x1');
		for (const refused of ['$1', '$0.nothing', '$0.id.deeper', '$00.0']) expect(() => resolve(refused, here), refused).toThrow();
	});

	test('a text a user typed goes through `literal` and comes out as it was typed', () => {
		const here = [{ id: 'x1' }];
		for (const typed of ['$100', '$0', '$1.5M', '$', '$$5', '$0.id', '100$', '', 'plain', '$abc', '$ 5']) {
			expect(resolve(literal(typed), here), typed).toBe(typed);
			expect(resolve({ name: literal(typed) }, here), typed).toEqual({ name: typed });
		}
		expect(literal('$100')).toBe('$$100');
		expect(literal('plain')).toBe('plain');
	});

	test('only the keys an answer holds can be named, not what every object inherits', () => {
		const here = [{ id: 'x1', list: [1] }];
		for (const path of ['constructor', '__proto__', 'toString', 'hasOwnProperty', 'list.length', 'list.constructor']) {
			expect(() => resolve(`$0.${path}`, here), path).toThrow('has nothing at');
		}
	});

	test('the answers a batch copies are capped in all, whichever op copies them', () => {
		const big = { keys: Array.from({ length: 1000 }, (_, i) => ({ time: i / 100, value: 0.5 })) };
		const here = [big];
		const spent = { bytes: 0 };
		let copies = 0;
		expect(() => {
			for (; copies < 100; copies++) resolve('$0.keys', here, spent);
		}).toThrow(`copy more than ${MAX_REFERENCE_BYTES} bytes of earlier answers ("$0.keys" is one): refer to less, or send fewer edits`);
		expect(copies).toBeGreaterThan(20);
		expect(copies).toBeLessThan(60);
		expect(resolve('$0.keys', here, { bytes: 0 })).toEqual(big.keys);
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
	test('a name is trimmed and cut to two hundred characters, as the backend cuts it', () => {
		expect(MAX_LABEL_CHARS).toBe(200);
		expect(capLabel('  Tidy the open  ')).toBe('Tidy the open');
		expect(capLabel('a'.repeat(200))).toBe('a'.repeat(200));
		expect(capLabel('a'.repeat(201))).toBe('a'.repeat(199) + '…');
		expect(capLabel('é'.repeat(250))).toBe('é'.repeat(199) + '…');
		expect(capLabel('  ' + '😀'.repeat(201) + '  ')).toBe('😀'.repeat(199) + '…');
		expect(capLabel('   ')).toBe('');
		expect(capLabel(null)).toBe('');
		expect(capLabel(undefined)).toBe('');
	});

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
