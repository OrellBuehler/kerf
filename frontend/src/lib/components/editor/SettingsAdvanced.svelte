<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import Btn from './Btn.svelte';
	import { settings } from '#lib/settings.svelte';
	import { workspace } from '#lib/workspace.svelte';
	import { resetAllWorkspacesAsked } from '#lib/workspace-actions';
	import { confirmAction, inTauri, revealLogs, revealSettingsFile } from '#lib/api';
	import { CONFIRM } from '#lib/confirm-copy';
	import { toast } from '#lib/notifications.svelte';
	import { report } from '#lib/ops';

	let resetting = $state(false);

	async function open(what: () => Promise<void>, label: string) {
		if (!inTauri()) {
			toast.info(`${label} is available in the desktop app.`);
			return;
		}
		try {
			await what();
		} catch (e) {
			report(e);
		}
	}

	async function resetAll() {
		const c = CONFIRM.resetSettings;
		if (!(await confirmAction(c.message, c.title, c))) return;
		resetting = true;
		try {
			if (await settings.resetAll()) {
				// The dock is built once from the settings; build it again from the fresh ones.
				workspace.rebuild();
				toast.success('Settings reset to their defaults', {
					description: inTauri() ? 'The old settings file is kept beside the new one.' : undefined
				});
			}
		} finally {
			resetting = false;
		}
	}
</script>

<SettingRow id="open-logs" label="Open the log folder" hint="Kerf writes a log file every day it runs. Attach the newest one when you report a problem.">
	<Btn variant="secondary" size="sm" icon="folder-open" onclick={() => open(revealLogs, 'The log folder')}>Open logs</Btn>
</SettingRow>

<SettingRow
	id="reveal-settings"
	label="Show the settings file"
	hint="Every preference lives in one JSON file in the app's config folder: the theme, shortcuts, layouts and the switches on these pages. It is safe to copy; edit it only with Kerf closed."
>
	<Btn variant="secondary" size="sm" icon="folder-open" onclick={() => open(revealSettingsFile, 'The settings file')}>Show file</Btn>
</SettingRow>

<SettingRow
	id="reset-layout"
	label="Reset the window layout"
	hint="Every workspace — Edit, Color, Audio, Motion, Deliver — goes back to its default arrangement of panels. Nothing else changes."
>
	<Btn variant="secondary" size="sm" icon="rotate-ccw" onclick={() => void resetAllWorkspacesAsked()}>Reset layout…</Btn>
</SettingRow>

<SettingRow
	id="reset-all"
	label="Reset all settings"
	hint="Every preference on these pages goes back to its default, shortcuts and theme included. Where the agent server listens and its access token stay as they are. The old settings file is kept beside the new one."
>
	<Btn variant="destructive" size="sm" icon="rotate-ccw" pending={resetting} data-testid="reset-all" onclick={resetAll}>Reset all…</Btn>
</SettingRow>
