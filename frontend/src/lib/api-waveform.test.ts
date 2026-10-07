import { describe, expect, test } from 'bun:test';
import { generateVoiceover, getWaveformRange, listAssets } from './api';

// Under bun there is no Tauri, so these drive the browser harness's answer to
// `get_waveform_range` — which has to honour the same contract as the backend's,
// or the timeline would be built against behaviour the desktop app never shows.

async function sample() {
	const [interview, broll] = await listAssets();
	return { interview, broll };
}

describe('getWaveformRange (browser harness)', () => {
	test('a stereo asset answers with two lanes of the requested buckets', async () => {
		const { interview } = await sample();
		const r = await getWaveformRange(interview.id, 0, 12.5, 250);
		expect(r.channels).toBe(2);
		expect(r.buckets).toBe(250);
		expect(r.duration).toBe(interview.duration);
		expect(r.min).toHaveLength(2);
		expect(r.max[1]).toHaveLength(250);
	});

	test('a mono asset answers with one lane', async () => {
		// The harness's only one-channel audio is a generated voiceover.
		const { asset } = await generateVoiceover({ text: 'Hello there.' });
		expect(asset.streams.find((s) => s.kind === 'audio')?.channels).toBe(1);
		const r = await getWaveformRange(asset.id, 0, asset.duration, 60);
		expect(r.channels).toBe(1);
		expect(r.min).toHaveLength(1);
		expect(r.max[0]).toHaveLength(60);
		expect(r.duration).toBe(asset.duration);
	}, 20_000);

	test('the same window is the same answer, call after call', async () => {
		const { interview } = await sample();
		expect(await getWaveformRange(interview.id, 30, 42, 400)).toEqual(await getWaveformRange(interview.id, 30, 42, 400));
	});

	test('the silence the analysis found is quiet, and past the end is zero', async () => {
		const { interview } = await sample();
		// The sample analysis marks 12.5–14 s of the interview silent.
		const quiet = await getWaveformRange(interview.id, 12.6, 13.9, 26);
		expect(quiet.max[0].every((v) => v > 0 && v < 0.02)).toBe(true);
		const past = await getWaveformRange(interview.id, interview.duration, interview.duration + 4, 40);
		expect(past.max.flat().every((v) => v === 0)).toBe(true);
	});

	test('a fractional bucket count is rounded, as the backend takes a whole number', async () => {
		const { interview } = await sample();
		expect((await getWaveformRange(interview.id, 0, 5, 99.6)).buckets).toBe(100);
		expect((await getWaveformRange(interview.id, 0, 5, -3)).buckets).toBe(0);
	});

	test('footage with no audio stream is refused, as is an unknown asset', async () => {
		const { broll } = await sample();
		await expect(getWaveformRange(broll.id, 0, 5, 50)).rejects.toThrow('no audio stream');
		await expect(getWaveformRange('00000000-0000-0000-0000-000000000000', 0, 5, 50)).rejects.toThrow('asset not found');
	});
});
