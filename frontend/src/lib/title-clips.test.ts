import { beforeEach, describe, expect, test } from 'bun:test';
import { addTitleClip, applyTemplateClip, getTimeline, revertTo, saveTemplate, setSpeed, setTemplateValues, updateTitleClip } from './api';
import { resetDevTemplates, SAMPLE_TEMPLATE_ID } from './template-dev';
import { findTitle, MAX_TITLE_SECONDS, placeTitleClip, titleClip, titleName, titleOf } from './title-clips';
import { sourceLimits } from './trim-tools';
import { GENERATED_ASSET } from './types';
import type { Clip, Timeline } from './types';

// The pure side mirrors kerf-core's `project/title_clips.rs` (where a title clip goes, what it is called); the harness
// half drives the same contract through `api.ts` under bun.

const ART =
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 90"><rect id="box" width="40" height="20" fill="#e63946"/></svg>';

const shot = (id: string, start: number, end: number): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: end - start,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});

const cut = (): Timeline => ({
	tracks: [
		{ id: 'v1', kind: 'video', name: 'V1', clips: [shot('s', 0, 4)] },
		{ id: 'a1', kind: 'audio', name: 'A1', clips: [] }
	]
});

describe('placing a title clip', () => {
	test('lands on the lowest video track free over its span', () => {
		const tl = cut();
		const end = titleClip({ name: 'End', svg: ART }, 4, 2);
		expect(placeTitleClip(tl, end)).toBe('v1');
		expect(end.asset_id).toBe(GENERATED_ASSET);
		expect(tl.tracks[0].clips.map((c) => c.id)).toEqual(['s', end.id]);
	});

	test('over footage goes on a new video track on top, named after the count', () => {
		const tl = cut();
		const over = titleClip({ name: 'Lower third', svg: ART }, 1, 2);
		const id = placeTitleClip(tl, over);
		expect(tl.tracks.map((t) => t.name)).toEqual(['V1', 'V2', 'A1']);
		expect(tl.tracks[1].id).toBe(id);
	});

	test('a locked track is passed over, and an explicit one must be video and free', () => {
		const tl = cut();
		tl.tracks[0].locked = true;
		placeTitleClip(tl, titleClip({ name: '', svg: ART }, 9, 1));
		expect(tl.tracks[1].kind).toBe('video');
		expect(() => placeTitleClip(tl, titleClip({ name: '', svg: ART }, 0, 1), 'a1')).toThrow('not a video track');
		const unlocked = cut();
		expect(() => placeTitleClip(unlocked, titleClip({ name: '', svg: ART }, 3, 1), 'v1')).toThrow('overlap');
	});

	test('a title reaches as far as its document may run, as the engine says', () => {
		expect(sourceLimits([]).get(GENERATED_ASSET)).toBe(MAX_TITLE_SECONDS);
	});

	test('a title is named by itself, its template, else a placeholder; and found floating or on a track', () => {
		expect(titleName({ name: ' End ', template: null })).toBe('End');
		expect(titleName({ name: '', template: { template_id: 'x', name: 'Card', controls: [], values: {} } })).toBe('Card');
		expect(titleName({ name: '' })).toBe('SVG');
		const tl = cut();
		const c = titleClip({ name: 'End', svg: ART }, 4, 2);
		placeTitleClip(tl, c);
		expect(findTitle(tl, c.id)).toMatchObject({ name: 'End', onTrack: true, duration: 2, svg: ART });
		expect(findTitle(tl, 's')).toBeUndefined();
	});
});

describe('title clips in the browser harness', () => {
	beforeEach(async () => {
		await revertTo(0);
		resetDevTemplates();
	});

	const clipOf = (tl: Timeline, id: string) => tl.tracks.flatMap((t) => t.clips).find((c) => c.id === id)!;

	test('a template goes on a track with its controls, and its values change in one edit', async () => {
		const edit = await applyTemplateClip(SAMPLE_TEMPLATE_ID, {}, 30);
		expect(edit.overlay_id).toBeUndefined();
		const clip = clipOf(edit.timeline, edit.clip_id!);
		const title = titleOf(clip)!;
		expect(title.template?.template_id).toBe(SAMPLE_TEMPLATE_ID);
		const control = title.template!.controls.find((c) => c.kind === 'text')!;
		const changed = await setTemplateValues(clip.id, { [control.id]: 'Bye' });
		expect(changed.clip_id).toBe(clip.id);
		expect(titleOf(clipOf(changed.timeline, clip.id))!.template!.values[control.id]).toBe('Bye');
		const saved = await saveTemplate(clip.id, '', []);
		expect(saved.info.name).toBe(title.name);
	});

	test('custom artwork on a track can be renamed and replaced, which ends a template', async () => {
		const added = await addTitleClip(ART, 50, 3, null, 'Card');
		const id = added.clip_id!;
		expect(clipOf(added.timeline, id).source_out).toBe(3);
		const renamed = await updateTitleClip(id, { name: ' Sting ' });
		expect(titleOf(clipOf(renamed.timeline, id))!.name).toBe('Sting');
		const replaced = await updateTitleClip(id, { svg: ART.replace('#e63946', '#00ff00') });
		expect(titleOf(clipOf(replaced.timeline, id))!.svg).toContain('#00ff00');
		await expect(updateTitleClip(id, {})).rejects.toThrow('nothing to change');
	});

	test('a title plays at its own speed', async () => {
		const { clip_id } = await addTitleClip(ART, 60, 2);
		await expect(setSpeed(clip_id!, 2)).rejects.toThrow('own speed');
		expect(titleOf(clipOf(await getTimeline(), clip_id!))).not.toBeNull();
	});
});
