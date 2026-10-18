<script lang="ts">
	// A two-state setting: a native checkbox read as a switch. The 16px box sits in a label with room around it, so the
	// press does not have to land on the box, and `SettingRow`'s name (`for`) toggles it too. With `text`, the words
	// beside the box are part of the label.
	let {
		checked,
		label,
		id = undefined,
		text = undefined,
		disabled = false,
		testid = undefined,
		onchange
	}: {
		checked: boolean;
		/** The accessible name. */
		label: string;
		/** The input's id, for a `SettingRow`'s `for`. */
		id?: string;
		text?: string;
		disabled?: boolean;
		testid?: string;
		/** The new state, and the box itself (to put back when the change is refused). */
		onchange: (checked: boolean, box: HTMLInputElement) => void;
	} = $props();
</script>

<label
	data-switch
	style="display:inline-flex;align-items:center;justify-content:{text ? 'flex-start' : 'center'};gap:8px;{text
		? 'font-size:12px;color:var(--text-primary);padding:6px 0'
		: 'width:32px;height:28px;margin:-5px -8px'};cursor:{disabled ? 'default' : 'pointer'}"
>
	<input
		{id}
		type="checkbox"
		role="switch"
		aria-label={label}
		data-testid={testid}
		{checked}
		{disabled}
		onchange={(e) => onchange(e.currentTarget.checked, e.currentTarget)}
		style="accent-color:var(--kerf-500);cursor:inherit;width:16px;height:16px;margin:0"
	/>
	{#if text}{text}{/if}
</label>
