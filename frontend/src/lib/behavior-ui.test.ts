import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import {
	ADD_CHOICES,
	MAX_SEED,
	additions,
	bakeTip,
	behaviorFields,
	fieldValue,
	limitNotice,
	parseSeed,
	rowName,
	tileTip,
	withField,
	withSeed
} from './behavior-ui';
import { BEHAVIOR_KINDS, behaviorProblem, defaultBehavior, sanitized } from './behaviors';
import { entryFor, fromPos, parseEntry, readout, toPos } from './slider-row';
import { MAX_BEHAVIORS, type Behavior, type Clip } from './types';

const clip = (id: string, behaviors?: Behavior[]): Clip =>
	({ id, asset_id: 'a', source_in: 0, source_out: 5, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0, ...(behaviors ? { behaviors } : {}) }) as Clip;

describe('the rows of each kind', () => {
	test('every number of a kind has a row, in the range the core holds it to', () => {
		for (const kind of BEHAVIOR_KINDS) {
			const start = defaultBehavior(kind, 5);
			for (const f of behaviorFields(kind)) {
				const { min, max, limit } = f.spec;
				expect(min, `${kind} ${f.key}`).toBeLessThan(max);
				expect(f.spec.neutral!, `${kind} ${f.key} rests inside its travel`).toBeGreaterThanOrEqual(min);
				expect(f.spec.neutral!).toBeLessThanOrEqual(max);
				// The travel is inside what a typed entry may reach, which is inside what the core accepts.
				const lo = limit?.min ?? min;
				const hi = limit?.max ?? max;
				expect(lo).toBeLessThanOrEqual(min);
				expect(hi).toBeGreaterThanOrEqual(max);
				for (const v of [lo, hi, min, max, f.spec.neutral!]) {
					expect(behaviorProblem(withField(start, f.key, v)), `${kind} ${f.key} ${v}`).toBeUndefined();
				}
			}
		}
	});

	test('what a new behaviour starts as is where its amount and speed rows rest (a spin and a drift rest at a stop)', () => {
		for (const kind of ['sway', 'breathe', 'shake'] as const) {
			for (const f of behaviorFields(kind)) {
				if (f.key === 'phase') continue;
				expect(fieldValue(defaultBehavior(kind), f.key), `${kind} ${f.key}`).toBe(f.spec.neutral!);
			}
		}
		for (const kind of ['spin', 'drift'] as const) {
			const speed = behaviorFields(kind)[0];
			expect(speed.spec.neutral, `${kind} rests still`).toBe(0);
			expect(fieldValue(defaultBehavior(kind), 'speed')).not.toBe(0);
		}
	});

	test('a row reads in units a person has', () => {
		const sway = behaviorFields('sway');
		expect(readout(5, sway[0].spec)).toBe('5.0°');
		expect(readout(0.5, sway[1].spec)).toBe('0.50Hz');
		expect(readout(0.04, behaviorFields('breathe')[0].spec)).toBe('4%');
		expect(readout(0.1, behaviorFields('spin')[0].spec)).toBe('36°/s');
		expect(readout(-0.25, behaviorFields('spin')[0].spec)).toBe('-90°/s');
		expect(readout(0.02, behaviorFields('drift')[0].spec)).toBe('2.0%/s');
		expect(readout(0.01, behaviorFields('shake')[0].spec)).toBe('1.0%');
	});

	test('a typed entry goes past the slider’s travel up to the core’s limit, and no further', () => {
		const [amount, speed] = behaviorFields('sway');
		expect(parseEntry('90', amount.spec)).toBe(90);
		expect(parseEntry('500', amount.spec)).toBe(180);
		expect(parseEntry('12 Hz', speed.spec)).toBe(12);
		expect(parseEntry('12hz', speed.spec)).toBe(12);
		expect(parseEntry('99', speed.spec)).toBe(30);
		expect(parseEntry('36', behaviorFields('spin')[0].spec)).toBeCloseTo(0.1, 12);
		expect(parseEntry('-5', behaviorFields('spin')[0].spec)).toBeCloseTo(-5 / 360, 12);
		expect(parseEntry('300', behaviorFields('breathe')[0].spec)).toBe(0.9);
		expect(parseEntry('2', behaviorFields('drift')[0].spec)).toBeCloseTo(0.02, 12);
		expect(parseEntry('20', behaviorFields('shake')[0].spec)).toBe(0.2);
		expect(entryFor('abc', 5, amount.spec)).toEqual({ kind: 'revert' });
		expect(entryFor('7', 5, amount.spec)).toEqual({ kind: 'commit', value: 7 });
		expect(fromPos(toPos(5, amount.spec), amount.spec)).toBe(5);
	});

	test('a number is written without losing the rest of the behaviour', () => {
		const b: Behavior = { kind: 'shake', amount: 0.01, speed: 8, seed: 7 };
		expect(withField(b, 'speed', 12)).toEqual({ kind: 'shake', amount: 0.01, speed: 12, seed: 7, phase: 0 });
		expect(withField({ kind: 'drift', speed: 0.1 }, 'angle', 90)).toEqual({ kind: 'drift', speed: 0.1, angle: 90, phase: 0 });
		expect(withSeed(b, 99)).toEqual({ kind: 'shake', amount: 0.01, speed: 8, seed: 99, phase: 0 });
		expect(withSeed({ kind: 'spin', speed: 0.1 }, 99)).toEqual({ kind: 'spin', speed: 0.1 });
		expect(sanitized(withField(b, 'phase', -3)).phase).toBe(-3);
	});
});

