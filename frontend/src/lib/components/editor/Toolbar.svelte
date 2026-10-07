<script lang="ts">
	import Icon from './Icon.svelte';
	import { formatTimecode } from '$lib/timecode';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import { ui, type Tool } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { PANEL_IDS, PANELS } from '$lib/layout';
	import { workspaceSpec } from '$lib/workspaces';
	import { DELIVERY_PRESETS, fitLabel, presetFor } from '$lib/delivery-formats';
	import { toast } from '$lib/notifications.svelte';
	import { settings } from '$lib/settings.svelte';
	import type { ActionId } from '$lib/keymap';
	import { TOOL_HINT, type TrimTool } from '$lib/trim-tools';

	let {
		onNew,
		onExport,
		onOpen,
		onSave
	}: { onNew: () => void; onExport: () => void; onOpen: () => void; onSave: () => void } =
		$props();

	// [tool, icon, name, the action whose key selects it] — the hint is read from
	// the keymap, so it shows the key the user actually has.
	const tools: [Tool, string, string, ActionId][] = [
		['pointer', 'MousePointer2', 'Select', 'tool.pointer'],
		['razor', 'Scissors', 'Razor', 'tool.razor']
	];
	// The tools that move a boundary rather than a clip, set apart from those two.
	// Their tooltips say what the drag does, since the glyphs alone do not.
	const trimTools: [TrimTool, string, string, ActionId][] = [
		['roll', 'separator-vertical', 'Roll', 'tool.roll'],
		['slip', 'gallery-horizontal', 'Slip', 'tool.slip'],
		['slide', 'arrow-left-right', 'Slide', 'tool.slide']
	];

	// The frame the cut is being made for. Changing it reshapes the preview, the
	// scrubbed still and the export together, so the vertical crop is something
	// you compose against rather than discover in the rendered file.
	const delivery = $derived(presetFor(editor.timeline.format));

	function pickDelivery(e: MouseEvent) {
		contextMenu.show(
			e,
			DELIVERY_PRESETS.map((p) => ({
				label: p.label === 'Source' ? 'Source shape' : `${p.label} — ${p.hint}`,
				icon: p.id === delivery.id ? 'check' : undefined,
				action: async () => {
					try {
						await editor.setDeliveryFormat(p.format);
						toast.success(p.format ? `Cutting for ${p.label} (${fitLabel(p.format.fit)})` : 'Following the footage');
					} catch (err) {
						toast.error(err instanceof Error ? err.message : String(err));
					}
				}
			}))
		);
	}

	function pickPanels(e: MouseEvent) {
		contextMenu.show(e, [
			...PANEL_IDS.map((id) => ({
				label: PANELS[id].title,
				icon: workspace.isOpen(id) ? 'check' : undefined,
				action: () => workspace.toggle(id)
			})),
			{ type: 'separator' as const },
			{
				label: `Reset ${workspaceSpec(workspace.active).label} workspace`,
				icon: 'rotate-ccw',
				action: () => workspace.reset()
			}
		]);
	}

	function tc(s: number): string {
		return formatTimecode(s, editor.fps);
	}
</script>

{#snippet divider()}
	<span style="width:1px;height:22px;background:var(--border-strong);margin:0 4px;flex:none"></span>
{/snippet}

<div
	style="height:var(--toolbar-h);display:flex;align-items:center;gap:6px;padding:0 12px;background:var(--surface-panel);border-bottom:var(--line-width) solid var(--border-default);flex:none"
>
	{#each tools as [id, ic, name, action] (id)}
		<IconBtn title={settings.withShortcut(name, action)} active={ui.tool === id} onclick={() => (ui.tool = id)}>
			<Icon n={ic} />
		</IconBtn>
	{/each}
	{@render divider()}
	{#each trimTools as [id, ic, name, action] (id)}
		<IconBtn
			title="{settings.withShortcut(`${name} tool`, action)} — {TOOL_HINT[id]}"
			aria-label="{name} tool"
			active={ui.tool === id}
			onclick={() => (ui.tool = id)}
		>
			<Icon n={ic} />
		</IconBtn>
	{/each}
	{@render divider()}
	<IconBtn title="Snap to clips" active={ui.snap} onclick={() => (ui.snap = !ui.snap)}>
		<Icon n="magnet" />
	</IconBtn>

	{@render divider()}

	<IconBtn
		title={settings.withShortcut('Undo', 'edit.undo')}
		disabled={!editor.canUndo}
		onclick={() => editor.undo()}
		style={editor.canUndo ? '' : 'opacity:.4;cursor:default'}
	>
		<Icon n="undo" />
	</IconBtn>
	<IconBtn
		title={settings.withShortcut('Redo', 'edit.redo')}
		disabled={!editor.canRedo}
		onclick={() => editor.redo()}
		style={editor.canRedo ? '' : 'opacity:.4;cursor:default'}
	>
		<Icon n="redo" />
	</IconBtn>

	{@render divider()}

	<IconBtn title={settings.withShortcut('Skip to start', 'playback.toStart')} onclick={() => ui.seek(0)}
		><Icon n="skip-back" /></IconBtn
	>
	<IconBtn
		title={settings.withShortcut(ui.playing ? 'Pause' : 'Play', 'playback.toggle')}
		onclick={() => ui.togglePlay()}
		style="background:var(--surface-hover);color:var(--text-primary)"
	>
		<Icon n={ui.playing ? 'pause' : 'play'} />
	</IconBtn>
	<IconBtn title={settings.withShortcut('Skip to end', 'playback.toEnd')} onclick={() => ui.seek(editor.duration)}
		><Icon n="skip-forward" /></IconBtn
	>
	<span title={`Timeline timecode · ${editor.fps.toFixed(3)} fps · non-drop`} style="font-family:var(--font-mono);font-size:13px;color:var(--kerf-300);margin-left:6px;font-weight:500">
		{tc(ui.time)}
	</span>
	<span style="font-size:11px;color:var(--text-muted)" title="Timeline frame rate; non-drop timecode">{Number(editor.fps.toFixed(3))} fps</span>

	{@render divider()}

	<Btn
		variant="ghost"
		size="sm"
		icon="crop"
		onclick={pickDelivery}
		title={delivery.format
			? `Delivering ${delivery.format.width}\u00d7${delivery.format.height} — ${fitLabel(delivery.format.fit)}. Click to change.`
			: 'The frame follows the footage. Click to cut for a delivery shape.'}
		style={delivery.format ? 'color:var(--kerf-300)' : ''}>{delivery.label}</Btn
	>

	<div style="flex:1"></div>

	<Btn variant="ghost" size="sm" icon="file-plus" onclick={onNew} title={settings.withShortcut('New empty project', 'file.new')}
		>New</Btn
	>
	<Btn variant="ghost" size="sm" icon="folder-open" onclick={onOpen} title={settings.withShortcut('Open project…', 'file.open')}
		>Open</Btn
	>
	<Btn
		variant={editor.saved ? 'ghost' : 'secondary'}
		size="sm"
		icon="save"
		onclick={onSave}
		title={settings.withShortcut('Save project as…', 'file.save')}>Save</Btn
	>
	{@render divider()}
	<Btn
		variant="ghost"
		size="sm"
		icon="layout-panel-left"
		title="Show or hide panels, or reset this workspace — drag a panel's tab to move it"
		onclick={pickPanels}>Panels</Btn
	>
	{@render divider()}
	<Btn variant="primary" size="sm" icon="upload" onclick={onExport} title={settings.withShortcut('Export', 'file.export')}
		>Export</Btn
	>
</div>
