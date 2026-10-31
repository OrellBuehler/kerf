<script lang="ts">
	// A clip's live behaviours: a sway, a breath, a spin, a drift or a shake that rides on top of the clip's pose and
	// its keyframes without ever being keyed, so it costs the export nothing however long the clip is. The list is the
	// clip's whole list (at most 8, in order, several of a kind add up); each row is one behaviour with its numbers.
	// Every slider writes one edit on release (`SliderRow`; Escape, pointercancel and blur abandon a gesture), a typed
	// entry one on Enter, Add, Remove and Bake one each. The preview shows the clip through FFmpeg while it has any
	// (the GPU preview refuses them), so what is drawn is what the export renders. *Bake* turns them into ordinary
	// keyframes; a clip too long for that is refused, and the section says what to do about it. The numbers are
	// `behavior-ui.ts`'s (pure, tested); the arithmetic is `behaviors.ts`'s.
	import InspectorSection from './InspectorSection.svelte';
	import SliderRow from './SliderRow.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '#lib/state.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import { toast } from '#lib/notifications.svelte';
	import { tip } from '#lib/tip';
	import { errorMessage, report } from '#lib/ops';
	import { BEHAVIOR_HINTS, behaviorsSummary, defaultBehavior, randomSeed } from '#lib/behaviors';
	import { propsPhrase } from '#lib/keyframe-copy';
	import {
		ADD_CHOICES,
		bakeTip,
		behaviorFields,
		fieldValue,
		parseSeed,
		rowName,
		withField,
		withSeed,
		type FieldKey
	} from '#lib/behavior-ui';
	import { MAX_BEHAVIORS, type Behavior, type BehaviorKind, type Clip } from '#lib/types';

	let { clip }: { clip: Clip } = $props();

	const list = $derived(clip.behaviors ?? []);
	const atLimit = $derived(list.length >= MAX_BEHAVIORS);
	/** Why the last bake was refused, said under the buttons until something changes. */
	let bakeError = $state<string | null>(null);
	$effect(() => {
		void clip.id;
		void list.length;
		bakeError = null;
	});

	/** What a row is editing: the clip and the place in its list, as they are when the edit *begins*. */
	interface Row {
		id: string;
		index: number;
	}

	/** The behaviours of the clip `id` as the timeline has them now (the selection may have moved since a gesture began). */
	function behaviorsOf(id: string): Behavior[] {
		for (const t of editor.timeline.tracks) {
			const c = t.clips.find((x) => x.id === id);
			if (c) return c.behaviors ?? [];
		}
		return [];
	}

	async function write(id: string, next: Behavior[]) {
		try {
			await editor.setBehaviors([id], next);
		} catch (e) {
			report(e);
		}
	}

	/** One number of one behaviour: the list is read as it is now, and only the row's own number changes. */
	function setField(at: Row, key: FieldKey, value: number) {
		const now = behaviorsOf(at.id);
		if (!now[at.index]) return;
		return write(at.id, now.map((b, i) => (i === at.index ? withField(b, key, value) : b)));
	}

	function setSeed(at: Row, seed: number) {
		const now = behaviorsOf(at.id);
		if (!now[at.index]) return;
		return write(at.id, now.map((b, i) => (i === at.index ? withSeed(b, seed) : b)));
	}

	function remove(at: Row) {
		const now = behaviorsOf(at.id);
		if (!now[at.index]) return;
		return write(at.id, now.filter((_, i) => i !== at.index));
	}

	function add(id: string, kind: BehaviorKind) {
		return write(id, [...behaviorsOf(id), defaultBehavior(kind, randomSeed())]);
	}

	/** The menu of kinds, opened under the Add button. */
	function openAdd(e: MouseEvent) {
		const button = e.currentTarget as HTMLElement;
		const r = button.getBoundingClientRect();
		const id = clip.id;
		contextMenu.show(
			new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }),
			ADD_CHOICES.map((c) => ({ label: c.label, meta: c.meta, action: () => void add(id, c.kind) })),
			button
		);
	}

	async function bake() {
		const id = clip.id;
		bakeError = null;
		try {
			const [done] = await editor.bakeBehaviors([id]);
			const what = done.properties.length ? `${done.keys} keyframes on ${propsPhrase(done.properties)}` : 'nothing to key';
			toast.info(`Baked the behaviours into ${what}`);
		} catch (e) {
			bakeError = errorMessage(e).replace(/^invalid argument: /, '');
		}
	}

	/** The seed field takes whole numbers; anything else goes back to the seed it held. */
	function commitSeed(e: Event & { currentTarget: HTMLInputElement }, at: Row, seed: number) {
		const v = parseSeed(e.currentTarget.value);
		e.currentTarget.value = String(v ?? seed);
		if (v !== undefined && v !== seed) void setSeed(at, v);
	}
