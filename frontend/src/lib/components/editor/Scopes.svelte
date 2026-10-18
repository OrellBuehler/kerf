<script lang="ts">
	// The Scopes panel: waveform, RGB parade, vectorscope and histogram of the frame under the
	// playhead, and a readout of what they say (levels, clipping, cast).
	//
	// The frame is the export's (`get_scopes`: the composited still from the originals, read with
	// the delivery's matrix), so it is a real decode: one request in flight, the newest playhead
	// asked for once it lands — a short debounce while paused, once a second while playing — and
	// nothing while the panel is not on screen. The grids are painted from `scopes-view.ts`; every
	// colour is a theme token resolved here, again whenever the theme changes.
	import { untrack } from 'svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { getScopes } from '#lib/api';
	import { describeError } from '#lib/log';
	import { intersectionObserverFor } from '#lib/realm';
	import { windows } from '#lib/windows.svelte';
	import { resolveColor } from '#lib/waveform-draw';
	import { parseCssColor } from '#lib/preview-bounds';
	import {
		blankPixels,
		histogramHeights,
		isScopeKind,
		paintGrid,
		PANEL_GRID,
		SCOPE_KINDS,
		summaryLines,
		TARGETS,
		vectorPoint,
		ycbcr,
		type Pixels,
		type Rgb
	} from '#lib/scopes-view';
	import type { ScopeKind, Scopes } from '#lib/types';

	const KIND_KEY = 'kerf.scopes.kind';
	function storedKind(): ScopeKind {
		try {
			const v = localStorage.getItem(KIND_KEY);
			return isScopeKind(v) ? v : 'waveform';
		} catch {
			return 'waveform';
		}
	}
	let kind = $state<ScopeKind>(storedKind());
	function choose(k: ScopeKind) {
		kind = k;
		try {
			localStorage.setItem(KIND_KEY, k);
		} catch {
			// A blocked store only forgets the choice.
		}
	}

	let root = $state<HTMLDivElement | null>(null);
	let host = $state<HTMLDivElement | null>(null);
	let canvas = $state<HTMLCanvasElement | null>(null);
	let visible = $state(true);
	$effect(() => {
		if (!root) return;
		void windows.version;
		const io = intersectionObserverFor(root, (entries) => (visible = entries.at(-1)?.isIntersecting ?? true));
		io.observe(root);
		return () => io.disconnect();
	});

	let box = $state({ w: 0, h: 0 });
	$effect(() => {
		if (!host) return;
		const ro = new ResizeObserver(() => {
			if (host) box = { w: host.clientWidth, h: host.clientHeight };
		});
		ro.observe(host);
		return () => ro.disconnect();
	});

	// ---- fetching ---------------------------------------------------------------

	/** The width the frame is measured at: enough for the grids and a fair clip count, a
	 *  fraction of a 4K decode. */
	const SCOPE_WIDTH = 960;
	/** While playing, a measurement every this often: the decode is from the originals. */
	const PLAYING_EVERY_MS = 2000;

	let scopes = $state.raw<Scopes | null>(null);
	let error = $state<string | null>(null);
	let inFlight = false;
	let timer: ReturnType<typeof setTimeout> | null = null;
	/** What the shown scopes were measured for; a different want asks again. */
	let measured = '';

	/** Moves with every refresh of the cut: an edit changes the frame under a still playhead. */
	let cutVersion = $state(0);
	$effect(() => {
		void editor.timeline;
		void editor.assets;
		untrack(() => cutVersion++);
	});
	const want = $derived(`${ui.time.toFixed(4)}|${kind}|${cutVersion}`);

	async function fetchNow() {
		if (inFlight) return;
		inFlight = true;
		const key = want;
		const t = ui.time;
		try {
			scopes = await getScopes(t, [kind], PANEL_GRID, 'auto', SCOPE_WIDTH);
			error = null;
		} catch (e) {
			error = describeError(e);
		} finally {
			measured = key;
			inFlight = false;
			if (want !== measured) schedule();
		}
	}

	function schedule() {
		if (timer) clearTimeout(timer);
		if (!visible) return;
		timer = setTimeout(
			() => {
				timer = null;
				void fetchNow();
			},
			ui.playing ? PLAYING_EVERY_MS : 150
		);
	}

	$effect(() => {
		void want;
		const on = visible;
		untrack(() => {
			if (on && want !== measured && (!ui.playing || !timer)) schedule();
		});
	});
	$effect(() => () => {
		if (timer) clearTimeout(timer);
	});

	// ---- drawing ------------------------------------------------------------------

	const palette = $derived.by(() => {
		void settings.theme;
		const el = root;
		if (!el) return null;
		const tone = (token: string): Rgb => {
			const hex = parseCssColor(resolveColor(token, el)) ?? '';
			const n = parseInt(hex.slice(1), 16);
			return Number.isFinite(n) ? [(n >> 16) & 255, (n >> 8) & 255, n & 255] : [255, 255, 255];
		};
		return {
			trace: tone('--scope-trace'),
			red: tone('--scope-red'),
			green: tone('--scope-green'),
			blue: tone('--scope-blue'),
			grid: resolveColor('--scope-graticule', el),
			label: resolveColor('--text-muted', el),
			redCss: resolveColor('--scope-red', el),
			greenCss: resolveColor('--scope-green', el),
			blueCss: resolveColor('--scope-blue', el),
			traceCss: resolveColor('--scope-trace', el)
		};
	});

	$effect(() => {
		const c = canvas;
		const p = palette;
		const s = scopes;
		const { w, h } = box;
		const k = kind;
		if (!c || !p || w <= 0 || h <= 0) return;
		const dpr = c.ownerDocument.defaultView?.devicePixelRatio ?? 1;
		const W = Math.max(1, Math.round(w * dpr));
		const H = Math.max(1, Math.round(h * dpr));
		const pad = Math.round(4 * dpr);
		if (H <= 2 * pad + 2) return;
		c.width = W;
		c.height = H;
		const ctx = c.getContext('2d');
		if (!ctx) return;
		ctx.clearRect(0, 0, W, H);
		ctx.lineWidth = Math.max(1, dpr);
		const put = (px: Pixels, x: number, y: number) => {
			const img = ctx.createImageData(px.width, px.height);
			img.data.set(px.data);
			ctx.putImageData(img, x, y);
		};
		const levels = (x0: number, width: number) => {
			ctx.strokeStyle = p.grid;
			for (const v of [0, 0.25, 0.5, 0.75, 1]) {
				const y = Math.round(pad + (1 - v) * (H - 2 * pad)) + 0.5;
				ctx.beginPath();
				ctx.moveTo(x0, y);
				ctx.lineTo(x0 + width, y);
				ctx.stroke();
			}
		};
		if (!s) return;
		if (k === 'waveform' && s.waveform) {
			const px = blankPixels(W, H - 2 * pad);
			paintGrid(px, s.waveform, p.trace);
			put(px, 0, pad);
			levels(0, W);
		} else if (k === 'parade' && s.parade) {
			const px = blankPixels(W, H - 2 * pad);
			const gap = Math.round(6 * dpr);
			const each = Math.floor((W - 2 * gap) / 3);
			s.parade.forEach((g, i) => paintGrid(px, g, [p.red, p.green, p.blue][i], i * (each + gap), each));
			put(px, 0, pad);
			for (let i = 0; i < 3; i++) levels(i * (each + gap), each);
		} else if (k === 'vectorscope' && s.vectorscope) {
			const side = Math.max(1, Math.min(W, H) - 2 * pad);
			const x0 = Math.round((W - side) / 2);
			const y0 = Math.round((H - side) / 2);
			const px = blankPixels(side, side);
			paintGrid(px, s.vectorscope, p.trace);
			put(px, x0, y0);
			ctx.strokeStyle = p.grid;
			const centre = vectorPoint(0, 0, side);
			const ring = vectorPoint(0.5, 0, side).x - centre.x;
			ctx.beginPath();
			ctx.arc(x0 + centre.x, y0 + centre.y, ring, 0, Math.PI * 2);
			ctx.stroke();
			// The skin-tone line, 123° from +Cb.
			const a = (123 * Math.PI) / 180;
			ctx.beginPath();
			ctx.moveTo(x0 + centre.x, y0 + centre.y);
			ctx.lineTo(x0 + centre.x + Math.cos(a) * ring, y0 + centre.y - Math.sin(a) * ring);
			ctx.stroke();
			ctx.fillStyle = p.label;
			ctx.font = `${Math.round(9 * dpr)}px sans-serif`;
			const m = s.summary.matrix;
			for (const t of TARGETS) {
				const [, cb, cr] = ycbcr(m, t.rgb[0], t.rgb[1], t.rgb[2]);
				const q = vectorPoint(cb, cr, side);
				const r = Math.round(4 * dpr);
				ctx.strokeRect(x0 + q.x - r, y0 + q.y - r, 2 * r, 2 * r);
				ctx.fillText(t.name, x0 + q.x + r + 2, y0 + q.y + r);
			}
		} else if (k === 'histogram' && s.histogram) {
			const hh = histogramHeights(s.histogram, ['red', 'green', 'blue', 'luma']);
			const usable = H - 2 * pad;
			const bar = W / 256;
			ctx.globalCompositeOperation = 'lighter';
			for (const [ch, css] of [
				['red', p.redCss],
				['green', p.greenCss],
				['blue', p.blueCss]
			] as const) {
				ctx.fillStyle = css;
				ctx.globalAlpha = 0.6;
				hh[ch].forEach((v, i) => {
					if (v > 0) ctx.fillRect(i * bar, pad + usable * (1 - v), Math.max(1, bar), usable * v);
				});
			}
			ctx.globalAlpha = 1;
			ctx.globalCompositeOperation = 'source-over';
			ctx.strokeStyle = p.traceCss;
			ctx.beginPath();
			hh.luma.forEach((v, i) => {
				const x = i * bar + bar / 2;
				const y = pad + usable * (1 - v);
				if (i === 0) ctx.moveTo(x, y);
				else ctx.lineTo(x, y);
			});
			ctx.stroke();
		}
	});

	const lines = $derived(scopes ? summaryLines(scopes.summary) : []);
