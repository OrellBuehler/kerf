import { describe, expect, test } from 'bun:test';
import { checkBandRequest, checkComposeRequest } from './music-wire';
import { composeRequest, defaultForm, formFromScore } from './compose-form';

const score = () => ({
	bpm: 120,
	key: 'Am',
	style: 'pop',
	sections: [{ kind: 'intro', bars: 4, chords: ['Am'], energy: 0.3 }],
	hits: [{ at_s: 1, kind: 'impact' }],
	seed: 3,
	instruments: { bass: 33 }
});

const refusal = (request: unknown) => {
	try {
		checkComposeRequest(request);
		return null;
	} catch (e) {
		return (e as Error).message;
	}
};

describe('the request the engine takes', () => {
	test('is accepted with every field it has', () => {
		expect(refusal({ score: score(), track_id: 't', at: 2, align: { to: 'cuts', track_id: 't', max_tempo_change: 0.1 }, replace_clip_id: 'c' })).toBeNull();
		expect(refusal({ score: { bpm: 100, key: 'C', style: 'pop', sections: [], seed: 0 } })).toBeNull();
	});

	test('names a misspelled key and lists the ones that exist, as serde does', () => {
		expect(refusal({ score: score(), replace_clip: 'c' })).toBe(
			'invalid args `request` for command `compose_music`: unknown field `replace_clip`, expected one of `score`, `track_id`, `at`, `align`, `replace_clip_id`'
		);
		expect(refusal({ score: score(), align: { to: 'cuts', tempo: 1 } })).toContain(
			'unknown field `tempo`, expected one of `to`, `track_id`, `max_tempo_change`'
		);
	});

	test('reaches into the score, its sections, hits and instruments', () => {
		const withScore = (change: (s: ReturnType<typeof score>) => void) => {
			const s = score();
			change(s);
			return refusal({ score: s });
		};
		expect(withScore((s) => Object.assign(s, { bmp: 1 }))).toContain('unknown field `bmp`, expected one of `bpm`, `key`, `style`, `sections`, `hits`, `seed`, `instruments`');
		expect(withScore((s) => Object.assign(s.sections[0], { bar: 4 }))).toContain('unknown field `bar`, expected one of `kind`, `bars`, `chords`, `energy`');
		expect(withScore((s) => Object.assign(s.hits[0], { at: 4 }))).toContain('unknown field `at`, expected `at_s` or `kind`');
		expect(withScore((s) => Object.assign(s.instruments, { strings: 4 }))).toContain('unknown field `strings`, expected one of `drums`, `bass`, `pad`, `keys`, `lead`');
	});

	test('reports the first key it meets, in the order they were written', () => {
		expect(refusal({ zzz: 1, aaa: 2, score: score() })).toContain('unknown field `zzz`');
		expect(refusal({ score: { ...score(), nope: 1 }, also: 2 })).toContain('unknown field `nope`');
		expect(refusal({ also: 2, score: { ...score(), nope: 1 } })).toContain('unknown field `also`');
	});

	test('leaves a value that is not an object to the engine’s other checks', () => {
		expect(refusal({ score: 'x' })).toBeNull();
		expect(refusal(null)).toBeNull();
		expect(refusal({ score: { ...score(), sections: 'x' } })).toBeNull();
	});
});

describe('band balance requests', () => {
	const band = (request: unknown) => {
		try {
			checkBandRequest(request);
			return null;
		} catch (e) {
			return (e as Error).message;
		}
	};

	test('take the four fields and the windows’ three', () => {
		expect(band({ asset_id: 'a', sections: [{ start: 0, end: 1, label: 'x' }] })).toBeNull();
		expect(band({ range: { start: 0, end: 1 } })).toBeNull();
	});

	test('refuse the rest, naming them', () => {
		expect(band({ clip: 'c' })).toBe(
			'invalid args `request` for command `get_band_balance`: unknown field `clip`, expected one of `asset_id`, `clip_id`, `range`, `sections`'
		);
		expect(band({ asset_id: 'a', sections: [{ start: 0, end: 1, name: 'x' }] })).toContain('unknown field `name`, expected one of `start`, `end`, `label`');
	});
});

describe('what the dialog sends', () => {
	test('is only keys the engine has', () => {
		const choices = { tracks: [{ id: 'v1', name: 'V1', cuts: 3 }], markers: 2 };
		const plain = composeRequest(defaultForm(), choices);
		expect(refusal(plain)).toBeNull();
		for (const align of [{ to: 'cuts' }, { to: 'markers' }] as const) {
			const f = { ...defaultForm(), align: true, alignTo: align.to, hits: [{ at_s: 3, kind: 'riser' as const }], instruments: { lead: 80 } };
			expect(refusal(composeRequest(f, choices, { at: 4 }))).toBeNull();
			expect(refusal(composeRequest(f, choices, { replaceClipId: 'c1' }))).toBeNull();
		}
	});

	test('and so is a score read back from an asset and edited', () => {
		const f = formFromScore({ bpm: 90, key: 'Dm', style: 'lofi', sections: [{ kind: 'verse', bars: 4, chords: ['Dm'], energy: 0.4 }], hits: [{ at_s: 2, kind: 'impact' }], seed: 5, instruments: { bass: 38 } });
		expect(refusal(composeRequest(f, { tracks: [], markers: 0 }))).toBeNull();
	});
});
