/* What the platform check says about a cut, and whether it says anything at all.
 *
 * `kerf_core::platform` decides what each publishing target accepts (the answer comes
 * back as `DeliveryCheck`s); this module decides which of those answers a person
 * should see. The old panel judged every platform Kerf knows about at every export —
 * an MP3 was "letterboxed on Instagram Reels", a GIF "below YouTube's 1920x1080" — and
 * put an amber triangle on the happiest path. Now:
 *
 *  - it only speaks about a video export that has a picture (`judgeable`);
 *  - with no shape ticked under Deliver to, it is one neutral line stating the file
 *    and where it already fits; amber and red are kept for a platform's real limits
 *    (length) on a platform that frame is made for;
 *  - with shapes ticked, each file is judged only for the platforms that take its shape;
 *  - it never complains that a frame is the wrong shape for a platform nobody picked.
 *
 * Pure: reports in, lines out, so every case is a bun test. */

import { VARIANT_PRESETS, hasPicture, ratioLabel } from './delivery-formats';
import { containerInfo } from './export-presets';
import { TARGETS } from './platforms';
import type { DeliveryCheck, DeliveryIssue, ExportOptions, PlatformReport, PlatformTarget, Timeline } from './types';

export type VerdictLevel = 'ok' | 'note' | 'warning' | 'error';

export interface VerdictLine {
	level: VerdictLevel;
	text: string;
}

/** The lines about one file: the single export (`shape: null`) or one ticked shape. */
export interface VerdictGroup {
	shape: string | null;
	lines: VerdictLine[];
}

export interface Verdict {
	groups: VerdictGroup[];
	/** Advice that nothing is wrong without, folded away. */
	tips: string[];
}

export interface TickedShape {
	/** The chip's label, `9:16`. */
	label: string;
	report: PlatformReport;
}

export interface VerdictInput {
	/** The one file the options write, judged at its own frame — read when no shape is ticked, so
	 *  `null` once one is. */
	file: PlatformReport | null;
	/** The project's own frame when the file does not follow it, else `null`. */
	project: PlatformReport | null;
	/** The shapes ticked under Deliver to, each judged at its own frame, in picker order. */
	shapes: TickedShape[];
	/** The publishing targets the backend knows (`platformTargets`), for the size a soft frame is
	 *  made for. Left out, the harness mirror in `platforms.ts` stands in. */
	targets?: PlatformTarget[];
}

/** Whether the check has anything to judge: a video export, of a cut that has a picture. Audio
 *  presets, a GIF and an empty timeline are all someone else's business. `opts` is `null` where
 *  there are no export options (the docked Deliver panel). */
export function judgeable(opts: Pick<ExportOptions, 'container' | 'video_codec'> | null, timeline: Timeline): boolean {
	if (!hasPicture(timeline)) return false;
	if (!opts) return true;
	return !containerInfo(opts.container).audioOnly && opts.container !== 'gif' && opts.video_codec !== 'gif';
}

/** The platforms worth suggesting a shape for when the file is not one, in the order shown. */
const SUGGEST = ['reels', 'shorts', 'tiktok'];

/** How each platform is named in the "Tick a shape for …" hint; the check's own label stands for any not here. */
const SHORT: Record<string, string> = {
	reels: 'Reels',
	shorts: 'Shorts',
	tiktok: 'TikTok',
	ig_feed: 'Instagram feed',
	youtube: 'YouTube'
};

/** What a landscape file is for, in words: YouTube is also where the web plays it. */
const FINE_FOR: Record<string, string> = { youtube: 'YouTube and the web' };

/** Platforms the muted-autoplay advice does not apply to. */
const LONG_FORM = new Set(['youtube']);

/** What a shape is called when naming the version to tick. */
const SHAPE_NOUN: Record<string, string> = { '9:16': 'vertical', '1:1': 'square', '4:5': 'portrait' };

/** `m:ss`, rounded like the platform messages (`fmtDur`), so the length this line states and the one a
 *  limit message names agree: a 180.6 s cut is 3:01 in both, never "3:00" beside "3:01 is over". */
