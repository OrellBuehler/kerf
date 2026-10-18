<script lang="ts">
	// A clip's grade beyond the Color numbers: a curve editor, three colour wheels and a LUT. Each
	// gesture writes one edit when it ends (a point or a puck let go, a slider released, a file
	// chosen); Escape, pointercancel or a lost focus abandon a drag and put it back. The curve and
	// the wheels' numbers are `grade.ts`'s, the TS mirror of the engine's, so what is drawn here is
	// what the export renders. The preview shows the grade through FFmpeg (the GPU preview refuses a
	// graded clip), so it matches the file.
	import InspectorSection from './InspectorSection.svelte';
	import SliderRow from './SliderRow.svelte';
	import Chip from './Chip.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { settings } from '$lib/settings.svelte';
	import { report } from '$lib/ops';
	import { inspectLut, pickLutFile } from '$lib/api';
	import { beginDrag } from '$lib/drag';
	import { resolveColor } from '$lib/waveform-draw';
	import { PERCENT } from '$lib/slider-row';
	import {
		CURVE_CHANNELS,
		IDENTITY_POINTS,
		NEUTRAL_WHEELS,
		WHEEL_RANGES,
		addPoint,
		curveShape,
		curvesSummary,
		evalShape,
		lutName,
		lutSummary,
		movePoint,
		nearestPoint,
		normalizeCurve,
		normalizeGrade,
		normalizeWheel,
		wheelHue,
		wheelRgb,
		wheelsSummary,
		withChannel,
		type CurveChannel,
		type WheelRange
	} from '$lib/grade';
	import type { Clip, CurvePoint, Grade, LutInfo, Wheel } from '$lib/types';

	let { clip }: { clip: Clip } = $props();

	const grade = $derived(normalizeGrade(clip.grade));

	async function write(clipId: string, next: Grade) {
		try {
			await editor.setGrade(clipId, next);
		} catch (e) {
			report(e);
		}
	}

	// ---- curves ----------------------------------------------------------------

	const SIZE = 200;
	const HIT = 0.05;
	const CHANNEL_TOKEN: Record<CurveChannel, string> = {
		master: '--text-primary',
		red: '--danger',
		green: '--success',
		blue: '--info'
	};
	const CHANNEL_LABEL: Record<CurveChannel, string> = { master: 'Master', red: 'Red', green: 'Green', blue: 'Blue' };

	let channel = $state<CurveChannel>('master');
	/** The points of `channel` while a gesture moves them; `null` between gestures. */
	let draft = $state.raw<CurvePoint[] | null>(null);
	let canvas = $state<HTMLCanvasElement | null>(null);

	const stored = (ch: CurveChannel) => grade.curves?.[ch] ?? [];
	const shown = (ch: CurveChannel) => {
		const pts = ch === channel && draft ? draft : stored(ch);
		return pts.length ? pts : IDENTITY_POINTS;
	};

	const palette = $derived.by(() => {
		void settings.theme;
		return {
			bg: resolveColor('--surface-inset'),
			grid: resolveColor('--border-subtle'),
			diagonal: resolveColor('--border-default'),
			point: resolveColor('--surface-panel'),
			channel: Object.fromEntries(CURVE_CHANNELS.map((c) => [c, resolveColor(CHANNEL_TOKEN[c])])) as Record<CurveChannel, string>
		};
	});

	$effect(() => {
		const el = canvas;
		if (!el) return;
		const dpr = window.devicePixelRatio || 1;
		el.width = SIZE * dpr;
		el.height = SIZE * dpr;
		const ctx = el.getContext('2d');
		if (!ctx) return;
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		const p = palette;
		ctx.fillStyle = p.bg;
		ctx.fillRect(0, 0, SIZE, SIZE);
		ctx.strokeStyle = p.grid;
		ctx.lineWidth = 1;
		for (let i = 1; i < 4; i++) {
			const v = (i * SIZE) / 4 + 0.5;
			ctx.beginPath();
			ctx.moveTo(v, 0);
			ctx.lineTo(v, SIZE);
			ctx.moveTo(0, v);
			ctx.lineTo(SIZE, v);
			ctx.stroke();
		}
		ctx.strokeStyle = p.diagonal;
		ctx.beginPath();
		ctx.moveTo(0, SIZE);
		ctx.lineTo(SIZE, 0);
		ctx.stroke();
		for (const ch of [...CURVE_CHANNELS.filter((c) => c !== channel), channel]) {
			const pts = shown(ch);
			const shape = curveShape(pts);
			if (!shape && ch !== channel) continue;
			ctx.globalAlpha = ch === channel ? 1 : 0.35;
			ctx.strokeStyle = p.channel[ch];
			ctx.lineWidth = ch === channel ? 2 : 1;
			ctx.beginPath();
			for (let i = 0; i <= SIZE; i++) {
				const x = i / SIZE;
				const y = shape ? evalShape(shape, x) : x;
				if (i === 0) ctx.moveTo(i, SIZE - y * SIZE);
				else ctx.lineTo(i, SIZE - y * SIZE);
			}
			ctx.stroke();
		}
		ctx.globalAlpha = 1;
		for (const pt of shown(channel)) {
			ctx.beginPath();
			ctx.arc(pt.x * SIZE, SIZE - pt.y * SIZE, 4, 0, Math.PI * 2);
			ctx.fillStyle = p.point;
			ctx.fill();
			ctx.strokeStyle = p.channel[channel];
			ctx.lineWidth = 2;
			ctx.stroke();
		}
	});

	function at(e: PointerEvent | MouseEvent): { x: number; y: number } {
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		return { x: (e.clientX - r.left) / r.width, y: 1 - (e.clientY - r.top) / r.height };
	}

	const sameCurve = (a: readonly CurvePoint[], b: readonly CurvePoint[]) =>
		JSON.stringify(normalizeCurve(a)) === JSON.stringify(normalizeCurve(b));

	/** Writes the channel's points, unless they are what the clip already has. */
	function writeCurve(clipId: string, ch: CurveChannel, points: CurvePoint[]) {
		if (sameCurve(points, stored(ch))) return Promise.resolve();
		return write(clipId, { ...grade, curves: withChannel(grade.curves, ch, normalizeCurve(points)) });
	}

	/** A click that only added a point (or put the puck down) waits this long for the second click
	 *  of a double-click, which undoes it, so a double-click writes no edit and its opposite. */
	const DOUBLE_CLICK_MS = 300;
	let pending: { key: string; timer: ReturnType<typeof setTimeout>; flush: () => void } | null = null;
	function defer(key: string, flush: () => void) {
		settle(key);
		const timer = setTimeout(() => {
			pending = null;
			flush();
		}, DOUBLE_CLICK_MS);
		pending = { key, timer, flush };
	}
	/** Drops the waiting click if it is `key`'s (true), or writes it now if it is another control's. */
	function settle(key: string): boolean {
		const p = pending;
		if (!p) return false;
		clearTimeout(p.timer);
		pending = null;
		if (p.key === key) return true;
		p.flush();
		return false;
	}
	$effect(() => () => settle(''));

	function curveDown(e: PointerEvent) {
		if (e.button !== 0 || editor.locked) return;
		const clipId = clip.id;
		const ch = channel;
		// The second click of a double-click lands on the point the first one is still waiting to add.
		const carried = settle(`curve:${ch}`);
		const start = shown(ch);
		const p = at(e);
		let index = nearestPoint(start, p.x, p.y, HIT);
		let points = start;
		let added = carried;
		if (index < 0) {
			const next = addPoint(start, p.x, p.y);
			if (!next) return;
			points = next.points;
			index = next.index;
			added = true;
		}
		draft = points;
		let moved = false;
		const target = e.currentTarget as HTMLElement;
		beginDrag(e, {
			move(ev) {
				moved = true;
				const r = target.getBoundingClientRect();
				draft = movePoint(points, index, (ev.clientX - r.left) / r.width, 1 - (ev.clientY - r.top) / r.height);
			},
			async commit() {
				const done = draft ?? points;
				if (!moved && added) {
					const mine = draft;
					defer(`curve:${ch}`, () => {
						void writeCurve(clipId, ch, done).then(() => {
							if (draft === mine) draft = null;
						});
					});
					return;
				}
				await writeCurve(clipId, ch, done);
				draft = null;
			},
			abandon() {
				draft = null;
			}
		});
	}

	/** A double-click or a right-click on a point takes it out. */
	function removeAt(e: MouseEvent) {
		const pts = shown(channel);
		const p = at(e);
		const i = nearestPoint(pts, p.x, p.y, HIT);
		if (i < 0) return;
		e.preventDefault();
		e.stopPropagation();
		settle(`curve:${channel}`);
		draft = null;
		void writeCurve(clip.id, channel, pts.filter((_, j) => j !== i));
	}

	// ---- wheels ----------------------------------------------------------------

	const WHEEL = 84;
	const RANGE_LABEL: Record<WheelRange, string> = { shadows: 'Shadows', midtones: 'Midtones', highlights: 'Highlights' };
	const wheels = $derived(grade.wheels ?? NEUTRAL_WHEELS);
	let puck = $state.raw<{ range: WheelRange; wheel: Wheel } | null>(null);
	const wheelOf = (r: WheelRange) => (puck?.range === r ? puck.wheel : wheels[r]);

	function writeWheel(clipId: string, range: WheelRange, wheel: Wheel) {
		const next = normalizeWheel(wheel);
		const now = normalizeWheel(wheels[range]);
		if (next.x === now.x && next.y === now.y && next.lightness === now.lightness) return Promise.resolve();
		return write(clipId, { ...grade, wheels: { ...wheels, [range]: normalizeWheel(wheel) } });
	}

	/** The hue disc: each pixel the direction `colorbalance` pushes at that puck position. */
	function paintDisc(el: HTMLCanvasElement) {
		const n = WHEEL * 2;
		el.width = n;
		el.height = n;
		const ctx = el.getContext('2d');
		if (!ctx) return;
		const img = ctx.createImageData(n, n);
		for (let j = 0; j < n; j++) {
			for (let i = 0; i < n; i++) {
				const x = ((i + 0.5) / n) * 2 - 1;
				const y = 1 - ((j + 0.5) / n) * 2;
				const o = (j * n + i) * 4;
				const [r, g, b] = wheelHue(x, y);
				img.data[o] = r;
				img.data[o + 1] = g;
				img.data[o + 2] = b;
				img.data[o + 3] = Math.hypot(x, y) <= 1 ? 255 : 0;
			}
		}
		ctx.putImageData(img, 0, 0);
	}

	function wheelPoint(el: HTMLElement, e: PointerEvent): { x: number; y: number } {
		const r = el.getBoundingClientRect();
		return { x: ((e.clientX - r.left) / r.width) * 2 - 1, y: 1 - ((e.clientY - r.top) / r.height) * 2 };
	}

	function wheelDown(e: PointerEvent, range: WheelRange) {
		if (e.button !== 0 || editor.locked) return;
		const clipId = clip.id;
		const el = e.currentTarget as HTMLElement;
		const base = wheels[range];
		const place = (ev: PointerEvent) => {
			const p = wheelPoint(el, ev);
			puck = { range, wheel: normalizeWheel({ ...base, x: p.x, y: p.y }) };
		};
		settle(`wheel:${range}`);
		place(e);
		let moved = false;
		beginDrag(e, {
			move(ev) {
				moved = true;
				place(ev);
			},
			async commit() {
				const done = puck?.wheel ?? base;
				if (!moved) {
					const mine = puck;
					defer(`wheel:${range}`, () => {
						void writeWheel(clipId, range, done).then(() => {
							if (puck === mine) puck = null;
						});
					});
					return;
				}
				await writeWheel(clipId, range, done);
				puck = null;
			},
			abandon() {
				puck = null;
			}
		});
	}

	const rgbText = (w: Wheel) =>
		wheelRgb(w)
			.map((v) => (v > 0 ? '+' : '') + v.toFixed(2))
			.join(' ');

	// ---- LUT -------------------------------------------------------------------

	let info = $state<{ path: string; info: LutInfo | null; error: string | null } | null>(null);
	const lutPath = $derived(grade.lut?.path);
	$effect(() => {
		const path = lutPath;
		if (!path) {
			info = null;
			return;
		}
		let live = true;
		inspectLut(path).then(
			(i) => live && (info = { path, info: i, error: null }),
			(e) => live && (info = { path, info: null, error: e instanceof Error ? e.message : String(e) })
		);
		return () => {
			live = false;
		};
	});

	async function chooseLut() {
		const clipId = clip.id;
		const path = await pickLutFile();
		if (!path) return;
		await write(clipId, { ...grade, lut: { path, intensity: grade.lut?.intensity ?? 1 } });
	}
