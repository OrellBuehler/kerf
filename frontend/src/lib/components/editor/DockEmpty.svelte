<script lang="ts">
	// The dock's watermark: every panel was closed, so say how to get one back. It was "Open a panel from the
	// Window menu" in a bare grey line.
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import { workspace } from '$lib/workspace.svelte';
</script>

<div style="height:100%;display:grid;place-items:center">
	<EmptyState icon="layout-panel-left" title="No panels open" hint="Open one from the Window menu, or put this workspace back as it came.">
		<Btn size="sm" icon="film" onclick={() => workspace.show('library')}>Open Library</Btn>
		<Btn size="sm" icon="refresh-cw" onclick={() => workspace.reset()}>Reset workspace</Btn>
	</EmptyState>
</div>
