import { describe, expect, test } from 'bun:test';
import { singleFlight } from './single-flight';

const gate = () => {
	let open!: () => void;
	const opened = new Promise<void>((r) => (open = r));
	return { opened, open };
};

describe('singleFlight', () => {
	test('a lone request runs once', async () => {
		let runs = 0;
		const w = singleFlight(async () => void runs++);
		w.request();
		await w.idle();
		expect(runs).toBe(1);
	});

	test('never two at once, however many are asked for', async () => {
		let live = 0;
		let peak = 0;
		let runs = 0;
		const gates = [gate(), gate(), gate()];
		const w = singleFlight(async () => {
			live++;
			peak = Math.max(peak, live);
			await gates[runs++].opened;
			live--;
		});
		w.request();
		w.request();
		w.request();
		w.request();
		expect(live).toBe(1);
		gates[0].open();
		await new Promise((r) => setTimeout(r, 0));
		gates[1].open();
		await w.idle();
		expect(peak).toBe(1);
	});

	test('requests made during a write collapse into one follow-up', async () => {
		let runs = 0;
		const first = gate();
		const w = singleFlight(async () => {
			runs++;
			if (runs === 1) await first.opened;
		});
		w.request();
		w.request();
		w.request();
		w.request();
		first.open();
		await w.idle();
		expect(runs).toBe(2);
	});

	test('the follow-up sees the newest state, because it reads it when it starts', async () => {
		let state = 'one';
		const written: string[] = [];
		const first = gate();
		const w = singleFlight(async () => {
			const snapshot = state;
			if (written.length === 0) await first.opened;
			written.push(snapshot);
		});
		w.request();
		state = 'two';
		w.request();
		state = 'three';
		w.request();
		first.open();
		await w.idle();
		// The first write carried what was current when it began; the one after it
		// carries the last change, and the middle one is not written at all.
		expect(written).toEqual(['one', 'three']);
	});

	test('a failing write does not wedge the queue', async () => {
		let runs = 0;
		const w = singleFlight(async () => {
			runs++;
			if (runs === 1) throw new Error('disk full');
		});
		w.request();
		await w.idle();
		w.request();
		await w.idle();
		expect(runs).toBe(2);
	});

	test('a request after everything settled starts a fresh write', async () => {
		let runs = 0;
		const w = singleFlight(async () => void runs++);
		w.request();
		await w.idle();
		w.request();
		await w.idle();
		w.request();
		await w.idle();
		expect(runs).toBe(3);
	});
});
