// What an export measured of the file it wrote (`ExportReport`), in words for the notification that
// ends it. `get_levels` measures the timeline's mix; this is the file — the number a platform judges
// and a lossy encode can move (an AAC re-encode raised a limited peak back over its ceiling).
// Pure and bun-tested.

import { toFixedEven } from './format-fixed';
import { fileName } from './failure-copy';
import type { ExportReport, LevelReading } from './types';

const db = (v: number) => toFixedEven(v, 1);

/** `-14.2 LUFS · true peak -0.4 dBTP · LRA 6.1 LU`: what the file's meter read. Silence has nothing to read. */
export function loudnessText(l: LevelReading | null | undefined): string | null {
	if (!l) return null;
	const parts: string[] = [];
	if (l.integrated_lufs != null) parts.push(`${db(l.integrated_lufs)} LUFS`);
	if (l.true_peak_dbtp != null) parts.push(`true peak ${db(l.true_peak_dbtp)} dBTP`);
	if (l.loudness_range_lu != null) parts.push(`LRA ${db(l.loudness_range_lu)} LU`);
	return parts.length ? parts.join(' · ') : 'silent';
}

/** `True peak -0.4 dBTP is 0.6 dB over the -1.0 dBTP ceiling`, or null when it is not over (or there is no target). */
export function overText(r: ExportReport): string | null {
	const peak = r.loudness?.true_peak_dbtp;
	if (r.true_peak_over_db == null || peak == null || !r.target) return null;
	return `True peak ${db(peak)} dBTP is ${db(r.true_peak_over_db)} dB over the ${db(r.target.true_peak_dbtp)} dBTP ceiling`;
}

/** The kind of file it is, for a person: its extension, else what ffprobe called the container. */
function kind(r: ExportReport): string | null {
	const ext = /\.([A-Za-z0-9]{1,5})$/.exec(r.path)?.[1];
	return ext ? ext.toLowerCase() : (r.container?.split(',')[0] ?? null);
}

/** `mp4 · h264 1920×1080 30 fps · aac 48 kHz stereo · 38.9 MB`: what the probe found. */
export function factsText(r: ExportReport): string {
	const parts: string[] = [];
	const k = kind(r);
	if (k) parts.push(k);
	if (r.video) {
		const size = r.video.width && r.video.height ? ` ${r.video.width}×${r.video.height}` : '';
		const fps = r.video.fps ? ` ${Number(r.video.fps.toFixed(2))} fps` : '';
		parts.push(`${r.video.codec}${size}${fps}`);
	}
	if (r.audio) {
		const rate = r.audio.sample_rate ? ` ${Number((r.audio.sample_rate / 1000).toFixed(1))} kHz` : '';
		const ch = r.audio.channels === 1 ? ' mono' : r.audio.channels === 2 ? ' stereo' : r.audio.channels ? ` ${r.audio.channels} ch` : '';
		const rateKbps = r.audio.bit_rate ? ` ${Math.round(r.audio.bit_rate / 1000)} kb/s` : '';
		parts.push(`${r.audio.codec}${rate}${ch}${rateKbps}`);
	}
	if (r.size_bytes != null) parts.push(`${(r.size_bytes / 1_000_000).toFixed(1)} MB`);
	return parts.join(' · ');
}

export interface WrittenNotice {
	/** A file's true peak is over its target's ceiling: the notice is a warning. */
	over: boolean;
	/** The lines under the headline; undefined when there is nothing to add. */
	description: string | undefined;
}

/** The notification under `Exported …`: the loudness the file measured (one line per file when there are
 *  several), a true peak over its ceiling spelled out, and — without alarm — what could not be measured. */
export function writtenNotice(reports: readonly ExportReport[]): WrittenNotice {
	const many = reports.length > 1;
	const lines: string[] = [];
	let over = false;
	for (const r of reports) {
		const name = many ? `${fileName(r.path)}: ` : '';
		const warn = overText(r);
		if (warn) over = true;
		const loud = loudnessText(r.loudness);
		if (warn) lines.push(`${name}${warn}${r.loudness?.integrated_lufs != null ? ` (${db(r.loudness.integrated_lufs)} LUFS)` : ''}`);
		else if (loud) lines.push(`${name}${loud}`);
		else if (r.problem) lines.push(`${name}not measured: ${r.problem}`);
	}
	return { over, description: lines.length ? lines.join('\n') : undefined };
}