function clock(secs: number): string {
	const s = Math.max(0, Math.round(secs));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

function list(words: string[], joiner: 'and' | 'or'): string {
	if (words.length < 2) return words.join('');
	return `${words.slice(0, -1).join(', ')} ${joiner} ${words[words.length - 1]}`;
}

/** Whether the platform takes the frame without bars: the check had no shape complaint. */
const takes = (c: DeliveryCheck) => !c.issues.some((i) => i.kind === 'shape');
/** What the platform would reject or under-distribute for the cut's length. */
const limits = (c: DeliveryCheck) => c.issues.filter((i) => i.kind === 'length' && i.severity !== 'tip');
const levelOf = (i: DeliveryIssue): VerdictLevel => (i.severity === 'error' ? 'error' : 'warning');

function limitLines(checks: DeliveryCheck[]): VerdictLine[] {
	return checks.flatMap((c) => limits(c).map((i) => ({ level: levelOf(i), text: i.message })));
}

function nothingToJudge(report: PlatformReport): boolean {
	return report.cut.duration <= 0 || report.targets.some((c) => c.issues.some((i) => i.kind === 'empty'));
}

/** Advice for the platforms a file is made for: captions for a feed that plays muted, softness for
 *  a frame below what the platforms show. Neither is a limit, so neither is amber. The softness is one
 *  tip for the frame however many platforms would upscale it, not one per platform. */
function tipsFor(reports: PlatformReport[], targets: readonly PlatformTarget[]): string[] {
	const out = new Set<string>();
	for (const r of reports) {
		const aimed = r.targets.filter(takes);
		const soft = aimed.filter((c) => c.issues.some((i) => i.kind === 'resolution'));
		if (soft.length) {
			const many = soft.length > 1;
			const native = targets.find((t) => t.id === soft[0].target);
			const size = native ? ` (${native.width}×${native.height})` : '';
			const names = list(
				soft.map((c) => c.label),
				'and'
			);
			out.add(
				`${r.cut.width}×${r.cut.height} is below the size ${names} ${many ? 'are' : 'is'} made for${size}; ${many ? 'they' : 'the platform'} will upscale it and it will look soft.`
			);
		}
		for (const c of aimed)
			for (const i of c.issues) if (i.kind === 'captions' && !LONG_FORM.has(c.target)) out.add(i.message);
	}
	return [...out];
}

/** The project is cut for a shape and the file is wider: the picture would sit in bars. Said in
 *  the platforms' words, and only where the cut is evidently for them. */
function barsLine(file: PlatformReport, project: PlatformReport | null): VerdictLine | null {
	if (!project) return null;
	const [pw, ph] = [project.cut.width, project.cut.height];
	const [fw, fh] = [file.cut.width, file.cut.height];
	if (!(ph > 0 && fh > 0) || fw / fh <= (pw / ph) * 1.01) return null;
	const barred = project.targets.filter(takes).filter((c) => {
		const there = file.targets.find((f) => f.target === c.target);
		return there !== undefined && !takes(there);
	});
	if (!barred.length) return null;
	const shape = ratioLabel(pw, ph);
	const names = list(
		barred.map((c) => c.label),
		'and'
	);
	const many = barred.length > 1;
	const lead = shape === '9:16' ? `${names} ${many ? 'are' : 'is'} vertical (9:16).` : `${names} ${many ? 'take' : 'takes'} ${shape}.`;
	const fix = VARIANT_PRESETS.some((p) => p.label === shape)
		? ` Tick ${shape} under Deliver to to export ${SHAPE_NOUN[shape] ? `a ${SHAPE_NOUN[shape]}` : 'that'} version.`
		: '';
	return { level: 'warning', text: `${lead} This ${ratioLabel(fw, fh)} cut would be letterboxed.${fix}` };
}

function singleFile(file: PlatformReport, project: PlatformReport | null): VerdictGroup {
	const { width, height, duration } = file.cut;
	const aimed = file.targets.filter(takes);
	const ready = aimed.filter((c) => limits(c).length === 0);
	const bars = barsLine(file, project);
	const unmet = SUGGEST.flatMap((id) => {
		const c = file.targets.find((t) => t.target === id);
		return c && !takes(c) ? [SHORT[id] ?? c.label] : [];
	});

	let text = `This cut is ${width}×${height} (${ratioLabel(width, height)}) and ${clock(duration)} long.`;
	if (ready.length)
		text += ` Fine for ${list(
			ready.map((c) => FINE_FOR[c.target] ?? c.label),
			'and'
		)}.`;
	else if (!aimed.length) text += ' No platform here takes this shape without bars.';
	if (unmet.length && !bars)
		text += ` Tick a shape under Deliver to for ${list(unmet, 'or')}.`;

	return {
		shape: null,
		lines: [{ level: 'note', text }, ...limitLines(aimed), ...(bars ? [bars] : [])]
	};
}

function ticked(shape: TickedShape): VerdictGroup {
	const aimed = shape.report.targets.filter(takes);
	const ready = aimed.filter((c) => limits(c).length === 0);
	const lines: VerdictLine[] = [];
	if (ready.length) lines.push({ level: 'ok', text: `Ready for ${ready.map((c) => c.label).join(' · ')}` });
	else if (!aimed.length) lines.push({ level: 'note', text: 'No platform here takes this shape without bars.' });
	lines.push(...limitLines(aimed));
	return { shape: shape.label, lines };
}

/** The platform check for a cut, or `null` when there is nothing to judge (no length, nothing on
 *  the timeline). */
export function platformVerdict(input: VerdictInput): Verdict | null {
	const { file, project, shapes } = input;
	const targets = input.targets ?? TARGETS;
	if (shapes.length) {
		if (shapes.some((s) => nothingToJudge(s.report))) return null;
		return { groups: shapes.map(ticked), tips: tipsFor(shapes.map((s) => s.report), targets) };
	}
	if (!file || nothingToJudge(file)) return null;
	return { groups: [singleFile(file, project)], tips: tipsFor([file], targets) };
}
