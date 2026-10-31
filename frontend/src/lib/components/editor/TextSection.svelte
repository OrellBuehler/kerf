<script lang="ts">
	// The Inspector's Text section for an animated SVG title, floating or on a track (not a preset, which is drawn from its
	// parameters). Each `<text>` with an id can be given a box: a width its lines wrap to, or that it shrinks to fit on one
	// line (`set_title_text_box`, one edit when a field commits, never per keystroke), and an animation: a preset picked
	// from the engine's list, its delay and its duration, and the curve its movement, scale and rotation travel on (a
	// spring, an overshoot, an ease: `set_title_text_animation` again, one edit each; the curves are the engine's too). A
	// box is drawn and an animation played only when the text is glyph outlines, which a template title's always is; a custom title's text can be
	// set as outlines or drawn with its fonts again (`convert_title_text`). The artwork stays the engine's: nothing here draws it.
	import InspectorSection from './InspectorSection.svelte';
	import Btn from './Btn.svelte';
	import { onMount } from 'svelte';
	import { editor } from '#lib/state.svelte';
	import { FieldEdits } from '#lib/field-edit';
	import { listTextAnimationCurves, listTextAnimationPresets } from '#lib/api';
	import { tip } from '#lib/tip';
	import { CURVE_PRESETS, TEXT_ANIMATION_PRESETS, checkAnimation, curveIdOf, hasMotion, presetOf, withMotionCurve } from '#lib/text-animation';
	import { holdForPreview, report } from '#lib/ops';
	import { say } from '#lib/template-actions';
	import { appliedLine } from '#lib/template-ui';
	import { boxWidthRefusal, isOutlined, titleTextAt, titleTexts, type TitleText } from '#lib/title-text';
	import type {
		AnimationCurvePreset,
		TemplateInstance,
		TextAnimation,
		TextAnimationPreset,
		TextBox,
		TextFit,
		TitleTextForm
	} from '#lib/types';

	/** A title, floating or on a track: its id, name, artwork and template. */
	let { o }: { o: { id: string; name: string; svg: string; template?: TemplateInstance | null } } = $props();

	const texts = $derived(titleTexts(o.svg));
	// The engine's list; the page's own copy (replayed against it by a test) stands in until it answers.
	let presets = $state<TextAnimationPreset[]>([...TEXT_ANIMATION_PRESETS]);
	let curves = $state<AnimationCurvePreset[]>([...CURVE_PRESETS]);
	onMount(() => {
		void listTextAnimationPresets()
			.then((list) => {
				if (list.length) presets = list;
			})
			.catch(() => {});
		void listTextAnimationCurves()
			.then((list) => {
				if (list.length) curves = list;
			})
			.catch(() => {});
	});
	const outlined = $derived(isOutlined(o.svg));
	let converting = $state(false);

	/** What a text is called here: the label of a template control that sets it, else its id. */
	const labelOf = (t: TitleText, template: TemplateInstance | null | undefined = o.template): string =>
		template?.controls.find((c) => c.kind === 'text' && c.targets.some((x) => x.element === t.id))?.label ?? `#${t.id}`;

	/** What a field is editing: the title and the text it took focus on, and what was typed into it. A box or a
	 *  timing field commits with its blur, and the click that blurs it is often the one that selects another title:
	 *  by then `o` is that title and the field has been re-rendered with its value (`FieldEdits`). */
	interface Edit {
		id: string;
		element: string;
	}
	const edits = new FieldEdits<Edit, HTMLInputElement>();
	const noteFocus = (e: { currentTarget: HTMLInputElement }, t: TitleText) => edits.focus(e.currentTarget, { id: o.id, element: t.id });
	const noteTyped = (e: { currentTarget: HTMLInputElement }) => edits.input(e.currentTarget, e.currentTarget.value);

	/** The text `at` names as the cut has it now (the title may have been edited, or gone, since the field took focus). */
	const textAt = (at: Edit) => titleTextAt(editor.timeline, at.id, at.element);

	/** The box field shows `t`'s box: the text as it is drawn now, i.e. the selection's. */
	const boxShown = (el: HTMLInputElement | HTMLSelectElement, t: TitleText) => {
		if (el.isConnected) el.value = el instanceof HTMLSelectElement ? t.box.fit : t.box.width === null ? '' : String(t.box.width);
	};

	/** A box field committed, to the text `at` it began on: one edit when the box changed, else the field shows the box again.
	 *  `shown` is the text the field is drawn for now, which is what it goes back to. */
	async function commit(el: HTMLInputElement | HTMLSelectElement, at: Edit, shown: TitleText, change: Partial<TextBox>) {
		const back = () => boxShown(el, shown);
		const was = textAt(at);
		if (!was) return back();
		const { title, text } = was;
		const next: TextBox = { ...text.box, ...change };
		if (next.width === text.box.width && (next.fit === text.box.fit || next.width === null)) return back();
		if (editor.locked || editor.busy || holdForPreview()) return back();
		try {
			const edit = await editor.setTitleTextBox(at.id, at.element, next);
			const line = appliedLine(title.name, edit.warnings);
			if (line) say(line);
		} catch (e) {
			report(e);
			back();
		}
	}

	/** What the animation select shows for a text: a preset's id, `custom` for one of its own, `` for none. */
	const choiceOf = (t: TitleText): string => (t.animation ? (presetOf(t.animation) ?? 'custom') : '');

	/** An animation (or none) stored as one edit on the text `at` began on; `back` puts the field back to the cut when it is refused. */
	async function setAnimation(at: Edit, next: TextAnimation | null, back: () => void) {
		if (editor.locked || editor.busy || holdForPreview()) return back();
		const name = editor.titleById(at.id)?.name ?? 'This title';
		try {
			const edit = await editor.setTitleTextAnimation(at.id, at.element, next);
			const line = appliedLine(name, edit.warnings);
			if (line) say(line);
		} catch (e) {
			report(e);
			back();
		}
	}

	// A select changes the moment it is picked, so the title it shows is the one it edits.
	function chooseAnimation(el: HTMLSelectElement, t: TitleText) {
		const back = () => {
			if (el.isConnected) el.value = choiceOf(t);
		};
		const picked = el.value;
		if (picked === choiceOf(t) || picked === 'custom') return back();
		const next = picked === '' ? null : (presets.find((p) => p.id === picked)?.animation ?? null);
		if (picked !== '' && !next) return back();
		void setAnimation({ id: o.id, element: t.id }, next, back);
	}

	/** What the curve select shows for a text: a curve preset's id, `custom` for one of its own or a mix. */
	const curveChoice = (t: TitleText): string => (t.animation ? (curveIdOf(t.animation) ?? 'custom') : 'linear');

	/** A curve picked: one edit that puts it on the properties the animation moves (or takes it off). */
	function chooseCurve(el: HTMLSelectElement, t: TitleText) {
		const current = t.animation;
		const back = () => {
			if (el.isConnected) el.value = curveChoice(t);
		};
		const found = curves.find((c) => c.id === el.value);
		if (!current || !found || found.id === curveChoice(t)) return back();
		let next: TextAnimation;
		try {
			next = withMotionCurve(current, found.curve);
		} catch (e) {
			report(e);
			return back();
		}
		void setAnimation({ id: o.id, element: t.id }, next, back);
	}

	/** The delay or the duration committed: one edit when it changed and is one an animation may have. It goes to the
	 *  text it took focus on, with what was typed, and is checked against that text's animation as the cut has it now. */
	function commitTiming(el: HTMLInputElement, shown: TitleText, key: 'delay' | 'duration') {
		const { scope: at, text: typed } = edits.end(el, { scope: { id: o.id, element: shown.id }, text: el.value });
		const back = () => {
			if (el.isConnected) el.value = shown.animation ? String(shown.animation[key]) : '';
		};
		// The field is drawn for another title by now: what was typed is not its value to show.
		if (at.id !== o.id) back();
		const was = textAt(at);
		const current = was?.text.animation;
		if (!was || !current) return back();
		const n = Number(typed);
		const why = typed.trim() === '' ? 'enter a number of seconds' : checkAnimation({ ...current, [key]: n });
		if (why) {
			report(new Error(`${labelOf(was.text, was.title.template ?? null)}: ${why}`));
			return back();
		}
		if (n === current[key]) return back();
		void setAnimation(at, { ...current, [key]: n }, back);
	}

	function commitWidth(el: HTMLInputElement, shown: TitleText) {
		const { scope: at, text: typed } = edits.end(el, { scope: { id: o.id, element: shown.id }, text: el.value });
		// The field is drawn for another title by now: what was typed is not its value to show.
		if (at.id !== o.id) boxShown(el, shown);
		const why = boxWidthRefusal(typed);
		if (why) {
			const was = textAt(at);
			report(new Error(`${was ? labelOf(was.text, was.title.template ?? null) : labelOf(shown)}: ${why}`));
			boxShown(el, shown);
			return;
		}
		void commit(el, at, shown, { width: typed.trim() === '' ? null : Number(typed) });
	}

	async function convert(form: TitleTextForm) {
		if (editor.locked || converting || holdForPreview()) return;
		converting = true;
		const { id, name } = o;
		try {
			const edit = await editor.convertTitleText(id, form);
			const line = appliedLine(name, edit.warnings);
			if (line) say(line);
		} catch (e) {
			report(e);
		} finally {
			converting = false;
		}
	}

	const field =
		'box-sizing:border-box;height:var(--control-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-size:var(--text-sm);padding:0 8px;min-width:0';
	const caption = 'font-size:12px;color:var(--text-muted);line-height:1.4';
	const FITS: { value: TextFit; label: string }[] = [
		{ value: 'wrap', label: 'Wrap lines' },
		{ value: 'shrink', label: 'Shrink to fit' }
	];
