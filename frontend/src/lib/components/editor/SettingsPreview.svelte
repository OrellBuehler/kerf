<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import Switch from './Switch.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import { settings } from '$lib/settings.svelte';
	import { gpuPreview } from '$lib/gpu-preview.svelte';
	import { PREVIEW_SOURCES, PROXY_SIZES } from '$lib/proxy-info';

	// What the GPU preview is doing on this machine; read when its section shows.
	const gpuStatus = $derived(gpuPreview.status);
	$effect(() => {
		void gpuPreview.refresh();
	});
</script>

<SettingRow id="proxy-size" label="Preview proxies" stacked>
	{#snippet hint()}
		Kerf makes a lightweight copy of each video so scrubbing stays smooth, even with 4K or 5K footage. Exports always
		use your original files.
	{/snippet}
	<ChoiceChips
		label="Proxy size"
		options={PROXY_SIZES.map((p) => ({ value: p.size, label: p.label, hint: p.hint }))}
		value={settings.proxySize}
		onpick={(size) => void settings.setProxySize(size)}
		follow={false}
		testid="proxy-size"
	/>
	<p style="margin:8px 0 0;font-size:12px;line-height:1.55;color:var(--text-muted)">
		Width of the copy in pixels. 1280 is the default: sharp enough to judge framing, light enough to scrub. 360 footage
		keeps its wider ratio. Copies made at another size stay on disk until you delete them from the bin.
	</p>
</SettingRow>

<SettingRow id="preview-source" label="Preview source" stacked>
	{#snippet hint()}
		{settings.proxySize === 0
			? 'Proxies are off, so the preview decodes the original.'
			: PREVIEW_SOURCES.find((p) => p.source === settings.previewSource)?.hint}
	{/snippet}
	<ChoiceChips
		label="Preview source"
		options={PREVIEW_SOURCES.map((p) => ({
			value: p.source,
			label: p.label,
			hint: p.hint,
			disabled: settings.proxySize === 0 && p.source !== 'original'
		}))}
		value={settings.previewSource}
		onpick={(source) => void settings.setPreviewSource(source)}
		testid="preview-source"
	/>
</SettingRow>

<SettingRow id="safe-areas" for="sw-safe-areas" label="Shade the safe areas over the preview">
	{#snippet hint()}
		Where a phone's own interface covers a vertical or square cut: the status strip at the top, the caption rail along
		the bottom and the action buttons on the right, plus a title-safe box inset from every edge. Keep faces and text out
		of the shaded zones. A guide only — nothing is cropped, and a 16:9 project draws none. The preview's right-click
		menu toggles the same setting.
	{/snippet}
	<Switch
		id="sw-safe-areas"
		label="Shade the safe areas over the preview"
		checked={settings.safeAreas}
		onchange={(on) => void settings.setSafeAreas(on)}
	/>
</SettingRow>

<SettingRow id="gpu-preview" for="sw-gpu-preview" label="GPU preview (experimental)">
	{#snippet hint()}
		Draws frames the GPU compositor can render exactly in a native surface instead of FFmpeg's JPEG, including fades,
		dissolves, dips, slides and pushes. Anything it can't draw, such as an unsupported effect or a dip to white it
		hasn't measured, is still drawn by FFmpeg. Playback is drawn on the GPU too when everything in the next five seconds
		is something it can draw exactly, and is FFmpeg's stream otherwise. The status bar shows which renderer made the
		frame you're looking at.
		{#if gpuStatus}
			<span
				data-testid="gpu-preview-status"
				style="display:block;margin-top:8px;color:var(--text-muted);font-family:var(--font-mono)"
			>
				{#if !gpuStatus.supported}
					Not available here: {gpuStatus.reason ?? 'no native surface on this platform'}
				{:else if gpuStatus.ready}
					{gpuStatus.technique === 'child' ? 'Child window' : 'Window surface'} · {gpuStatus.adapter}{gpuStatus.software
						? ' (software)'
						: ''}
				{:else if settings.gpuPreview}
					{gpuStatus.reason ?? 'Starts with the next frame.'}
				{:else if gpuStatus.reason}
					{gpuStatus.reason}
				{:else}
					Available: {gpuStatus.technique === 'child' ? 'a child window over the preview' : 'the window surface under the preview'}.
				{/if}
			</span>
		{/if}
		{#if gpuStatus?.supported && gpuStatus.technique === 'child'}
			<span style="display:block;margin-top:6px;color:var(--text-muted)">
				On this platform the surface is a window over the preview, so a frame with a title box, the trim monitor or
				safe-area guides on it is FFmpeg's.
			</span>
		{/if}
	{/snippet}
	<Switch
		id="sw-gpu-preview"
		label="Draw the preview on the GPU"
		checked={settings.gpuPreview}
		onchange={(on) => void settings.setGpuPreview(on)}
	/>
</SettingRow>
