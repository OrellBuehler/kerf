<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { clipDuration } from '$lib/types';
	import { toast } from 'svelte-sonner';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const kind = $derived(asset?.streams.some((s) => s.kind === 'video') ? 'video' : 'audio');
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	const track = $derived(
		clip ? editor.timeline.tracks.find((t) => t.clips.some((c) => c.id === clip.id)) : undefined
	);

	function tc(s: number): string {
		const t = Math.max(0, s);
		const m = Math.floor(t / 60);
		const sec = Math.floor(t % 60);
		const cs = Math.floor((t % 1) * 100);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}.${cs
			.toString()
			.padStart(2, '0')}`;
	}

	async function run(op: () => Promise<unknown>) {
		try {
			await op();
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	const inputCss =
		'width:90px;background:var(--surface-inset);border:1px solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:5px 7px;text-align:right';
</script>

{#snippet secHead(label: string)}
	<div style="display:flex;align-items:center;gap:8px;margin:14px 0 9px">
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>{label}</span
		>
		<div style="flex:1;height:1px;background:var(--border-subtle)"></div>
	</div>
{/snippet}

{#snippet readRow(label: string, value: string)}
	<div style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)">{value}</span>
	</div>
{/snippet}

{#snippet numRow(label: string, value: number, step: number, onCommit: (v: number) => void)}
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:3px 0">
		<span style="font-size:12px;color:var(--text-muted)">{label}</span>
		<input
			type="number"
			{value}
			{step}
			min="0"
			disabled={editor.busy}
			onchange={(e) => {
				const v = parseFloat(e.currentTarget.value);
				if (Number.isFinite(v)) onCommit(v);
			}}
			style={inputCss}
		/>
	</label>
{/snippet}

<div
	style="width:var(--inspector-w);flex:none;background:var(--surface-panel);border-left:1px solid var(--border-default);display:flex;flex-direction:column;overflow:hidden"
>
	<div
		style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-default)"
	>
		<Icon n="sliders-horizontal" s={14} color="var(--text-secondary)" />
		<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary)">Inspector</span>
	</div>

	<div style="flex:1;overflow-y:auto;padding:12px">
		{#if clip}
			<div style="display:flex;gap:9px;align-items:center">
				<div
					style="width:40px;height:28px;border-radius:3px;flex:none;background:{kind === 'audio'
						? 'var(--track-audio)'
						: 'linear-gradient(135deg,#22303f,#33424f)'};display:grid;place-items:center;color:rgba(255,255,255,.8)"
				>
					<Icon n={kind === 'audio' ? 'audio-waveform' : 'video'} s={14} />
				</div>
				<div style="flex:1;min-width:0">
					<div
						style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
						title={asset?.name ?? 'clip'}
					>
						{asset?.name ?? 'clip'}
					</div>
					<div style="margin-top:3px">
						<Badge tone="neutral">{track?.name ?? (kind === 'audio' ? 'audio' : 'video')}</Badge>
					</div>
				</div>
			</div>

			{@render secHead('Position')}
			{@render readRow('Start', tc(clip.timeline_start))}
			{@render readRow('Duration', tc(clipDuration(clip)))}

			{@render secHead('Trim (source)')}
			{@render readRow('Source range', `${tc(clip.source_in)} – ${tc(clip.source_out)}`)}
			{@render numRow('In', clip.source_in, 0.1, (v) =>
				run(() => editor.trim(clip.id, v, undefined))
			)}
			{@render numRow('Out', clip.source_out, 0.1, (v) =>
				run(() => editor.trim(clip.id, undefined, v))
			)}

			{#if hasAudio}
				{@render secHead('Volume')}
				<label style="display:flex;align-items:center;gap:10px;padding:3px 0">
					<input
						type="range"
						min="0"
						max="2"
						step="0.05"
						value={clip.volume}
						disabled={editor.busy}
						onchange={(e) => {
							const v = parseFloat(e.currentTarget.value);
							if (Number.isFinite(v)) void run(() => editor.setVolume(clip.id, v));
						}}
						style="flex:1;accent-color:var(--kerf-500)"
					/>
					<span
						style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);width:46px;text-align:right"
						>{Math.round(clip.volume * 100)}%</span
					>
				</label>
			{/if}

			{@render secHead('Fades')}
			{@render numRow('Fade in', clip.fade_in, 0.1, (v) =>
				run(() => editor.setFade(clip.id, Math.max(0, v), undefined))
			)}
			{@render numRow('Fade out', clip.fade_out, 0.1, (v) =>
				run(() => editor.setFade(clip.id, undefined, Math.max(0, v)))
			)}

			<div style="margin-top:18px">
				<Btn
					variant="destructive"
					size="sm"
					icon="trash"
					iconSize={13}
					style="width:100%"
					disabled={editor.busy}
					onclick={() => run(() => editor.remove(clip.id))}>Remove clip</Btn
				>
			</div>
		{:else}
			<div
				style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:40px 16px;color:var(--text-disabled);text-align:center"
			>
				<Icon n="sliders-horizontal" s={22} />
				<span style="font-size:12px">Select a clip to inspect it</span>
			</div>
		{/if}
	</div>
</div>
