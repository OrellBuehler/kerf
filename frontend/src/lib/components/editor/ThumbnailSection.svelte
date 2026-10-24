<script lang="ts">
	// The Deliver panel's Thumbnails: pictures that front the video with graphics of their own (a headline, a
	// template) and never appear in it. Pick one, its frame (the playhead or a suggestion), its size, what of the
	// video's own graphics the background keeps, and its layers; the preview is the engine's drawing of it, from
	// the proxies. Every change is one `set_thumbnail` edit (a field on change, never per keystroke).
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { listTemplates, suggestThumbnailFrames, thumbnailPreview } from '#lib/api';
	import { report } from '#lib/ops';
	import { saveThumbnail } from '#lib/file-actions';
	import { toast } from '#lib/notifications.svelte';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { HEADLINE_ACCENT, HEADLINE_COLOR, swatchHex } from '#lib/title-color';
	import { HEADLINE_PLACEMENTS, THUMBNAIL_SIZES, keptLayers, layerKind, placementOf, sizeName } from '#lib/thumbnail-design';
	import type { HeadlinePlacement } from '#lib/thumbnail-design';
	import type { SuggestedFrame, TemplateInfo, ThumbnailLayer, ThumbnailLayerSpec, ThumbnailSpec, VideoGraphics } from '#lib/types';

	const thumbnails = $derived(editor.timeline.thumbnails ?? []);
	let chosen = $state<string | null>(null);
	const current = $derived(thumbnails.find((t) => t.id === chosen) ?? thumbnails[0] ?? null);
	const off = $derived(editor.locked || editor.busy);

	const GRAPHICS: { value: VideoGraphics; label: string }[] = [
		{ value: 'titles', label: 'Titles, no captions' },
		{ value: 'none', label: 'Footage only' },
		{ value: 'all', label: 'Everything the video shows' }
	];
	const PLACES: { value: HeadlinePlacement; label: string }[] = [
		{ value: 'top', label: 'Top' },
		{ value: 'middle', label: 'Middle' },
		{ value: 'bottom', label: 'Bottom' }
	];

	async function change(spec: ThumbnailSpec, id: string | null = current?.id ?? null) {
		try {
			const done = await editor.setThumbnail(id, spec);
			chosen = done.id;
			for (const w of done.warnings) toast.info(w);
		} catch (e) {
			report(e);
		}
	}

	/** The stack with one layer changed (`patch` over keeping it) or left out (`patch` null). */
	function withLayer(layer: ThumbnailLayer, patch: ThumbnailLayerSpec | null): ThumbnailLayerSpec[] {
		if (!current) return [];
		return keptLayers(current).flatMap((k) => (k.layer_id !== layer.id ? [k] : patch ? [{ ...k, ...patch }] : []));
	}
	const editLayer = (layer: ThumbnailLayer, patch: ThumbnailLayerSpec | null) => change({ layers: withLayer(layer, patch) });
	const addLayer = (spec: ThumbnailLayerSpec) => current && change({ layers: [...keptLayers(current), spec] });
	const headline = (text: string) => ({ text, color: HEADLINE_COLOR, accent: HEADLINE_ACCENT });
	function retext(layer: ThumbnailLayer, patch: { text?: string; subtitle?: string; color?: string }) {
		const was = layer.text ?? headline('');
		const next = { ...was, ...patch };
		if (!next.text.trim()) return;
		void editLayer(layer, { text: { ...next, subtitle: next.subtitle?.trim() ? next.subtitle : null } });
	}

	// The preview: the engine's drawing, asked for a moment after the thumbnail or the cut last changed.
	let preview = $state<string | null>(null);
	let drawing = $state(false);
	let previewError = $state<string | null>(null);
	$effect(() => {
		const t = current;
		void editor.timeline;
		if (!t) return;
		const key = JSON.stringify(t);
		let gone = false;
		const timer = setTimeout(async () => {
			drawing = true;
			try {
				const url = await thumbnailPreview(t.id, Math.min(t.width, 480));
				if (gone || JSON.stringify(current) !== key) {
					if (url) URL.revokeObjectURL(url);
					return;
				}
				if (preview) URL.revokeObjectURL(preview);
				preview = url;
				previewError = null;
			} catch (e) {
				if (!gone) previewError = e instanceof Error ? e.message : String(e);
			} finally {
				if (!gone) drawing = false;
			}
		}, 300);
		return () => {
			gone = true;
			clearTimeout(timer);
		};
	});

	let suggestions = $state<SuggestedFrame[]>([]);
	let suggesting = $state(false);
	async function suggest() {
		suggesting = true;
		try {
			suggestions = await suggestThumbnailFrames(6);
		} catch (e) {
			report(e);
		} finally {
			suggesting = false;
		}
	}
	function useTime(time: number) {
		ui.seek(time);
		void change({ time });
	}

	let templates = $state<TemplateInfo[]>([]);
	$effect(() => {
		void listTemplates()
			.then((list) => (templates = list.templates))
			.catch(() => (templates = []));
	});
