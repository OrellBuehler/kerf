<script lang="ts">
	// The Scopes panel: waveform, RGB parade, vectorscope and histogram of the frame under the
	// playhead, and a readout of what they say (levels, clipping, cast).
	//
	// The frame is the export's (`get_scopes`: the composited still from the originals, read with
	// the delivery's matrix), so it is a real decode: one request in flight, the newest playhead
	// asked for once it lands — a short debounce while paused, once a second while playing — and
	// nothing while the panel is not on screen, or while there is no picture to measure (an empty
	// cut, a gap: said in the plot, not measured as black). The grids are painted from
	// `scopes-view.ts`, with their numbers (levels on the brightness scopes, rings and the skin
	// line on the vectorscope); every colour is a theme token resolved here, again whenever the
	// theme changes (and when the display's pixel ratio does). The scope is picked with the kit's `ChoiceChips` (the arrows move through
	// it); the readout, not the plot, scrolls when the panel is short.
	import { untrack } from 'svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import { roving } from '#lib/roving';
	import { tip } from '#lib/tip';
	import { tabStop } from '#lib/menu-nav';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { settings } from '#lib/settings.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { getScopes } from '#lib/api';
	import { describeError } from '#lib/log';
	import { intersectionObserverFor } from '#lib/realm';
	import { windows } from '#lib/windows.svelte';
	import { resolveColor } from '#lib/waveform-draw';
	import { parseCssColor } from '#lib/preview-bounds';
	import {
		LEVELS,
		RINGS,
		SCOPE_KINDS,
		TARGETS,
		blankPixels,
		histogramHeights,
		isScopeKind,
		dprQuery,
		fitLegend,
		kindLook,
		kindOptions,
		labelY,
		legendParts,
		levelY,
		paintGrid,
		PANEL_GRID,
		readoutLines,
		scaleSide,
		scopeSubject,
		showsScale,
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
	/** The panel's width: the chips are named in full only where they fit a row. */
	let panelWidth = $state(0);
	$effect(() => {
		if (!host || !root) return;
		const ro = new ResizeObserver(() => {
			if (host) box = { w: host.clientWidth, h: host.clientHeight };
			if (root) panelWidth = root.clientWidth;
		});
		ro.observe(host);
		ro.observe(root);
		return () => ro.disconnect();
	});

	/** The display's pixel ratio: the canvas is drawn at it, and drawn again when it changes (the window dragged to another
	 *  screen, the page zoomed). A resolution query is true only at one ratio, so it is made again each time it fires. */
	let dpr = $state(1);
	$effect(() => {
		const win = canvas?.ownerDocument.defaultView;
		if (!win) return;
		let query: MediaQueryList | null = null;
		const read = () => {
			query?.removeEventListener('change', read);
			dpr = win.devicePixelRatio || 1;
			query = win.matchMedia(dprQuery(dpr));
			query.addEventListener('change', read);
		};
		read();
		// A page zoom changes the ratio with a resize, which not every platform also says as a media change.
		win.addEventListener('resize', read);
		return () => {
			query?.removeEventListener('change', read);
			win.removeEventListener('resize', read);
		};
	});

	// ---- what there is to measure ---------------------------------------------------

	const subject = $derived(scopeSubject(editor.timeline, ui.time));

	// ---- fetching ---------------------------------------------------------------

	/** The width the frame is measured at: enough for the grids and a fair clip count, a
	 *  fraction of a 4K decode. */
	const SCOPE_WIDTH = 960;
	/** While playing, a measurement every this often: the decode is from the originals. */
	const PLAYING_EVERY_MS = 2000;

	let scopes = $state.raw<Scopes | null>(null);
	/** The playhead the shown scopes were measured at (it may lag the playhead while a newer one is asked). */
	let shownAt = $state(0);
	let error = $state<string | null>(null);
	let inFlight = false;
	let timer: ReturnType<typeof setTimeout> | null = null;
	/** What the shown scopes were measured for; a different want asks again. */
	let measured = $state('');

	/** Moves with every refresh of the cut: an edit changes the frame under a still playhead. */
	let cutVersion = $state(0);
	$effect(() => {
		void editor.timeline;
		void editor.assets;
		untrack(() => cutVersion++);
	});
	const want = $derived(`${ui.time.toFixed(4)}|${kind}|${cutVersion}`);

	async function fetchNow() {
		if (inFlight || subject !== 'picture') return;
		inFlight = true;
		const key = want;
		const t = ui.time;
		try {
			scopes = await getScopes(t, [kind], PANEL_GRID, 'auto', SCOPE_WIDTH);
			shownAt = t;
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
		if (!visible || subject !== 'picture') return;
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
		const on = visible && subject === 'picture';
		untrack(() => {
			if (on && want !== measured && (!ui.playing || !timer)) schedule();
		});
	});
	$effect(() => () => {
		if (timer) clearTimeout(timer);
	});

	/** What the plot and the readout show: the measurement, while there is a picture to have measured. */
	const shown = $derived(subject === 'picture' && !error ? scopes : null);
	const updating = $derived(subject === 'picture' && measured !== want);
	/** What the plot says in place of a trace, if anything. */
	const note = $derived.by((): { title: string; detail?: string } | null => {
		if (subject === 'empty')
			return { title: 'Nothing to measure', detail: 'No picture is drawn: the cut is empty, or every picture track is hidden. Show or fill one, and the frame under the playhead is measured here.' };
		if (subject === 'gap') return { title: 'No picture under the playhead', detail: 'Move the playhead onto a clip.' };
		if (error) return { title: 'Couldn’t measure the frame', detail: error };
		if (!shown) return { title: 'Measuring the frame at the playhead…' };
		return null;
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
		const style = getComputedStyle(el);
		return {
			trace: tone('--scope-trace'),
			red: tone('--scope-red'),
			green: tone('--scope-green'),
			blue: tone('--scope-blue'),
			grid: resolveColor('--scope-graticule', el),
			label: resolveColor('--scope-label', el),
			// A canvas cannot read `var(--font-mono)` or `var(--text-2xs)`; the theme's face and size are resolved here
			// with the colours.
			font: style.getPropertyValue('--font-mono').trim() || 'monospace',
			size: parseFloat(style.getPropertyValue('--text-2xs')) || 10,
			redCss: resolveColor('--scope-red', el),
			greenCss: resolveColor('--scope-green', el),
			blueCss: resolveColor('--scope-blue', el),
			traceCss: resolveColor('--scope-trace', el)
		};
	});

	/** Room for the level numbers beside a waveform or parade, and for the histogram's under it, px. */
	const GUTTER = 26;
	const FOOT = 14;

	$effect(() => {
		const c = canvas;
		const p = palette;
		const s = shown;
		const { w, h } = box;
		const k = kind;
		if (!c || !p || w <= 0 || h <= 0) return;
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
		const px = p.size * dpr;
		ctx.font = `${Math.round(px)}px ${p.font}`;
		const put = (img: Pixels, x: number, y: number) => {
			const data = ctx.createImageData(img.width, img.height);
			data.data.set(img.data);
			ctx.putImageData(data, x, y);
		};
		const line = (x1: number, y1: number, x2: number, y2: number) => {
			ctx.beginPath();
			ctx.moveTo(x1, y1);
			ctx.lineTo(x2, y2);
			ctx.stroke();
		};
		const text = (t: string, x: number, y: number, align: CanvasTextAlign, base: CanvasTextBaseline = 'middle') => {
			ctx.textAlign = align;
			ctx.textBaseline = base;
			ctx.fillText(t, x, y);
		};
		const numbered = scaleSide(k) === 'left';
		const gutter = numbered ? Math.round(GUTTER * dpr) : 0;
		const foot = scaleSide(k) === 'foot' ? Math.round(FOOT * dpr) : 0;
		const top = pad;
		const rows = H - 2 * pad - foot;
		// The ruled levels (0 to 100 %), and their numbers in the gutter on the first plot's left.
		const levels = (x0: number, width: number, numbers: boolean) => {
			ctx.strokeStyle = p.grid;
			for (const v of LEVELS) {
				const y = Math.round(levelY(v, top, rows)) + 0.5;
				line(x0, y, x0 + width, y);
			}
			if (!numbers) return;
			ctx.fillStyle = p.label;
			for (const v of LEVELS) text(String(v), x0 - Math.round(4 * dpr), labelY(levelY(v, top, rows), px, H), 'right');
		};
		if (k === 'waveform') {
			const plot = W - gutter;
			if (s?.waveform) {
				const img = blankPixels(plot, rows);
				paintGrid(img, s.waveform, p.trace);
				put(img, gutter, top);
			}
			levels(gutter, plot, true);
		} else if (k === 'parade') {
			const plot = W - gutter;
			const gap = Math.round(6 * dpr);
			const each = Math.floor((plot - 2 * gap) / 3);
			if (s?.parade) {
				const img = blankPixels(plot, rows);
				s.parade.forEach((g, i) => paintGrid(img, g, [p.red, p.green, p.blue][i], i * (each + gap), each));
				put(img, gutter, top);
			}
			for (let i = 0; i < 3; i++) levels(gutter + i * (each + gap), each, i === 0);
		} else if (k === 'vectorscope') {
			const side = Math.max(1, Math.min(W, H) - 2 * pad);
			const x0 = Math.round((W - side) / 2);
			const y0 = Math.round((H - side) / 2);
			const at = (cb: number, cr: number) => {
				const q = vectorPoint(cb, cr, side);
				return { x: x0 + q.x, y: y0 + q.y };
			};
			if (s?.vectorscope) {
				const img = blankPixels(side, side);
				paintGrid(img, s.vectorscope, p.trace);
				put(img, x0, y0);
			}
			ctx.strokeStyle = p.grid;
			const centre = at(0, 0);
			const full = at(0.5, 0).x - centre.x;
			for (const r of RINGS) {
				ctx.beginPath();
				ctx.arc(centre.x, centre.y, full * r.share, 0, Math.PI * 2);
				ctx.stroke();
			}
			// The skin-tone line, 123° from +Cb.
			const a = (123 * Math.PI) / 180;
			line(centre.x, centre.y, centre.x + Math.cos(a) * full, centre.y - Math.sin(a) * full);
			ctx.fillStyle = p.label;
			const m = s?.summary.matrix ?? 'bt709';
			const r = Math.round(4 * dpr);
			for (const t of TARGETS) {
				const [, cb, cr] = ycbcr(m, t.rgb[0], t.rgb[1], t.rgb[2]);
				const q = at(cb, cr);
				ctx.strokeRect(q.x - r, q.y - r, 2 * r, 2 * r);
				text(t.name, q.x + r + 2, q.y + r, 'left', 'alphabetic');
			}
			if (showsScale(side / dpr)) {
				// The numbers, along a line that no target sits on (22° up from +Cb); the skin line is named at its end.
				const b = (22 * Math.PI) / 180;
				for (const ring of RINGS) {
					const d = full * ring.share;
					text(ring.label, centre.x + Math.cos(b) * d, centre.y - Math.sin(b) * d, 'center');
				}
				text('Skin', centre.x + Math.cos(a) * full - 2, centre.y - Math.sin(a) * full - Math.round(px * 0.8), 'right', 'alphabetic');
			}
		} else if (k === 'histogram') {
			const usable = rows;
			const bar = W / 256;
			ctx.strokeStyle = p.grid;
			for (const v of LEVELS.slice(1, -1)) line(Math.round((W * v) / 100) + 0.5, top, Math.round((W * v) / 100) + 0.5, top + usable);
			if (s?.histogram) {
				const hh = histogramHeights(s.histogram, ['red', 'green', 'blue', 'luma']);
				ctx.globalCompositeOperation = 'lighter';
				for (const [ch, css] of [
					['red', p.redCss],
					['green', p.greenCss],
					['blue', p.blueCss]
				] as const) {
					ctx.fillStyle = css;
					ctx.globalAlpha = 0.6;
					hh[ch].forEach((v, i) => {
						if (v > 0) ctx.fillRect(i * bar, top + usable * (1 - v), Math.max(1, bar), usable * v);
					});
				}
				ctx.globalAlpha = 1;
				ctx.globalCompositeOperation = 'source-over';
				ctx.strokeStyle = p.traceCss;
				ctx.beginPath();
				hh.luma.forEach((v, i) => {
					const x = i * bar + bar / 2;
					const y = top + usable * (1 - v);
					if (i === 0) ctx.moveTo(x, y);
					else ctx.lineTo(x, y);
				});
				ctx.stroke();
			}
			ctx.strokeStyle = p.grid;
			line(0, top + usable + 0.5, W, top + usable + 0.5);
			ctx.fillStyle = p.label;
			for (const v of LEVELS) {
				const x = (W * v) / 100;
				text(String(v), x, top + usable + Math.round(2 * dpr), v === 0 ? 'left' : v === 100 ? 'right' : 'center', 'top');
			}
		}
	});

	const lines = $derived(shown ? readoutLines(kind, shown.summary) : []);
	const legend = $derived(shown ? legendParts(shown.summary, timeDisplay.fmt(shownAt)) : []);
	const options = $derived(kindOptions(panelWidth));
	const look = $derived(kindLook(panelWidth));
	const stop = $derived(tabStop(SCOPE_KINDS.map((k) => ({ checked: k.id === kind, enabled: true }))));
	/** The legend's pieces that fit its line (the plot's width less the panel's padding). */
	const legendLine = $derived(fitLegend(legend, panelWidth - 16));
	const full = $derived(SCOPE_KINDS.find((k) => k.id === kind)?.label ?? '');
</script>

<div class="scopes" bind:this={root}>
	{#if look === 'icons'}
		<!-- No word fits a row of four at this width: the four scopes are icons (their names in the tooltips), and the name of the
		     one that is on is written beside them. One tab stop, the arrows move through it, as the chips do. -->
		<div class="kinds" role="radiogroup" aria-label="Scope" use:roving={{ follow: true }}>
			{#each SCOPE_KINDS as k, i (k.id)}
				<IconBtn
					size={28}
					role="radio"
					aria-checked={k.id === kind}
					tabindex={i === stop ? 0 : -1}
					data-on={k.id === kind ? '' : undefined}
					data-testid="scope-{k.id}"
					title={k.hint}
					onclick={() => choose(k.id)}><Icon n={k.icon} s={15} /></IconBtn
				>
			{/each}
			<span class="kindname" aria-hidden="true">{full}</span>
		</div>
	{:else}
		<ChoiceChips label="Scope" {options} value={kind} onpick={choose} testid="scope" />
	{/if}
	<div class="plot" bind:this={host}>
		<canvas bind:this={canvas} aria-label="{full} of the frame at the playhead"></canvas>
		{#if note}
			<div class="note" role={error && subject === 'picture' ? 'alert' : 'status'}>
				<div class="msg">
					<strong>{note.title}</strong>
					{#if note.detail}<span>{note.detail}</span>{/if}
				</div>
			</div>
		{/if}
	</div>
	{#if shown}
		<div class="readout" role="group" aria-label="What the scope reads">
			<dl>
				{#each lines as line (line.label)}
					<dt>{line.label}</dt>
					<dd class:warn={line.warn}>{line.text}</dd>
				{/each}
			</dl>
			<p class="legend" use:tip={legendLine.length < legend.length ? legend.join(' · ') : undefined}>
				{#each [...legendLine, ...(updating ? ['updating…'] : [])] as part, i (i)}{#if i > 0}{' '}<span aria-hidden="true">·</span>{' '}{/if}<span class="part">{part}</span>{/each}
			</p>
		</div>
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
		overflow: hidden;
		background: var(--surface-panel);
	}
	.plot {
		position: relative;
		flex: 1 1 120px;
		min-height: 88px;
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
	/* On the matte, which is dark in every theme: the label token, not the panel's own muted text. The message has the
	   matte behind it, so the graticule's lines do not strike through it. */
	.note {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		padding: 8px;
		pointer-events: none;
	}
	.msg {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 4px;
		max-width: 100%;
		padding: 6px 12px;
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--frame-matte) 85%, transparent);
		text-align: center;
		font-size: var(--text-sm);
		line-height: 1.4;
		color: var(--scope-label);
	}
	.msg strong {
		font-weight: 500;
	}
	.msg span {
		font-size: var(--text-xs);
		opacity: 0.85;
	}
	/* The readout is what gives way when the panel is short: it scrolls, the plot keeps its height. */
	.readout {
		flex: 0 1 auto;
		min-height: 0;
		overflow-y: auto;
		overscroll-behavior: contain;
	}
	dl {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 2px 8px;
		margin: 0;
		font-size: var(--text-xs);
		line-height: 1.4;
	}
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
		min-width: 0;
		font-family: var(--font-mono);
		color: var(--text-secondary);
	}
	dd.warn {
		color: var(--warning);
	}
	.legend .part {
		white-space: nowrap;
	}
	.kinds {
		flex: none;
		display: flex;
		align-items: center;
		gap: 4px;
	}
	.kindname {
		margin-left: 6px;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: var(--text-sm);
		color: var(--text-secondary);
	}
	.legend {
		margin: 4px 0 0;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-muted);
	}
</style>
