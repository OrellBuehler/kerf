<script lang="ts">
	// The library asset the Inspector is on, to look at and listen to before it is on the timeline: its picture, a
	// still wherever the pointer is over it (so a clip is told from the next without playing it), and Play. Starting it
	// stops the timeline's transport, and the transport starting stops it: one thing plays.
	import { onDestroy, untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { SourcePlayer } from '#lib/source-player.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { thumbnails } from '#lib/thumbnails';
	import { audioStream, fmtDuration, videoStream, type MediaInfo } from '#lib/media-info';
	import { tidyReason } from '#lib/failure-copy';
	import type { Asset } from '#lib/types';

	let { asset, info, missing = false }: { asset: Asset; info: MediaInfo; missing?: boolean } = $props();

	const player = new SourcePlayer();
	onDestroy(() => player.dispose());

	const v = $derived(videoStream(asset));
	const ratio = $derived(v?.width && v?.height ? v.width / v.height : 16 / 9);
	const playable = $derived(!missing && info.kind !== 'image' && asset.duration > 0);
	const step = $derived(v?.fps ? 1 / v.fps : 0.01);

	// By value, not by the asset object: a tag put on while it plays hands a new object over and must not stop it.
	const id = $derived(asset.id);
	const duration = $derived(asset.duration);
	const kind = $derived(info.kind);
	const sound = $derived(!!audioStream(asset));
	const fps = $derived(v?.fps ?? 0);
	$effect(() => {
		const source = missing ? null : { id, duration, video: kind === 'video', audio: sound, fps };
		untrack(() => player.load(source, thumbnails.peek(id) ?? null));
	});

	$effect(() => {
		if (ui.playing) untrack(() => player.pause());
	});

	/** Where over the picture the pointer is, 0 to 1; `null` when it is not. */
	let over = $state<number | null>(null);
	const fraction = (e: PointerEvent | MouseEvent) => {
		const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
		return r.width > 0 ? Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) : 0;
	};

	let resume = false;

	function toggle(e: MouseEvent) {
		// A pointer click must not leave focus on the button: a focused button swallows Space, the transport's key.
		if (e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();
		if (!player.playing && ui.playing) ui.pause();
		player.toggle();
	}
</script>

<div data-testid="asset-preview" style="margin-top:10px">
	{#if info.kind !== 'audio'}
		<div style="display:grid;place-items:center;background:var(--surface-void);border-radius:var(--radius-sm);overflow:hidden">
			<div
				role="presentation"
				onpointermove={(e) => {
					if (!playable || player.playing) return;
					over = fraction(e);
					player.hover(over * asset.duration);
				}}
				onpointerleave={() => {
					over = null;
					player.hover(null);
				}}
				onclick={(e) => playable && player.seek(fraction(e) * asset.duration)}
				style="position:relative;width:min(100%,calc(320px * {ratio}));aspect-ratio:{ratio};display:grid;place-items:center;color:var(--text-muted);cursor:{playable ? 'ew-resize' : 'default'}"
			>
				{#if player.frame}
					<img src={player.frame} alt="" draggable="false" decoding="async" style="width:100%;height:100%;object-fit:contain;display:block" />
				{:else}
					<Icon n={missing ? 'alert-triangle' : info.kind === 'image' ? 'image' : 'video'} s={22} />
				{/if}
				{#if over !== null && !player.playing}
					<span
						style="position:absolute;top:0;bottom:0;left:{over * 100}%;width:1px;background:var(--kerf-400);pointer-events:none"
					></span>
					<span
						style="position:absolute;left:4px;bottom:4px;padding:1px 4px;border-radius:var(--radius-sm);background:color-mix(in srgb,var(--surface-void) 72%,transparent);color:var(--text-on-video);font-family:var(--font-mono);font-size:10px;line-height:1.4;font-variant-numeric:tabular-nums;pointer-events:none"
						>{fmtDuration(over * asset.duration)}</span
					>
				{/if}
			</div>
		</div>
	{/if}
	{#if playable}
		<div style="margin-top:6px;display:flex;align-items:center;gap:6px">
			<IconBtn title={player.playing ? 'Pause' : 'Play this media'} size={26} data-testid="asset-preview-play" onclick={toggle}>
				<Icon n={player.playing ? 'pause' : 'play'} s={14} />
			</IconBtn>
			<input
				type="range"
				aria-label="Position in this media"
				min="0"
				max={asset.duration}
				{step}
				value={player.time}
				oninput={(e) => {
					// A drag shows stills; the stream starts again once, where the drag ends.
					if (player.playing) {
						resume = true;
						player.pause();
					}
					player.seek(Number(e.currentTarget.value));
				}}
				onchange={() => {
					if (!resume) return;
					resume = false;
					player.play();
				}}
				style="flex:1;min-width:0"
			/>
			<span
				style="flex:none;font-family:var(--font-mono);font-size:11px;font-variant-numeric:tabular-nums;color:var(--text-muted);white-space:nowrap"
				>{fmtDuration(player.time)} / {info.duration}</span
			>
			{#if sound}
				<IconBtn
					title={player.muted ? 'Turn this preview’s sound on' : 'Mute this preview'}
					size={26}
					onclick={() => player.setMuted(!player.muted)}
				>
					<Icon n={player.muted ? 'volume-x' : 'volume-2'} s={14} />
				</IconBtn>
			{/if}
		</div>
	{/if}
	{#if player.error}
		<div role="alert" style="margin-top:4px;font-size:11px;line-height:1.4;color:var(--text-muted)">
			Couldn't play it: {tidyReason(player.error)}
		</div>
	{/if}
</div>