describe('the seed field', () => {
	test('takes whole numbers up to a u32 and nothing else', () => {
		expect(parseSeed('7')).toBe(7);
		expect(parseSeed(' 4000000001 ')).toBe(4_000_000_001);
		expect(parseSeed(String(MAX_SEED))).toBe(MAX_SEED);
		expect(parseSeed(String(MAX_SEED + 1))).toBeUndefined();
		for (const bad of ['', 'x', '-1', '1.5', '1e3', '99999999999']) expect(parseSeed(bad), bad).toBeUndefined();
	});
});

describe('the words', () => {
	test('a kind that is on the clip twice is numbered', () => {
		const list = [defaultBehavior('sway'), defaultBehavior('shake'), defaultBehavior('sway')];
		expect(list.map((_, i) => rowName(list, i))).toEqual(['Sway 1', 'Shake', 'Sway 2']);
		expect(rowName([defaultBehavior('spin')], 0)).toBe('Spin');
	});

	test('the menu offers the five kinds with their starting numbers', () => {
		expect(ADD_CHOICES.map((c) => [c.label, c.meta])).toEqual([
			['Sway', '5° · 0.5Hz'],
			['Breathe', '4% · 0.3Hz'],
			['Spin', '36°/s'],
			['Drift', '2%/s'],
			['Shake', '1% · 8Hz']
		]);
	});

	test('bake says why it is off, and what it is', () => {
		expect(bakeTip(0)).toBe('Add a behaviour first');
		expect(bakeTip(2)).toContain('keyframes');
		expect(bakeTip(2)).toContain('split it');
	});

	test('a tile says what it does and where it starts, or why it is off', () => {
		expect(tileTip('sway', 'Rocks it', null)).toEqual({ text: 'Sway', detail: 'Rocks it. Starts at 5° · 0.5Hz; tune it in the Inspector.' });
		expect(tileTip('sway', 'Rocks it', 'Select a clip on the timeline to give it a motion.').detail).toBe('Select a clip on the timeline to give it a motion.');
	});
});

describe('adding to the selected clips', () => {
	test('clips with no behaviours are one edit, each given the new one last', () => {
		const { full, groups } = additions([clip('a'), clip('b')], 'sway', 0);
		expect(full).toEqual([]);
		expect(groups).toEqual([{ ids: ['a', 'b'], list: [{ kind: 'sway', amount: 5, speed: 0.5, phase: 0 }] }]);
	});

	test('clips that carry different lists are told apart, and keep what they have', () => {
		const sway = defaultBehavior('sway');
		const { groups } = additions([clip('a', [sway]), clip('b'), clip('c', [{ ...sway }]), clip('d', [defaultBehavior('spin')])], 'shake', 12);
		expect(groups.map((g) => g.ids)).toEqual([['a', 'c'], ['b'], ['d']]);
		expect(groups[0].list).toEqual([sway, { kind: 'shake', amount: 0.01, speed: 8, seed: 12, phase: 0 }]);
		expect(groups[1].list.length).toBe(1);
	});

	test('a clip at the limit refuses the lot, and the notice says how many', () => {
		const eight = Array.from({ length: MAX_BEHAVIORS }, () => defaultBehavior('spin'));
		const { full } = additions([clip('a'), clip('b', eight), clip('c', eight)], 'sway', 0);
		expect(full.map((c) => c.id)).toEqual(['b', 'c']);
		expect(limitNotice(full, 3)).toBe('2 of the 3 selected clips already have 8 behaviours — remove one first');
		expect(limitNotice([full[0]], 1)).toBe('This clip already has 8 behaviours — remove one first');
	});
});

describe('the section and the tab', () => {
	const section = readFileSync(new URL('./components/editor/BehaviorsSection.svelte', import.meta.url), 'utf8');
	const tab = readFileSync(new URL('./components/editor/MotionTab.svelte', import.meta.url), 'utf8');
	const inspector = readFileSync(new URL('./components/editor/Inspector.svelte', import.meta.url), 'utf8');

	test('the Inspector shows the section for a picture, after the transform', () => {
		expect(inspector).toContain('<BehaviorsSection {clip} />');
		expect(inspector.indexOf('<BehaviorsSection')).toBeGreaterThan(inspector.indexOf('InspectorSection title="Transform"'));
		expect(inspector.indexOf('<BehaviorsSection')).toBeLessThan(inspector.indexOf('InspectorSection title="Framing"'));
	});

	test('every write is one backend edit, from a slider’s release, a typed entry or a button', () => {
		expect(section).toContain('editor.setBehaviors([id], next)');
		expect(section).toContain('editor.bakeBehaviors([id])');
		expect(section).toContain('<SliderRow');
		expect(section).toContain('onCommit={(v, row) => setField(row, f.key, v)}');
		// The summary names the behaviours; the refusal of a bake is shown, not toasted away.
		expect(section).toContain('summary={behaviorsSummary(list)}');
		expect(section).toContain('role="alert"');
	});

	test('names its controls and speaks only in tokens', () => {
		for (const name of ['aria-label="{name} behaviour"', 'title="Remove {name.toLowerCase()}"', 'aria-label="{name} seed"', 'title="Another shake"']) {
			expect(section).toContain(name);
		}
		for (const source of [section, tab]) expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgba?\(/);
		expect(section).not.toMatch(/[⌘⌃⌥⇧]/);
	});

	test('the Motion tab adds a behaviour in one click, through the same op', () => {
		expect(tab).toContain('addBehavior(c.kind)');
		expect(tab).toContain('aria-label="Behaviours"');
		expect(tab).toContain('data-testid="behavior-{c.kind}"');
	});
});