</script>

<div class="scopes" bind:this={root}>
	<div class="kinds" role="radiogroup" aria-label="Scope">
		{#each SCOPE_KINDS as k (k.id)}
			<button type="button" role="radio" aria-checked={kind === k.id} class:on={kind === k.id} onclick={() => choose(k.id)}
				>{k.label}</button
			>
		{/each}
	</div>
	<div class="plot" bind:this={host}>
		<canvas bind:this={canvas} aria-label="{SCOPE_KINDS.find((k) => k.id === kind)?.label} of the frame at the playhead"
		></canvas>
		{#if !scopes && !error}
			<p class="note">Measuring the frame at the playhead…</p>
		{/if}
	</div>
	{#if error}
		<p class="error" role="alert">Couldn't measure the frame — {error}</p>
	{/if}
	{#if scopes}
		<dl class="readout">
			{#each lines as line (line.label)}
				<dt>{line.label}</dt>
				<dd class:warn={line.warn}>{line.text}</dd>
			{/each}
		</dl>
		<p class="caption">
			{scopes.summary.width}×{scopes.summary.height}, {scopes.summary.matrix.toUpperCase()} — the frame the export
			writes.
		</p>
	{/if}
</div>

<style>
	.scopes {
		flex: 1;
		min-height: 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 8px;
		box-sizing: border-box;
		overflow: auto;
		background: var(--surface-panel);
	}
	.kinds {
		flex: none;
		display: flex;
		gap: 4px;
		flex-wrap: wrap;
	}
	.kinds button {
		font: var(--type-label);
		padding: 3px 8px;
		border-radius: var(--radius-sm);
		border: var(--line-width) solid var(--border-subtle);
		background: var(--surface-raised);
		color: var(--text-secondary);
		cursor: pointer;
	}
	.kinds button.on {
		border-color: var(--selection-border);
		color: var(--text-primary);
		background: var(--selection-fill);
	}
	.plot {
		position: relative;
		flex: 1 1 160px;
		min-height: 120px;
		border-radius: var(--radius-sm);
		background: var(--frame-matte);
		overflow: hidden;
	}
	canvas {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
	}
	.note {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		margin: 0;
		font-size: 12px;
		color: var(--text-muted);
	}
	.readout {
		flex: none;
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 2px 8px;
		margin: 0;
		font-size: 11px;
		line-height: 1.4;
	}
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
		font-family: var(--font-mono);
		color: var(--text-secondary);
	}
	dd.warn {
		color: var(--warning);
	}
	.caption,
	.error {
		flex: none;
		margin: 0;
		font-size: 11px;
		color: var(--text-muted);
	}
	.error {
		color: var(--danger);
	}
</style>
