import { describe, expect, test } from 'bun:test';
import { sampleFilmstrip } from './sample-filmstrip';
import { frameAt, locate, planFilmstrip, stripGeometry } from './filmstrip-geometry';
import { generateVoiceover, getFilmstrip, listAssets } from './api';
import type { StreamInfo } from './types';

// The harness's stand-in for `get_filmstrip` has to be the strip the backend
// would send — same geometry, same transport — or the timeline would be built
// against a shape the desktop app never produces.

const video = (width: number, height: number, image = false): StreamInfo => ({
	index: 0,
	kind: 'video',
	codec: image ? 'png' : 'h264',
	width,
	height,
	image
});

const decode = (url: string) => decodeURIComponent(url.slice(url.indexOf(',') + 1));

describe('sampleFilmstrip', () => {
	test('has the geometry the plan describes and a generated image per sheet', () => {
		const asset = { id: 'clip-a', duration: 60, streams: [video(1920, 1080)] };
		const strip = sampleFilmstrip(asset);
		const { sheets, ...geometry } = strip;
		const { sheets: expectedSheets, ...expectedGeometry } = stripGeometry(planFilmstrip(asset));
		expect(geometry).toEqual(expectedGeometry);
		expect(sheets).toHaveLength(expectedSheets.length);
		expect(strip.frames).toBe(120);
		for (const [i, sheet] of sheets.entries()) {
			const { data_url, ...rest } = sheet;
			expect(rest).toEqual(expectedSheets[i]);
			expect(data_url.startsWith('data:image/svg+xml;utf8,')).toBe(true);
			// The image is the size the sheet says it is.
			const svg = decode(data_url);
			expect(svg).toContain(`width="${sheet.width}" height="${sheet.height}"`);
		}
	});

	test('every thumbnail is drawn once, at its own x, and the padding is not', () => {
		// 49 thumbnails: 25 + 24 on two sheets, the last with one thumbnail of padding.
		const asset = { id: 'clip-b', duration: 24.5, streams: [video(1920, 1080)] };
		const strip = sampleFilmstrip(asset);
		expect(strip.columns).toBe(25);
		const last = decode(strip.sheets[1].data_url);
		for (let i = 0; i < strip.sheets[1].count; i++) {
			expect(last).toContain(`>#${25 + i}</text>`);
		}
		expect(last).not.toContain('>#49</text>');
		const lastX = (strip.sheets[1].count - 1) * strip.frame_width;
		expect(last).toContain(`<rect x="${lastX}" y="0" width="${strip.frame_width}"`);
		expect(last).not.toContain(`<rect x="${lastX + strip.frame_width}" y="0" width="${strip.frame_width}"`);
		// What a caller computes for a time lands on that thumbnail's label: 20.3 s is
		// the thumbnail at 20.5 s, the 17th on the second sheet.
		const k = frameAt(strip, 20.3);
		const hit = locate(strip, k)!;
		expect([k, hit.sheet.first_frame, hit.x]).toEqual([41, 25, 16 * strip.frame_width]);
		const drawn = decode(hit.sheet.data_url);
		expect(drawn).toMatch(new RegExp(`<text x="${hit.x + 4}"[^>]*>0:20\\.5</text>`));
		expect(drawn).toMatch(new RegExp(`<text x="${hit.x + 4}"[^>]*>#41</text>`));
	});

	test('is deterministic, and one asset does not look like another', () => {
		const a = { id: 'clip-a', duration: 30, streams: [video(1280, 720)] };
		expect(sampleFilmstrip(a)).toEqual(sampleFilmstrip({ ...a }));
		const other = sampleFilmstrip({ ...a, id: 'clip-z' });
		expect(other.sheets[0].data_url).not.toBe(sampleFilmstrip(a).sheets[0].data_url);
	});

	test('a still is one thumbnail, a portrait clip a narrow one', () => {
		const still = sampleFilmstrip({ id: 's', duration: 5, streams: [video(800, 600, true)] });
		expect([still.frames, still.frame_width, still.sheets.length, still.sheets[0].count]).toEqual([1, 128, 1, 1]);
		expect(decode(still.sheets[0].data_url)).toContain('still');
		const portrait = sampleFilmstrip({ id: 'p', duration: 10, streams: [video(1080, 1920)] });
		expect(portrait.frame_width).toBe(54);
	});

	test('an asset with no video stream is refused as the backend refuses it', () => {
		const voice = { id: 'v', duration: 10, streams: [{ index: 0, kind: 'audio' as const, codec: 'aac' }] };
		expect(() => sampleFilmstrip(voice)).toThrow(/invalid argument: asset v has no video stream/);
	});
});

describe('getFilmstrip (browser harness)', () => {
	test('answers for the sample footage with the contract of the backend', async () => {
		const [interview, broll] = await listAssets();
		const strip = await getFilmstrip(interview.id);
		expect(strip.interval).toBe(0.5);
		expect(strip.frames).toBe(240);
		expect(strip.frame_width).toBe(170);
		expect(strip.frame_height).toBe(96);
		expect(strip.sheets.reduce((n, s) => n + s.count, 0)).toBe(strip.frames);
		expect((await getFilmstrip(broll.id)).frames).toBe(90);
		expect(await getFilmstrip(interview.id)).toEqual(strip);
	});

	test('rejects an unknown asset, and an asset with no picture', async () => {
		await expect(getFilmstrip('00000000-0000-0000-0000-000000000000')).rejects.toThrow(/asset not found/);
		const { asset } = await generateVoiceover({ text: 'Hello there.' });
		await expect(getFilmstrip(asset.id)).rejects.toThrow(/has no video stream/);
	}, 20_000);
});
