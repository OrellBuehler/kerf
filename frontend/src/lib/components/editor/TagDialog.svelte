<script lang="ts">
	// Tag assets: the prompt that follows an import (`mode: 'import'`: Skip, and "Don’t ask again" writes the setting)
	// and the bin's "Tag N assets…" (`'bulk'`). Type `key: value` or a plain word, Enter adds it to the list; the tags
	// the assets already carry are shown with `all` or `some (n)` and can be marked for removal. Apply is one
	// `edit_asset_tags` call for every asset — the engine refuses the lot or applies the lot, and says why here.
	import { untrack } from 'svelte';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import CheckRow from './CheckRow.svelte';
	import TagChip from './TagChip.svelte';
	import TagInput from './TagInput.svelte';
	import { editor } from '#lib/state.svelte';
	import { settings } from '#lib/settings.svelte';
	import { toast } from '#lib/notifications.svelte';
	import { tidyReason } from '#lib/failure-copy';
	import { sameTag } from '#lib/tags';
	import { addNote, dialogTitle, namesLine, parseTagInput, pendingEdit, planAdd, tallyNote, tallyTags, taggedLine } from '#lib/tag-ui';
	import type { Tag } from '#lib/types';

	let { assetIds, mode, onClose }: { assetIds: string[]; mode: 'import' | 'bulk'; onClose: () => void } = $props();

	const wanted = new Set(untrack(() => assetIds));
	/** The assets as they are now: a tag changed elsewhere (an agent's) shows up, a removed asset drops out. */
	const targets = $derived(editor.assets.filter((a) => wanted.has(a.id)));
	const names = $derived(targets.map((a) => a.name));
	const tally = $derived(tallyTags(targets.map((a) => a.tags ?? [])));

	let added = $state<Tag[]>([]);
	let removed = $state<Tag[]>([]);
	let text = $state('');
	let error = $state('');
	let saving = $state(false);
	let never = $state(false);

	// Every asset going away under the dialog (a removal, a different project) ends it.
	$effect(() => {
		if (targets.length === 0) onClose();
	});

	const dirty = $derived(added.length > 0 || removed.length > 0 || text.trim() !== '');
	const isRemoved = (tag: Tag) => removed.some((r) => sameTag(r, tag));

	/** Take a typed tag into the list. Whether it went in (the field keeps its text when it did not). */
	function take(tag: Tag): boolean {
		error = '';
		const plan = planAdd(tag, added, removed, tally);
		if (plan === 'add') added = [...added, tag];
		else if (plan === 'restore') removed = removed.filter((r) => !sameTag(r, tag));
		else {
			error = addNote(plan, tag, targets.length);
			return false;
		}
		return true;
	}

	function toggleRemoval(tag: Tag) {
		removed = isRemoved(tag) ? removed.filter((r) => !sameTag(r, tag)) : [...removed, tag];
	}

	/** Leave the dialog; a ticked "Don’t ask again" is written whichever way it closes. */
	function leave() {
		if (mode === 'import' && never) void settings.setAskTagsOnImport(false);
		onClose();
	}

	async function apply() {
		if (saving) return;
		error = '';
		// What is typed and not yet added counts: pressing Apply is pressing Enter first.
		if (text.trim() !== '') {
			const parsed = parseTagInput(text);
			if ('error' in parsed) {
				error = parsed.error;
				return;
			}
			if (!take(parsed.tag)) return;
			text = '';
		}
		const edit = pendingEdit(added, removed);
		if (!edit) return leave();
		saving = true;
		try {
			await editor.tagAssets(
				targets.map((a) => a.id),
				edit
			);
			toast.success(taggedLine(names));
			leave();
		} catch (e) {
			error = tidyReason(e instanceof Error ? e.message : String(e));
		} finally {
			saving = false;
		}
	}
</script>

<Modal
	title={dialogTitle(mode, names)}
	icon="tags"
	width={460}
	busy={saving}
	dismissOnScrim={false}
	rescue
	initial="[data-tag-field]"
	onClose={leave}
>
	<div data-testid="tag-dialog" style="flex:1;min-height:0;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:14px">
		<p style="margin:0;font-size:12px;line-height:1.5;color:var(--text-secondary)">
			{#if mode === 'import'}
				<span data-testid="tag-dialog-names" style="color:var(--text-muted)">{namesLine(names)}</span><br />
			{/if}
			Tag them so an agent can find them and build a cut by a tag: <span style="white-space:nowrap;font-family:var(--font-mono);color:var(--text-primary)">location: Cumbuco</span>,
			<span style="white-space:nowrap;font-family:var(--font-mono);color:var(--text-primary)">rider: Orell</span>, or a plain
			<span style="white-space:nowrap;font-family:var(--font-mono);color:var(--text-primary)">jump</span>. Enter adds a tag to the list; the button puts the list on {targets.length === 1 ? 'the asset' : `all ${targets.length}`}.
		</p>

		{#if tally.length > 0}
			<section aria-label="Tags already on {targets.length === 1 ? 'the asset' : 'the assets'}" data-testid="tag-existing">
				<div class="k-overline" style="margin-bottom:6px">On {targets.length === 1 ? 'this asset' : `these ${targets.length} assets`}</div>
				<div style="display:flex;flex-wrap:wrap;gap:6px">
					{#each tally as t (JSON.stringify(t.tag))}
						<TagChip
							tag={t.tag}
							note={tallyNote(t)}
							tone={isRemoved(t.tag) ? 'removed' : 'neutral'}
							removeLabel={`Remove ${t.tag.key != null ? `${t.tag.key}: ` : ''}${t.tag.value} from ${t.all ? 'all' : t.count} ${t.count === 1 ? 'asset' : 'assets'}`}
							onremove={() => toggleRemoval(t.tag)}
						/>
					{/each}
				</div>
			</section>
		{/if}

		<section aria-label="Add tags">
			<div class="k-overline" style="margin-bottom:6px">Add</div>
			{#if added.length > 0}
				<div data-testid="tag-pending" style="display:flex;flex-wrap:wrap;gap:6px;margin-bottom:8px">
					{#each added as tag (JSON.stringify(tag))}
						<TagChip
							{tag}
							tone="pending"
							onremove={() => (added = added.filter((t) => !sameTag(t, tag)))}
						/>
					{/each}
				</div>
			{/if}
			<TagInput
				bind:value={text}
				exclude={[...added, ...tally.filter((t) => t.all).map((t) => t.tag)]}
				{error}
				busy={saving}
				rows={5}
				reserve
				oncommit={take}
				data-tag-field
				data-rescue
			/>
		</section>
	</div>
	{#snippet footer()}
		<div style="padding:12px 16px;display:flex;align-items:center;gap:8px">
			{#if mode === 'import'}
				<CheckRow checked={never} onchange={(e) => (never = e.currentTarget.checked)} data-testid="tag-never">Don’t ask again</CheckRow>
			{/if}
			<div style="flex:1"></div>
			<Btn variant="ghost" size="md" pending={saving} onclick={leave}>{mode === 'import' ? 'Skip' : 'Cancel'}</Btn>
			<Btn variant="primary" size="md" icon="tag" pending={saving} disabled={!dirty} data-testid="tag-apply" onclick={() => void apply()}>
				{targets.length > 1 ? `Tag ${targets.length} assets` : 'Apply'}
			</Btn>
		</div>
	{/snippet}
</Modal>
