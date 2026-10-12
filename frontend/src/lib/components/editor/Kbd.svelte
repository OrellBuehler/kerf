<script lang="ts">
	// A printed shortcut drawn the one way (`.k-kbd`, `accel.ts`). `inherit` takes the surrounding ink,
	// for a row that is off.
	import { accelParts } from '#lib/accel';

	let { keys, inherit = false }: { keys: string; inherit?: boolean } = $props();
	const parts = $derived(accelParts(keys));
</script>

<kbd class="k-kbd" style={inherit ? 'color:inherit' : undefined}
	>{parts.head}<span class={parts.cap ? 'k-kcap' : parts.arrow ? 'k-arrow' : undefined}>{parts.key}</span></kbd
>