</script>

<InspectorSection title="Behaviours" summary={behaviorsSummary(list)}>
	<div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin:2px 0 6px">
		<Btn
			size="sm"
			variant="secondary"
			icon="plus"
			iconSize={12}
			pending={editor.locked}
			disabled={atLimit}
			title={atLimit ? `A clip carries at most ${MAX_BEHAVIORS} behaviours` : 'Add a sway, a breath, a spin, a drift or a shake on top of the clip’s pose'}
			onclick={openAdd}>Add</Btn
		>
		<Btn
			size="sm"
			variant="secondary"
			pending={editor.locked}
			disabled={list.length === 0}
			title={bakeTip(list.length)}
			onclick={() => void bake()}>Bake</Btn
		>
	</div>
	{#if bakeError}
		<div role="alert" data-bake-error style="font:var(--type-caption);color:var(--red-400);margin:0 0 6px">{bakeError}</div>
	{/if}
	{#if list.length === 0}
		<div style="font:var(--type-caption);color:var(--text-muted);margin:0 0 4px">
			Motion that goes on by itself and never needs keyframes: it rides on top of the clip’s position, scale and
			rotation and its own keys.
		</div>
	{/if}
	{#each list as b, i (i)}
		{@const at = { id: clip.id, index: i }}
		{@const name = rowName(list, i)}
		<div role="group" aria-label="{name} behaviour" data-behavior={b.kind} style="margin:6px 0 8px">
			<div style="display:flex;align-items:center;gap:8px;margin:0 0 2px">
				<span class="k-overline" style="flex:1" use:tip={BEHAVIOR_HINTS[b.kind]}>{name}</span>
				<IconBtn size={24} title="Remove {name.toLowerCase()}" pending={editor.locked} onclick={() => void remove(at)}>
					<Icon n="x" s={12} />
				</IconBtn>
			</div>
			{#each behaviorFields(b.kind) as f (f.key)}
				<SliderRow
					label={f.label}
					value={fieldValue(b, f.key)}
					spec={f.spec}
					scope={at}
					reserve={false}
					disabled={editor.locked}
					onCommit={(v, row) => setField(row, f.key, v)}
				/>
			{/each}
			{#if b.kind === 'shake'}
				{@const seed = b.seed ?? 0}
				<div style="display:grid;grid-template-columns:72px minmax(0,1fr) auto;align-items:center;column-gap:8px;padding:2px 0">
					<label for="seed-{i}" style="font-size:var(--text-sm);color:var(--text-secondary)">Seed</label>
					<input
						id="seed-{i}"
						class="seed"
						type="text"
						inputmode="numeric"
						autocomplete="off"
						spellcheck="false"
						aria-label="{name} seed"
						value={seed}
						readonly={editor.locked}
						aria-disabled={editor.locked || undefined}
						onchange={(e) => commitSeed(e, at, seed)}
						onkeydown={(e) => {
							if (e.key === 'Escape') {
								e.currentTarget.value = String(seed);
								e.currentTarget.blur();
								e.stopPropagation();
							}
						}}
					/>
					<IconBtn size={24} title="Another shake" pending={editor.locked} onclick={() => void setSeed(at, randomSeed())}>
						<Icon n="refresh-cw" s={12} />
					</IconBtn>
				</div>
			{/if}
		</div>
	{/each}
</InspectorSection>

<style>
	.seed {
		box-sizing: border-box;
		width: 100%;
		min-width: 0;
		height: 24px;
		padding: 0 6px;
		text-align: right;
		font-family: var(--font-mono);
		font-size: var(--text-sm);
		color: var(--text-secondary);
		background: transparent;
		border: var(--line-width) solid transparent;
		border-radius: var(--radius-sm);
	}
	.seed:hover:not([aria-disabled='true']) {
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
	.seed:focus {
		background: var(--surface-inset);
		border-color: var(--border-strong);
		color: var(--text-primary);
	}
</style>