</script>

{#if !current}
	<div style="display:flex;flex-direction:column;gap:8px;font-size:12px;color:var(--text-muted);line-height:1.45">
		<span>A picture that fronts the video with a headline or a template of its own, never drawn in the cut.</span>
		<Btn size="sm" icon="plus" pending={off} onclick={() => change({ time: ui.time }, null)} data-testid="thumbnail-new">
			New thumbnail from the playhead
		</Btn>
	</div>
{:else}
	<div style="display:flex;flex-direction:column;gap:9px;font-size:12px;color:var(--text-secondary)">
		<div style="display:flex;align-items:center;gap:6px">
			<select
				class="k-select"
				style="flex:1"
				aria-label="Thumbnail"
				value={current.id}
				onchange={(e) => (chosen = e.currentTarget.value)}
			>
				{#each thumbnails as t (t.id)}
					<option value={t.id}>{t.name} · {t.width}×{t.height}</option>
				{/each}
			</select>
			<IconBtn size={28} title="New thumbnail from the playhead" pending={off} onclick={() => change({ time: ui.time }, null)}>
				<Icon n="plus" s={14} />
			</IconBtn>
			<IconBtn
				size={28}
				title="Remove this thumbnail"
				pending={off}
				onclick={() => editor.removeThumbnail(current.id).catch(report)}
			>
				<Icon n="trash" s={14} />
			</IconBtn>
		</div>

		<div
			data-testid="thumbnail-preview"
			style="position:relative;width:100%;max-height:280px;aspect-ratio:{current.width} / {current.height};margin:0 auto;background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-sm);overflow:hidden;display:flex;align-items:center;justify-content:center"
		>
			{#if preview}
				<img src={preview} alt="Thumbnail “{current.name}”" style="width:100%;height:100%;object-fit:contain;opacity:{drawing ? 0.7 : 1}" />
			{:else}
				<span style="padding:8px;text-align:center;color:var(--text-muted)"
					>{previewError ?? (drawing ? 'Drawing…' : 'The thumbnail is drawn in the desktop app.')}</span
				>
			{/if}
		</div>

		<label style="display:flex;align-items:center;gap:8px">
			<span style="width:64px;color:var(--text-muted)">Name</span>
			<input
				type="text"
				style="flex:1"
				value={current.name}
				readonly={editor.locked}
				onchange={(e) => change({ name: e.currentTarget.value })}
			/>
		</label>

		<div style="display:flex;align-items:center;gap:8px">
			<span style="width:64px;color:var(--text-muted)">Frame</span>
			<span style="font-family:var(--font-mono)">{timeDisplay.fmt(current.time)}</span>
			<span style="flex:1"></span>
			<Btn size="sm" pending={off} onclick={() => useTime(ui.time)}>Use playhead</Btn>
			<Btn size="sm" icon="sparkles" pending={suggesting} onclick={suggest}>{suggesting ? 'Looking…' : 'Suggest'}</Btn>
		</div>
		{#if suggestions.length > 0}
			<div role="group" aria-label="Suggested frames" style="display:flex;flex-wrap:wrap;gap:6px;padding-left:72px">
				{#each suggestions as s (s.time)}
					<Btn
						size="sm"
						variant="ghost"
						pending={off}
						title={s.score > 0 ? `Score ${s.score.toFixed(2)}: sharpness ${s.sharpness}, exposure ${s.exposure}, contrast ${s.contrast}` : undefined}
						onclick={() => useTime(s.time)}>{timeDisplay.fmt(s.time)}</Btn
					>
				{/each}
			</div>
		{/if}

		<div style="display:flex;align-items:flex-start;gap:8px">
			<span style="width:64px;flex:none;color:var(--text-muted);line-height:28px">Size</span>
			<ChoiceChips
				label="Thumbnail size"
				options={THUMBNAIL_SIZES.map((s) => ({ value: s.id as string, label: s.label, hint: `${s.width}×${s.height}` }))}
				value={sizeName(current.width, current.height)}
				onpick={(size) => change({ size })}
			/>
		</div>

		<label style="display:flex;align-items:center;gap:8px">
			<span style="width:64px;color:var(--text-muted)">Graphics</span>
			<select
				class="k-select"
				style="flex:1"
				aria-label="The video's own graphics in the background"
				value={current.graphics ?? 'titles'}
				onchange={(e) => change({ graphics: e.currentTarget.value as VideoGraphics })}
			>
				{#each GRAPHICS as g (g.value)}
					<option value={g.value}>{g.label}</option>
				{/each}
			</select>
		</label>

		{#each [['focus_x', 'Focus ←→'], ['focus_y', 'Focus ↑↓']] as [axis, label] (axis)}
			<label style="display:flex;align-items:center;gap:8px">
				<span style="width:64px;color:var(--text-muted)">{label}</span>
				<input
					type="range"
					min="0"
					max="1"
					step="0.01"
					style="flex:1"
					aria-label="Where the crop is centred, {axis === 'focus_x' ? 'across' : 'down'} the frame"
					value={axis === 'focus_x' ? current.focus_x : current.focus_y}
					disabled={editor.locked}
					onchange={(e) => change({ [axis]: Number(e.currentTarget.value) })}
				/>
			</label>
		{/each}

		<span class="k-overline" style="margin-top:4px">Layers</span>
		{#each current.layers ?? [] as layer (layer.id)}
			<div
				data-testid="thumbnail-layer"
				style="display:flex;flex-direction:column;gap:6px;padding:8px;border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-sm)"
			>
				<div style="display:flex;align-items:center;gap:6px">
					<span style="flex:1;color:var(--text-primary);overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{layer.name}</span>
					<span style="color:var(--text-muted)">{layerKind(layer)}</span>
					<IconBtn size={24} title="Remove this layer" pending={off} onclick={() => editLayer(layer, null)}>
						<Icon n="x" s={12} />
					</IconBtn>
				</div>
				{#if layer.text}
					<textarea
						rows="2"
						aria-label="Headline"
						value={layer.text.text}
						readonly={editor.locked}
						onchange={(e) => retext(layer, { text: e.currentTarget.value })}
					></textarea>
					<div style="display:flex;align-items:center;gap:6px">
						<input
							type="text"
							style="flex:1"
							placeholder="Subtitle"
							aria-label="Subtitle"
							value={layer.text.subtitle ?? ''}
							readonly={editor.locked}
							onchange={(e) => retext(layer, { subtitle: e.currentTarget.value })}
						/>
						<input
							type="color"
							aria-label="Headline colour"
							value={swatchHex(layer.text.color)}
							disabled={editor.locked}
							onchange={(e) => retext(layer, { color: e.currentTarget.value })}
						/>
					</div>
				{:else if layer.template}
					{#each layer.template.controls.filter((c) => c.kind !== 'image') as c (c.id)}
						<label style="display:flex;align-items:center;gap:8px">
							<span style="width:64px;color:var(--text-muted);overflow:hidden;text-overflow:ellipsis">{c.label}</span>
							<input
								type="text"
								style="flex:1"
								value={layer.template.values[c.id] ?? ''}
								readonly={editor.locked}
								onchange={(e) => editLayer(layer, { values: { [c.id]: e.currentTarget.value } })}
							/>
						</label>
					{/each}
				{/if}
				<ChoiceChips
					label="Where the layer sits"
					options={PLACES}
					value={placementOf(layer) ?? ('' as HeadlinePlacement)}
					onpick={(p) => editLayer(layer, { ...HEADLINE_PLACEMENTS[p] })}
				/>
			</div>
		{/each}
		<div style="display:flex;align-items:center;gap:6px;flex-wrap:wrap">
			<Btn size="sm" icon="plus" pending={off} onclick={() => addLayer({ text: headline('Headline') })}>Headline</Btn>
			{#if templates.length > 0}
				<select
					class="k-select"
					style="flex:1"
					aria-label="Add a template layer"
					value=""
					disabled={off}
					onchange={(e) => {
						const id = e.currentTarget.value;
						e.currentTarget.value = '';
						if (id) void addLayer({ template_id: id, x: 0, y: 0, width: 1, height: 1 });
					}}
				>
					<option value="">Add a template…</option>
					{#each templates as t (t.id)}
						<option value={t.id}>{t.name}</option>
					{/each}
				</select>
			{/if}
		</div>

		<Btn variant="primary" size="md" icon="download" pending={editor.coverRun !== null} onclick={() => saveThumbnail(current.id, current.name)}>
			Save thumbnail…
		</Btn>
	</div>
{/if}