</script>

<InspectorSection title="Text" summary={outlined ? 'Outlines' : 'Fonts'}>
	{#each texts as t (t.id)}
		<div data-title-text={t.id} style="display:flex;flex-direction:column;gap:3px;padding:4px 0">
			<span style="{caption};overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{labelOf(t)}</span>
			<div style="display:flex;align-items:center;gap:6px;min-width:0">
				<input
					type="number"
					min="1"
					step="any"
					value={t.box.width ?? ''}
					placeholder="No box"
					readonly={editor.locked}
					aria-disabled={editor.locked || undefined}
					aria-label="{labelOf(t)}: box width"
					onfocus={(e) => noteFocus(e, t)}
					oninput={noteTyped}
					oncompositionend={noteTyped}
					onchange={(e) => commitWidth(e.currentTarget, t)}
					style="{field};width:96px;font-family:var(--font-mono);text-align:right"
				/>
				<select
					class="k-select"
					value={t.box.fit}
					aria-label="{labelOf(t)}: fit"
					disabled={t.box.width === null}
					aria-disabled={editor.locked || undefined}
					onchange={(e) => void commit(e.currentTarget, { id: o.id, element: t.id }, t, { fit: e.currentTarget.value as TextFit })}
				>
					{#each FITS as f (f.value)}
						<option value={f.value}>{f.label}</option>
					{/each}
				</select>
			</div>
			{#if t.box.width !== null && !t.outlined}
				<div style={caption}>Drawn with its font, so it does not keep to its box.</div>
			{/if}
			<div style="display:flex;align-items:center;gap:6px;min-width:0;margin-top:2px">
				<select
					class="k-select"
					value={choiceOf(t)}
					aria-label="{labelOf(t)}: animation"
					aria-disabled={editor.locked || undefined}
					use:tip={presets.find((p) => p.id === choiceOf(t))?.description}
					onchange={(e) => chooseAnimation(e.currentTarget, t)}
				>
					<option value="">No animation</option>
					{#each presets as p (p.id)}
						<option value={p.id}>{p.name}</option>
					{/each}
					{#if choiceOf(t) === 'custom'}
						<option value="custom" disabled>Custom</option>
					{/if}
				</select>
				{#if t.animation}
					<input
						type="number"
						min="0"
						max="30"
						step="0.1"
						value={t.animation.delay}
						readonly={editor.locked}
						aria-disabled={editor.locked || undefined}
						aria-label="{labelOf(t)}: animation delay in seconds"
						use:tip={'Delay (s)'}
						onfocus={(e) => noteFocus(e, t)}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(e) => commitTiming(e.currentTarget, t, 'delay')}
						style="{field};width:64px;font-family:var(--font-mono);text-align:right"
					/>
					<input
						type="number"
						min="0.05"
						max="30"
						step="0.1"
						value={t.animation.duration}
						readonly={editor.locked}
						aria-disabled={editor.locked || undefined}
						aria-label="{labelOf(t)}: animation duration in seconds"
						use:tip={'Duration (s)'}
						onfocus={(e) => noteFocus(e, t)}
						oninput={noteTyped}
						oncompositionend={noteTyped}
						onchange={(e) => commitTiming(e.currentTarget, t, 'duration')}
						style="{field};width:64px;font-family:var(--font-mono);text-align:right"
					/>
				{/if}
			</div>
			{#if t.animation && hasMotion(t.animation)}
				<select
					class="k-select"
					value={curveChoice(t)}
					aria-label="{labelOf(t)}: motion curve"
					aria-disabled={editor.locked || undefined}
					use:tip={curves.find((c) => c.id === curveChoice(t))?.description ?? 'Curve of the movement, scale and rotation'}
					onchange={(e) => chooseCurve(e.currentTarget, t)}
				>
					{#each curves as c (c.id)}
						<option value={c.id}>{c.name}</option>
					{/each}
					{#if curveChoice(t) === 'custom'}
						<option value="custom" disabled>Custom curve</option>
					{/if}
				</select>
			{/if}
			{#if t.animation && !t.outlined}
				<div style={caption}>Drawn with its font, so it does not play.</div>
			{/if}
		</div>
	{:else}
		<div style={caption}>Give a text an <code>id</code> in the SVG source to give it a box or an animation.</div>
	{/each}
	{#if texts.length}
		<div style="{caption};margin-top:4px">
			The width is in the artwork's units, from where the text is anchored. Shrink to fit keeps it on one line, made smaller
			until it fits. An animation moves each character, word or line in turn; its delay and duration are in seconds from
			the start of the title. Its curve shapes how what moves, turns or scales gets into place: an overshoot or a spring
			goes past it and comes back.
		</div>
	{/if}
	<div style="{caption};margin-top:8px">
		{#if o.template}
			A template title's text is drawn as outlines: the same on every machine, whatever fonts it has.
		{:else if outlined}
			The text is drawn as outlines: the same on every machine, whatever fonts it has.
		{:else}
			The text is drawn with its fonts. As outlines it looks the same on every machine, and a box wraps or shrinks it.
		{/if}
	</div>
	{#if !o.template}
		<div style="margin-top:6px">
			{#if outlined}
				<Btn size="sm" icon="type" pending={editor.locked || converting} onclick={() => void convert('text')}>Draw with fonts</Btn>
			{:else}
				<Btn size="sm" icon="pen-tool" pending={editor.locked || converting} onclick={() => void convert('outlines')}
					>Draw as outlines</Btn
				>
			{/if}
		</div>
	{/if}
</InspectorSection>
