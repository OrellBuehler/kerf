<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { RULER, SCENE_X, V1_REVIEW, V1_EDIT, A1, type ClipBlock, type DiffState } from './data';

	const review = $derived(ui.phase === 'review');
	const showClips = $derived(ui.phase === 'review' || ui.phase === 'editing');
	const v1 = $derived(review ? V1_REVIEW : V1_EDIT);

	function positioned(clips: ClipBlock[]) {
		let x = 4;
		return clips.map((c) => {
			const left = x;
			x += c.w + 2;
			return { c, left };
		});
	}

	const pv1 = $derived(positioned(v1));
	const pa1 = $derived(positioned(A1));

	function diffStyle(kind: 'video' | 'audio', diff: DiffState): string {
		if (diff === 'keep')
			return 'background:var(--track-video);border:1.5px solid var(--diff-add);box-shadow:0 0 0 1px var(--diff-add-surface);';
		if (diff === 'cut')
			return 'background:var(--diff-remove-surface);border:1.5px dashed var(--diff-remove);';
		return kind === 'audio'
			? 'background:var(--track-audio);border:1px solid var(--track-audio-edge);'
			: 'background:var(--track-video);border:1px solid var(--track-video-edge);';
	}
</script>

{#snippet trackHead(label: string, sub: string, icons: string[], h: string)}
	<div
		style="height:{h};border-bottom:1px solid var(--border-subtle);display:flex;align-items:center;gap:8px;padding:0 10px"
	>
		<span style="font-family:var(--font-mono);font-size:11px;font-weight:600;color:var(--text-secondary);width:18px"
			>{label}</span
		>
		<span style="font-size:11px;color:var(--text-muted);flex:1">{sub}</span>
		<div style="display:flex;gap:4px">
			{#each icons as ic (ic)}<Icon n={ic} s={12} color="var(--text-disabled)" />{/each}
		</div>
	</div>
{/snippet}

{#snippet clip(c: ClipBlock, left: number, kind: 'video' | 'audio', diff: DiffState)}
	<div
		style="position:absolute;left:{left}px;top:5px;height:calc(100% - 10px);width:{c.w}px;border-radius:2px;overflow:hidden;display:flex;align-items:center;padding:0 7px;cursor:pointer;{diffStyle(
			kind,
			diff
		)}"
	>
		{#if kind === 'audio'}
			<div
				style="position:absolute;inset:0;background:repeating-linear-gradient(90deg, var(--waveform) 0 1px, transparent 1px 3px);opacity:.4;mask-image:linear-gradient(transparent 28%, #000 28%, #000 72%, transparent 72%)"
			></div>
		{/if}
		<span
			style="position:relative;font-size:10px;font-weight:600;color:{diff === 'cut'
				? 'var(--red-400)'
				: 'rgba(255,255,255,.92)'};white-space:nowrap;overflow:hidden;text-overflow:ellipsis;text-decoration:{diff ===
			'cut'
				? 'line-through'
				: 'none'}">{c.label}</span
		>
	</div>
{/snippet}

<div
	style="height:296px;flex:none;border-top:1px solid var(--border-default);background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>
	<!-- timeline toolbar -->
	<div
		style="height:34px;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-subtle);flex:none"
	>
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>Timeline</span
		>
		{#if review}<Badge tone="agent" dot>AI preview · not applied</Badge>{/if}
		{#if ui.phase === 'editing'}<Badge tone="success" dot>Cut applied</Badge>{/if}
		<div style="flex:1"></div>
		<Icon n="search" s={13} color="var(--text-muted)" />
		<div style="width:90px;height:4px;border-radius:999px;background:var(--surface-inset);position:relative">
			<div style="position:absolute;inset:0 auto 0 0;width:55%;background:var(--neutral-600);border-radius:999px"></div>
		</div>
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)"
			>{ui.snap ? 'snap on' : 'snap off'}</span
		>
	</div>

	<div style="flex:1;display:flex;min-height:0">
		<!-- track headers -->
		<div
			style="width:var(--track-header-w);flex:none;border-right:1px solid var(--border-default);background:var(--surface-app)"
		>
			<div style="height:var(--ruler-h);border-bottom:1px solid var(--border-subtle)"></div>
			{@render trackHead('V1', 'Video', ['eye', 'lock'], 'var(--track-h-video)')}
			{@render trackHead('A1', 'Voiceover', ['volume-2', 'lock'], 'var(--track-h-audio)')}
			{@render trackHead('A2', 'Music', ['volume-2', 'lock'], 'var(--track-h-audio)')}
		</div>

		<!-- scrollable track area -->
		<div style="flex:1;overflow-x:auto;overflow-y:hidden;position:relative">
			<div style="min-width:760px;position:relative">
				<!-- ruler -->
				<div
					style="height:var(--ruler-h);border-bottom:1px solid var(--border-subtle);position:relative;background:var(--surface-app)"
				>
					{#each RULER as t, i (t)}
						<span
							style="position:absolute;left:{i * 92 +
								6}px;top:7px;font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{t}</span
						>
					{/each}
					{#if showClips}
						{#each SCENE_X as x (x)}
							<span
								title="Detected scene cut"
								style="position:absolute;left:{x}px;bottom:0;width:0;height:0;border-left:4px solid transparent;border-right:4px solid transparent;border-top:5px solid var(--scene-marker);transform:translateX(-50%)"
							></span>
						{/each}
					{/if}
				</div>

				<!-- grid lines -->
				{#each RULER as _, i (i)}
					<span
						style="position:absolute;left:{i *
							92}px;top:var(--ruler-h);bottom:0;width:1px;background:{i % 2
							? 'var(--timeline-grid)'
							: 'var(--timeline-grid-major)'}"
					></span>
				{/each}

				<!-- V1 -->
				<div
					style="height:var(--track-h-video);border-bottom:1px solid var(--border-subtle);position:relative"
				>
					{#if showClips}
						{#each pv1 as p (p.c.id)}
							{@render clip(p.c, p.left, 'video', review ? p.c.state : 'normal')}
						{/each}
					{/if}
				</div>

				<!-- A1 voiceover with waveform + silence region -->
				<div
					style="height:var(--track-h-audio);border-bottom:1px solid var(--border-subtle);position:relative"
				>
					{#if showClips}
						{#if review}
							<span
								title="Detected silence 1.8s"
								style="position:absolute;left:132px;top:4px;bottom:4px;width:34px;background:var(--silence-region);border:1px solid rgba(229,84,75,.3);border-radius:2px"
							></span>
						{/if}
						{#each pa1 as p (p.c.id)}
							{@render clip(p.c, p.left, 'audio', 'normal')}
						{/each}
					{/if}
				</div>

				<!-- A2 music -->
				<div style="height:var(--track-h-audio);position:relative">
					{#if showClips}
						<div
							style="position:absolute;left:4px;top:5px;height:calc(100% - 10px);width:588px;border-radius:2px;background:var(--track-audio);border:1px solid var(--track-audio-edge);display:flex;align-items:center;padding:0 8px;overflow:hidden"
						>
							<div
								style="position:absolute;inset:0;background:repeating-linear-gradient(90deg, var(--waveform) 0 1px, transparent 1px 4px);opacity:.35;mask-image:linear-gradient(transparent 30%, #000 30%, #000 70%, transparent 70%)"
							></div>
							<span
								style="position:relative;font-family:var(--font-mono);font-size:9px;color:rgba(255,255,255,.7)"
								>ambient_loop · −24 LUFS</span
							>
						</div>
					{/if}
				</div>

				<!-- playhead -->
				<div
					style="position:absolute;left:196px;top:0;bottom:0;width:2px;background:var(--playhead);box-shadow:0 0 10px 1px var(--playhead-glow);z-index:30"
				>
					<span
						style="position:absolute;top:-1px;left:-5px;width:12px;height:9px;background:var(--playhead);clip-path:polygon(0 0,100% 0,50% 100%)"
					></span>
				</div>
			</div>
		</div>
	</div>
</div>
