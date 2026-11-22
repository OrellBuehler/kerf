<script lang="ts">
	// Name a workspace: the one to be kept from the panels on screen, or a saved one to be renamed. What a name may be is
	// `checkName` (something typed, a button's length, not another workspace's); this says what is wrong before it is asked.
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import { workspace } from '#lib/workspace.svelte';
	import { settings } from '#lib/settings.svelte';
	import { MAX_WORKSPACE_NAME, checkName, suggestName, workspaceLabel } from '#lib/workspaces';

	let { onClose }: { onClose: () => void } = $props();

	const asked = workspace.naming;
	const renaming = asked?.mode === 'rename' ? asked.key : null;
	let name = $state(renaming ? workspaceLabel(settings.workspaces, renaming) : suggestName(settings.workspaces));
	/** What the dock refused, when it had a reason the check could not see. */
	let refused = $state<string | null>(null);

	const check = $derived(checkName(settings.workspaces, name, renaming ?? undefined));
	// Nothing is said about an empty field until it has been typed in: the dialog opens with a name offered.
	const problem = $derived(refused ?? (check.ok ? null : check.reason));
	const unchanged = $derived(renaming !== null && check.ok && check.name === workspaceLabel(settings.workspaces, renaming));

	function submit(e?: SubmitEvent) {
		e?.preventDefault();
		if (!check.ok) return;
		if (unchanged) return onClose();
		const why = renaming ? workspace.rename(renaming, name) : workspace.saveAs(name);
		if (why) refused = why;
		else onClose();
	}

	const inputCss =
		'flex:1;min-width:0;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:13px;min-height:30px;padding:4px 7px';
</script>

<Modal
	title={renaming ? 'Rename workspace' : 'Save workspace'}
	icon="layout-panel-left"
	width={400}
	dismissOnScrim={false}
	rescue
	initial="[data-name]"
	{onClose}
>
	<form id="workspace-name-form" onsubmit={submit} style="padding:14px 16px;display:flex;flex-direction:column;gap:10px">
		<label style="display:flex;align-items:center;gap:8px">
			<span style="font-size:12px;color:var(--text-muted);width:44px;flex:none">Name</span>
			<input
				data-name
				type="text"
				maxlength={MAX_WORKSPACE_NAME}
				autocomplete="off"
				spellcheck="false"
				bind:value={name}
				oninput={() => (refused = null)}
				onfocus={(e) => e.currentTarget.select()}
				aria-invalid={problem !== null}
				aria-describedby="workspace-name-note"
				style={inputCss}
			/>
		</label>
		<p
			id="workspace-name-note"
			role={problem ? 'alert' : undefined}
			style="margin:0;font-size:12px;line-height:1.5;min-height:18px;color:{problem ? 'var(--warning)' : 'var(--text-muted)'}"
		>
			{#if problem}
				{problem}
			{:else if renaming}
				The name on its button in the title bar.
			{:else}
				Keeps which panels are open, where they are and how big, as they are on screen now. The workspace you are in keeps its
				own arrangement.
			{/if}
		</p>
	</form>
	{#snippet footer()}
		<div style="padding:12px 16px;display:flex;align-items:center;gap:8px">
			<div style="flex:1"></div>
			<Btn variant="ghost" size="md" onclick={onClose}>Cancel</Btn>
			<Btn variant="primary" size="md" icon="check" data-rescue onclick={() => submit()} disabled={!check.ok}>
				{renaming ? 'Rename' : 'Save workspace'}
			</Btn>
		</div>
	{/snippet}
</Modal>
