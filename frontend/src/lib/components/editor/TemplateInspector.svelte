<script lang="ts">
	// The Inspector's view of a template selected in the library: the animation playing, so it is known before it is on
	// the timeline, what it can change and the fonts it carries, and the ways onto the timeline the card's menu has.
	import { onDestroy, untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { addTemplateClipHere, addTemplateHere, deleteTemplateFile, exportTemplateFile, posterOf } from '#lib/template-actions';
	import { controlCount, fontsLine, kindLabel } from '#lib/template-ui';
	import { frameAt, framesOf } from '#lib/template-frames';
	import { fmtSize } from '#lib/bin-view';
	import { fmtDuration } from '#lib/media-info';
	import type { TemplateInfo } from '#lib/types';

	let { info }: { info: TemplateInfo } = $props();

	const ROW_ITEM = 'flex:1 1 0;min-width:max-content';
	const id = $derived(info.id);
	const duration = $derived(info.duration);
	let poster = $state<string | null>(null);
	let frames = $state<string[]>([]);
	let time = $state(0);
	let playing = $state(true);
	let raf = 0;

	function stop() {
		cancelAnimationFrame(raf);
		raf = 0;
	}

	function run() {
		stop();
		let last = performance.now();
		const step = (now: number) => {
			if (duration > 0) time = (time + (now - last) / 1000) % duration;
			last = now;
			raf = requestAnimationFrame(step);
		};
		raf = requestAnimationFrame(step);
	}

	$effect(() => {
		const [wanted, length] = [id, duration];
		let live = true;
		untrack(() => {
			poster = null;
			frames = [];
			time = 0;
			playing = true;
		});
		posterOf(wanted).then(
			(url) => live && (poster = url),
			() => {}
		);
		framesOf(wanted, length).then(
			(list) => {
				if (!live) return;
				frames = list;
				if (list.length > 1 && untrack(() => playing)) run();
			},
			() => {}
		);
		return () => {
			live = false;
			stop();
		};
	});
	onDestroy(stop);

	const picture = $derived(frames.length > 0 ? frames[frameAt(time, duration, frames.length)] : poster);

	function toggle(e: MouseEvent) {
		if (e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();
		playing = !playing;
		if (playing) run();
		else stop();
	}
</script>

{#snippet row(label: string, value: string)}
	<div style="display:flex;align-items:baseline;justify-content:space-between;gap:12px;padding:3px 0">
		<span style="flex:none;font-size:12px;color:var(--text-muted);white-space:nowrap">{label}</span>
		<span
			data-selectable
			style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
			>{value}</span
		>
	</div>
{/snippet}

<div data-testid="template-inspector">
	<div style="display:flex;gap:9px;align-items:center">
		<div
			style="width:40px;height:28px;border-radius:var(--radius-sm);flex:none;background:var(--surface-void);display:grid;place-items:center;color:var(--text-muted)"
		>
			<Icon n="layout-template" s={14} />
		</div>
		<div style="flex:1;min-width:0">
			<div
				data-selectable
				style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
			>
				{info.name}
			</div>
			<div style="margin-top:3px"><Badge tone="neutral">Template · library</Badge></div>
		</div>
	</div>

	<div
		data-testid="template-preview"
		style="margin-top:10px;display:grid;place-items:center;background:var(--surface-void);border-radius:var(--radius-sm);overflow:hidden"
	>
		<div
			style="width:min(100%,calc(320px * {info.width / info.height}));aspect-ratio:{info.width} / {info.height};display:grid;place-items:center;color:var(--text-muted)"
		>
			{#if picture}
				<img src={picture} alt="" draggable="false" style="width:100%;height:100%;object-fit:contain;display:block" />
			{:else}
				<Icon n="layout-template" s={22} />
			{/if}
		</div>
	</div>
	{#if frames.length > 1}
		<div style="margin-top:6px;display:flex;align-items:center;gap:6px">
			<IconBtn title={playing ? 'Pause' : 'Play the animation'} size={26} onclick={toggle}>
				<Icon n={playing ? 'pause' : 'play'} s={14} />
			</IconBtn>
			<input
				type="range"
				aria-label="Position in the animation"
				min="0"
				max={duration}
				step={duration / frames.length}
				value={time}
				oninput={(e) => {
					playing = false;
					stop();
					time = Math.min(Number(e.currentTarget.value), duration - 0.001);
				}}
				style="flex:1;min-width:0"
			/>
			<span
				style="flex:none;font-family:var(--font-mono);font-size:11px;font-variant-numeric:tabular-nums;color:var(--text-muted);white-space:nowrap"
				>{fmtDuration(time)} / {fmtDuration(duration)}</span
			>
		</div>
	{/if}

	<div style="margin-top:10px;display:flex;flex-wrap:wrap;gap:6px">
		<Btn
			size="sm"
			style={ROW_ITEM}
			icon="plus"
			iconSize={12}
			pending={editor.locked}
			title="Put it on the titles lane at the playhead"
			onclick={() => void addTemplateHere(info)}>Add at playhead</Btn
		>
		<Btn
			size="sm"
			style={ROW_ITEM}
			icon="between-horizontal-start"
			iconSize={12}
			pending={editor.locked}
			title="Put it on a video track at the playhead, where it takes transitions and keyframes like a clip"
			onclick={() => void addTemplateClipHere(info)}>Add to a track</Btn
		>
	</div>

	{#if info.description}
		<div data-selectable style="margin-top:10px;font-size:12px;line-height:1.5;color:var(--text-secondary);overflow-wrap:anywhere">
			{info.description}
		</div>
	{/if}

	<InspectorSection title="Template" id="template-card-template" summary={fmtDuration(duration)} open>
		{@render row('Duration', fmtDuration(duration))}
		{@render row('Frame', `${info.width}×${info.height}`)}
		{#if info.fonts.length > 0}{@render row('Fonts', fontsLine(info.fonts))}{/if}
		{@render row('Size', fmtSize(info.size_bytes))}
	</InspectorSection>

	<InspectorSection title="Controls" id="template-card-controls" summary={controlCount(info.controls.length)} open>
		{#if info.controls.length === 0}
			<div style="font-size:12px;line-height:1.5;color:var(--text-muted)">Nothing in it changes: it is added as it is.</div>
		{:else}
			{#each info.controls as c (c.id)}{@render row(c.label, kindLabel(c.kind))}{/each}
			<div style="margin-top:4px;font-size:12px;line-height:1.5;color:var(--text-muted)">Set once it is on the timeline.</div>
		{/if}
	</InspectorSection>

	<div style="margin-top:18px;display:flex;flex-wrap:wrap;gap:6px">
		<Btn size="sm" style={ROW_ITEM} icon="upload" iconSize={12} onclick={() => void exportTemplateFile(info)}>Export…</Btn>
		<Btn
			size="sm"
			style={ROW_ITEM}
			variant="destructive"
			data-quiet
			icon="trash"
			iconSize={12}
			onclick={() => void deleteTemplateFile(info).then((gone) => gone && ui.selectedTemplate?.id === info.id && ui.selectTemplate(null))}
			>Delete…</Btn
		>
	</div>
</div>
