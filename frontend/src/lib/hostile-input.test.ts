import { describe, expect, test } from 'bun:test';
import { parseAss, parseCaptions, parseSrt, parseTimestamp } from './caption-import';
import { KEY_PRESETS, parseKeyOverrides, presetOverrides } from './keymap';
import { sanitizeLayout } from './layout';
import { parseLaunchRequest } from './launch';
import { PRESETS, parseTheme, themeJson } from './theme';
import { parseTimecode, timecodeRate } from './timecode';
import { parseHeights } from './track-heights';
import { parsePrefs } from './voiceover';
import { defaultWorkspaces, readWorkspaces } from './workspaces';

// Hostile input for what the page reads from a file or from storage: caption files, the settings
// file's opaque parts (keybindings, theme, layout, workspaces), localStorage, typed timecode. The
// twin of kerf-core's `hostile` tests: a fixed seed, every prefix, bounded rounds of byte flips,
// insertions, deletions and duplicated spans, numbers pushed to their edges — and no case throws.

const SEED = 0x9e3779b9;
const ROUNDS = 1500;
const INTERESTING = '0123456789:;.,-+e/\\"\'{}[]<>\n\r\t \u0000\u001bÿ\ud800';
const EXTREMES = ['0', '-1', '-0.0', '0.5', '4294967295', '9007199254740993', '1e308', '-1e308', '1e-308', 'NaN', 'Infinity'];

function xorshift(seed: number) {
	let x = seed >>> 0 || 1;
	return (n: number) => {
		x ^= x << 13;
		x >>>= 0;
		x ^= x >>> 17;
		x ^= x << 5;
		x >>>= 0;
		return n > 0 ? x % n : 0;
	};
}

function mutants(input: string, seed: number, rounds = ROUNDS): string[] {
	const out: string[] = [];
	const step = Math.max(1, Math.ceil(input.length / 512));
	for (let n = 0; n < input.length; n += step) out.push(input.slice(0, n));
	const below = xorshift(seed);
	const char = () => (below(2) === 0 ? INTERESTING[below(INTERESTING.length)] : String.fromCharCode(below(0x10000)));
	for (let round = 0; round < rounds; round++) {
		let v = input;
		for (let k = 0; k < 1 + below(4); k++) {
			const at = below(v.length + 1);
			switch (round % 5) {
				case 0:
					v = v.slice(0, at) + String.fromCharCode((v.charCodeAt(at) || 0) ^ (1 << below(16))) + v.slice(at + 1);
					break;
				case 1:
					v = v.slice(0, at) + char() + v.slice(at);
					break;
				case 2:
					v = v.slice(0, at) + v.slice(at + 1 + below(16));
					break;
				case 3: {
					const span = v.slice(at, at + 1 + below(64));
					const to = below(v.length + 1);
					v = v.slice(0, to) + span + v.slice(to);
					break;
				}
				default:
					v = v.slice(0, at) + Array.from({ length: below(32) }, char).join('');
			}
		}
		out.push(v);
	}
	for (const m of input.matchAll(/[-+.eE0-9]*[0-9][-+.eE0-9]*/g)) {
		if (out.length > rounds + 6000) break;
		for (const x of EXTREMES) out.push(input.slice(0, m.index) + x + input.slice(m.index + m[0].length));
	}
	return out;
}

/** The cases `f` breaks on: a `TypeError`, a `RangeError` or anything else that is not a parser's own
 *  `Error` (`parseCaptions` refuses an unusable ASS `Format:` line with one, as kerf-core returns an
 *  `Err`). */
function throwing(inputs: string[], f: (s: string) => unknown): string[] {
	const bad: string[] = [];
	inputs.forEach((input, i) => {
		for (const s of mutants(input, SEED ^ i)) {
			try {
				f(s);
			} catch (e) {
				if (!(e instanceof Error) || e.constructor !== Error) bad.push(s);
			}
		}
	});
	return bad.slice(0, 3);
}

/** Feeds `f` what `JSON.parse` makes of each case — the shape a stored value comes back in. */
const asJson = (f: (v: unknown) => unknown) => (s: string) => {
	let v: unknown;
	try {
		v = JSON.parse(s);
	} catch {
		return;
	}
	f(v);
};

const SRT = '﻿1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i> {\\an8}<font color="#ff0000">world</font>\r\n\r\n2\n00:00:03.000 --> 00:00:04.000 X1:10\nsecond\nline\n\n99\n01:02:03,4 --> 01:02:05,45\nlast\n';
const ASS =
	'[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.50,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nworld, again\\h!\nComment: 0,0:00:03.00,0:00:04.00,Default,,0,0,0,,note\nDialogue: 1,1:02:03.45,1:02:05.00,Default,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}last\n';

describe('hostile input never throws', () => {
	test('caption files', () => {
		expect(
			throwing([SRT, ASS], (s) => {
				parseSrt(s);
				parseAss(s);
				parseCaptions(s);
				parseCaptions(s, 'srt');
				parseCaptions(s, 'ass');
				for (const token of s.split(/\s+/).slice(0, 8)) parseTimestamp(token);
			})
		).toEqual([]);
	});

	test('stored keybindings', () => {
		const stored = KEY_PRESETS.map((p) => JSON.stringify(presetOverrides(p, 'other')));
		expect(
			throwing(
				stored,
				asJson((v) => {
					parseKeyOverrides(v, 'mac');
					parseKeyOverrides(v, 'other');
				})
			)
		).toEqual([]);
	});

	test('a theme file, a layout and the workspaces', () => {
		const workspaces = JSON.stringify({
			...defaultWorkspaces(),
			layouts: { edit: { grid: { root: { type: 'branch', data: [{ type: 'leaf', data: { views: ['preview'], activeView: 'preview', id: '1' }, size: 300 }] }, width: 800, height: 600, orientation: 'HORIZONTAL' }, panels: { preview: { id: 'preview', contentComponent: 'preview', title: 'Preview' } }, activeGroup: '1' } }
		});
		expect(
			throwing(
				[themeJson(PRESETS['kerf-dark']), workspaces],
				asJson((v) => {
					parseTheme(v);
					sanitizeLayout(v);
					sanitizeLayout((v as { layouts?: { edit?: unknown } } | null)?.layouts?.edit);
					readWorkspaces(v, v);
					parseLaunchRequest(v);
				})
			)
		).toEqual([]);
	});

	test('what the page keeps in localStorage', () => {
		const heights = JSON.stringify({ all: 'medium', tracks: { a: 'large', b: 80 } });
		const prefs = JSON.stringify({ voice: 'af_heart', speed: 1.1, place: 'playhead' });
		expect(
			throwing([heights, prefs], (s) => {
				parseHeights(s);
				parsePrefs(s);
			})
		).toEqual([]);
	});

	test('typed timecode', () => {
		for (const fps of [25, 30000 / 1001, 60000 / 1001]) {
			const r = timecodeRate(fps);
			expect(
				throwing(['01:02:03:04', '+1.00', '-00;01;00;02', '1000'], (s) => {
					const f = parseTimecode(s, r, 100);
					if (f !== null && !(Number.isSafeInteger(f) && f >= 0)) throw new RangeError(`frame ${f}`);
				})
			).toEqual([]);
		}
	});
});
