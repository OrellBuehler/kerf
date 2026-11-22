<script lang="ts">
	// A theme in miniature, drawn from the theme's own colours (not the page's): a window, a panel with a line of text and a
	// dot of the accent, and the two kinds of clip on a lane. Enough to tell the themes in a list apart at a glance.
	import type { Theme } from '#lib/theme';

	let { theme }: { theme: Pick<Theme, 'colors'> } = $props();
	const c = $derived(theme.colors);
</script>

<span
	aria-hidden="true"
	style="position:relative;display:block;flex:none;width:56px;height:36px;overflow:hidden;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-strong);background:{c['surface-app']}"
>
	<span style="position:absolute;left:4px;top:4px;width:22px;height:15px;border-radius:2px;background:{c['surface-panel']}"></span>
	<span style="position:absolute;left:7px;top:8px;width:14px;height:2px;background:{c['text-primary']}"></span>
	<span style="position:absolute;left:7px;top:12px;width:9px;height:2px;background:{c['text-muted']}"></span>
	<span style="position:absolute;left:30px;top:4px;width:20px;height:15px;border-radius:2px;background:{c['surface-raised']}"></span>
	<span style="position:absolute;left:41px;top:8px;width:6px;height:6px;border-radius:50%;background:{c['kerf-500']}"></span>
	<span style="position:absolute;left:4px;top:23px;width:30px;height:4px;background:{c['track-video']}"></span>
	<span style="position:absolute;left:4px;top:28px;width:44px;height:4px;background:{c['track-audio']}"></span>
	<span style="position:absolute;left:20px;top:21px;width:2px;height:12px;background:{c['kerf-500']}"></span>
</span>
