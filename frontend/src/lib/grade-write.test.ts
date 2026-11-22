import { describe, expect, test } from 'bun:test';
import { gradeWriter, type GradeHost } from './grade-write';
import type { Grade } from './types';

function host(initial: Record<string, Grade>) {
	const grades: Record<string, Grade> = { ...initial };
	const log: string[] = [];
	const errors: unknown[] = [];
	let hold: Promise<void> | null = null;
	const h: GradeHost = {
		gradeOf: (id) => grades[id],
		set: async (id, g) => {
			log.push(`set ${id} ${JSON.stringify(g)}`);
			if (hold) await hold;
			grades[id] = g;
		},
		report: (e) => errors.push(e)
	};
	return { h, grades, log, errors, slow: (p: Promise<void>) => (hold = p) };
}

const lut = (path: string) => ({ lut: { path, intensity: 1 } });

describe('writing the grade', () => {
	test('a change is built from the grade of the clip it is for, not of whichever is selected', async () => {
		const { h, grades } = host({ a: { curves: { master: [{ x: 0, y: 0.1 }, { x: 1, y: 1 }] } }, b: lut('/b.cube') });
		const write = gradeWriter(h);
		await write('a', (g) => ({ ...g, wheels: { shadows: { x: 0.1, y: 0, lightness: 0 }, midtones: { x: 0, y: 0, lightness: 0 }, highlights: { x: 0, y: 0, lightness: 0 } } }));
		expect(grades.a.curves).toBeDefined();
		expect(grades.a.wheels?.shadows.x).toBe(0.1);
		expect(grades.a.lut).toBeUndefined();
		expect(grades.b).toEqual(lut('/b.cube'));
	});

	test('two writes in a row each read the grade the one before left', async () => {
		const { h, grades, slow } = host({ a: {} });
		let release!: () => void;
		slow(new Promise<void>((r) => (release = r)));
		const write = gradeWriter(h);
		const first = write('a', (g) => ({ ...g, ...lut('/one.cube') }));
		const second = write('a', (g) => ({ ...g, curves: { red: [{ x: 0, y: 0.2 }, { x: 1, y: 1 }] } }));
		release();
		await Promise.all([first, second]);
		expect(grades.a.lut?.path).toBe('/one.cube');
		expect(grades.a.curves?.red).toBeDefined();
	});

	test('no change writes nothing, and a clip that is gone is not written', async () => {
		const { h, log, errors } = host({ a: {} });
		const write = gradeWriter(h);
		await write('a', () => null);
		await write('gone', (g) => ({ ...g, ...lut('/x.cube') }));
		expect(log).toEqual([]);
		expect(errors).toEqual([]);
	});

	test('a refusal is reported once, and the next write still goes', async () => {
		const { h, errors, grades } = host({ a: {} });
		const refusing: GradeHost = { ...h, set: async (id, g) => { if (g.lut?.path === '/bad.cube') throw new Error('no such LUT'); await h.set(id, g); } };
		const write = gradeWriter(refusing);
		await write('a', (g) => ({ ...g, ...lut('/bad.cube') }));
		await write('a', (g) => ({ ...g, ...lut('/good.cube') }));
		expect(errors.length).toBe(1);
		expect((errors[0] as Error).message).toBe('no such LUT');
		expect(grades.a.lut?.path).toBe('/good.cube');
	});

	test('a change that throws is reported and does not stop the queue', async () => {
		const { h, errors, grades } = host({ a: {} });
		const write = gradeWriter(h);
		await write('a', () => {
			throw new Error('bad change');
		});
		await write('a', (g) => ({ ...g, ...lut('/ok.cube') }));
		expect(errors.length).toBe(1);
		expect(grades.a.lut?.path).toBe('/ok.cube');
	});
});