</script>

<InspectorSection title="Curves" summary={curvesSummary(clip.grade)}>
	<div style="display:flex;gap:5px;flex-wrap:wrap;align-items:center;margin-bottom:6px">
		{#each CURVE_CHANNELS as ch (ch)}
			<Chip pressed={channel === ch} onclick={() => (channel = ch)}>{CHANNEL_LABEL[ch]}</Chip>
		{/each}
		<span style="flex:1"></span>
		<Btn
			variant="ghost"
			size="sm"
			pending={editor.locked}
			disabled={!stored(channel).length}
			title={`Straighten the ${CHANNEL_LABEL[channel].toLowerCase()} curve`}
			onclick={() => writeCurve(clip.id, channel, [])}>Reset</Btn
		>
	</div>
	<canvas
		bind:this={canvas}
		data-curve-editor
		aria-label="{CHANNEL_LABEL[channel]} curve: click to add a point, drag to move it, double-click to remove it"
		style="display:block;width:{SIZE}px;height:{SIZE}px;max-width:100%;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);cursor:crosshair;touch-action:none"
		onpointerdown={curveDown}
		ondblclick={removeAt}
		oncontextmenu={removeAt}
	></canvas>
	<div style="font-size:var(--text-xs);color:var(--text-muted);margin:6px 0">
		Click to add a point and drag it; double-click a point to take it out. Each channel runs first, then the master.
	</div>
</InspectorSection>

<InspectorSection title="Color wheels" summary={wheelsSummary(clip.grade)}>
	{#each WHEEL_RANGES as range (range)}
		{@const w = wheelOf(range)}
		<div style="display:flex;gap:10px;align-items:center;margin:6px 0">
			<div
				role="slider"
				tabindex="-1"
				aria-label="{RANGE_LABEL[range]} hue"
				aria-valuetext={rgbText(w)}
				aria-valuenow={Math.round(Math.hypot(w.x, w.y) * 100)}
				style="position:relative;width:{WHEEL}px;height:{WHEEL}px;flex:none;cursor:crosshair;touch-action:none"
				onpointerdown={(e) => wheelDown(e, range)}
				ondblclick={() => {
					settle(`wheel:${range}`);
					puck = null;
					void writeWheel(clip.id, range, { ...wheels[range], x: 0, y: 0 });
				}}
			>
				<canvas
					use:paintDisc
					style="width:100%;height:100%;border-radius:var(--radius-full);border:var(--line-width) solid var(--border-strong)"
				></canvas>
				<span
					style="position:absolute;left:{(w.x + 1) * 50}%;top:{(1 - w.y) * 50}%;width:10px;height:10px;margin:-5px 0 0 -5px;border-radius:var(--radius-full);background:var(--surface-panel);border:2px solid var(--text-primary);pointer-events:none"
				></span>
			</div>
			<div style="flex:1;min-width:0">
				<div style="display:flex;justify-content:space-between;font-size:12px;color:var(--text-muted)">
					<span>{RANGE_LABEL[range]}</span>
					<span style="font-family:var(--font-mono)">{rgbText(w)}</span>
				</div>
				<SliderRow
					label="Lightness"
					labelWidth={58}
					reserve={false}
					value={w.lightness}
					spec={{ min: -1, max: 1, step: 0.01, neutral: 0, digits: 2 }}
					scope={clip.id}
					disabled={editor.locked}
					onCommit={(v, clipId) => writeWheel(clipId, range, { ...w, lightness: v })}
				/>
			</div>
		</div>
	{/each}
	<div style="display:flex;justify-content:flex-end">
		<Btn
			variant="ghost"
			size="sm"
			pending={editor.locked}
			disabled={!grade.wheels}
			title="Back to neutral wheels"
			onclick={() => write(clip.id, { ...grade, wheels: null })}>Reset</Btn
		>
	</div>
</InspectorSection>

<InspectorSection title="LUT" summary={lutSummary(clip.grade)}>
	{#if grade.lut}
		<div style="font-size:12px;color:var(--text-secondary);margin-bottom:4px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" data-selectable>
			{lutName(grade.lut.path)}
		</div>
		{#if info?.path === grade.lut.path}
			<div style="font-size:var(--text-xs);color:{info.error ? 'var(--danger)' : 'var(--text-muted)'};margin-bottom:6px">
				{info.error ? `Not applied: ${info.error}` : `${info.info?.size}³${info.info?.title ? ` · ${info.info.title}` : ''}`}
			</div>
		{/if}
		<SliderRow
			label="Intensity"
			value={grade.lut.intensity}
			spec={{ min: 0, max: 1, step: 0.01, neutral: 1, ...PERCENT }}
			scope={clip.id}
			disabled={editor.locked}
			onCommit={(v, clipId) => write(clipId, { ...grade, lut: grade.lut ? { ...grade.lut, intensity: v } : null })}
		/>
	{/if}
	<div style="display:flex;gap:6px;margin:6px 0">
		<Btn size="sm" pending={editor.locked} title="Look the clip up through a .cube 3D LUT" onclick={chooseLut}>Choose LUT…</Btn>
		<Btn variant="ghost" size="sm" pending={editor.locked} disabled={!grade.lut} title="Remove the LUT" onclick={() => write(clip.id, { ...grade, lut: null })}
			>Clear</Btn
		>
	</div>
	<div style="font-size:var(--text-xs);color:var(--text-muted)">
		Applied after the Color numbers and before the curves and wheels.
	</div>
</InspectorSection>
