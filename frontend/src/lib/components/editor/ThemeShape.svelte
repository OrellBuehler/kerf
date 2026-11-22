<script lang="ts">
	// The theme editor's second page: how the interface is built rather than coloured — corners, lines, sliders and depth,
	// how tall controls and bars are, and the typefaces. Every change goes to the editor's session and is on the page at once.
	import ChoiceChips from './ChoiceChips.svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { settings } from '#lib/settings.svelte';
	import { DENSITIES, SHAPE_GROUPS, SHAPE_TOKENS, THUMB_STYLES, shapeText } from '#lib/theme';
	import { MONO_FONTS, UI_FONTS } from '#lib/theme-fonts';
	import { shapeChanged, shapeDefault } from '#lib/theme-session';
	import { OVERLINE } from '#lib/settings-ui';
	import { tip } from '#lib/tip';

	const themes = settings.themes;
	const draft = $derived(themes.session?.draft);

	const uiStack = $derived(UI_FONTS.find((f) => f.id === draft?.shape['font-ui'])?.stack);
	const monoStack = $derived(MONO_FONTS.find((f) => f.id === draft?.shape['font-mono'])?.stack);
	const roundness = $derived(draft?.shape['radius-scale'] ?? 1);
</script>

{#if draft}
	{#each SHAPE_GROUPS as group (group.id)}
		<div data-shape-group={group.id} style="padding:12px 0 6px">
			<div style={OVERLINE}>{group.label}</div>
			<div style="margin-top:6px;display:flex;flex-direction:column;gap:2px">
				{#each SHAPE_TOKENS.filter((t) => t.group === group.id) as t (t.name)}
					<div style="display:grid;grid-template-columns:132px minmax(0,1fr) 48px 24px;align-items:center;gap:8px;min-height:28px;font-size:12px;color:var(--text-secondary)">
						<label for="shape-{t.name}" use:tip={t.hint} style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{t.label}</label>
						<input
							id="shape-{t.name}"
							type="range"
							min={t.min}
							max={t.max}
							step={t.step}
							value={draft.shape[t.name]}
							aria-label={t.label}
							oninput={(e) => themes.setShape(t.name, Number(e.currentTarget.value))}
							onchange={() => themes.endEdit()}
						/>
						<span style="text-align:right;font-family:var(--font-mono);font-size:11px;color:var(--text-primary)">{shapeText(t, draft.shape[t.name])}</span>
						{#if shapeChanged(draft, t.name)}
							<IconBtn title="Put back {shapeText(t, shapeDefault(draft, t.name))}" size={24} onclick={() => themes.resetShape(t.name)}><Icon n="rotate-ccw" s={12} /></IconBtn>
						{:else}
							<span></span>
						{/if}
					</div>
					{#if t.name === 'radius-scale'}
						<div style="display:flex;gap:8px;padding:2px 0 6px 140px" aria-hidden="true">
							{#each [4, 6, 12] as px (px)}
								<span style="width:{px === 12 ? 64 : 40}px;height:22px;border:var(--line-width) solid var(--border-strong);background:var(--surface-inset);border-radius:{px * roundness}px"></span>
							{/each}
						</div>
					{/if}
				{/each}
				{#if group.id === 'sliders'}
					<div style="display:grid;grid-template-columns:132px minmax(0,1fr) 24px;align-items:center;gap:8px;min-height:30px;font-size:12px;color:var(--text-secondary)">
						<span>Thumb style</span>
						<ChoiceChips
							label="Slider thumb style"
							options={THUMB_STYLES.map((s) => ({ value: s.id, label: s.label }))}
							value={draft.shape['slider-thumb-style']}
							onpick={(v) => {
								themes.setChoice('slider-thumb-style', v);
								themes.endEdit();
							}}
						/>
						{#if shapeChanged(draft, 'slider-thumb-style')}
							<IconBtn title="Put back {shapeDefault(draft, 'slider-thumb-style')}" size={24} onclick={() => themes.resetShape('slider-thumb-style')}><Icon n="rotate-ccw" s={12} /></IconBtn>
						{:else}
							<span></span>
						{/if}
					</div>
				{/if}
			</div>
		</div>
	{/each}

	<div data-shape-group="density" style="padding:12px 0 6px">
		<div style={OVERLINE}>Spacing</div>
		<div style="margin-top:6px;display:grid;grid-template-columns:132px minmax(0,1fr) 24px;align-items:center;gap:8px;min-height:30px;font-size:12px;color:var(--text-secondary)">
			<span>Density</span>
			<ChoiceChips
				label="Density"
				options={DENSITIES.map((d) => ({ value: d.id, label: d.label, hint: d.hint }))}
				value={draft.shape.density}
				onpick={(v) => {
					themes.setChoice('density', v);
					themes.endEdit();
				}}
			/>
			{#if shapeChanged(draft, 'density')}
				<IconBtn title="Put back {shapeDefault(draft, 'density')}" size={24} onclick={() => themes.resetShape('density')}><Icon n="rotate-ccw" s={12} /></IconBtn>
			{:else}
				<span></span>
			{/if}
		</div>
		<p style="margin:4px 0 0;font-size:11px;line-height:1.5;color:var(--text-muted)">
			Buttons, fields, tabs, menu rows and the title and status bars. Text stays the size it is.
		</p>
	</div>

	<div data-shape-group="type" style="padding:12px 0 6px">
		<div style={OVERLINE}>Typefaces</div>
		<div style="margin-top:6px;display:flex;flex-direction:column;gap:6px">
			{#each [{ field: 'font-ui' as const, label: 'Interface', list: UI_FONTS }, { field: 'font-mono' as const, label: 'Numbers and timecode', list: MONO_FONTS }] as row (row.field)}
				<div style="display:grid;grid-template-columns:132px minmax(0,1fr) 24px;align-items:center;gap:8px;min-height:30px;font-size:12px;color:var(--text-secondary)">
					<label for="shape-{row.field}">{row.label}</label>
					<select
						id="shape-{row.field}"
						class="k-select"
						value={draft.shape[row.field]}
						onchange={(e) => {
							themes.setChoice(row.field, e.currentTarget.value as never);
							themes.endEdit();
						}}
					>
						{#each row.list as f (f.id)}
							<option value={f.id}>{f.label}</option>
						{/each}
					</select>
					{#if shapeChanged(draft, row.field)}
						<IconBtn title="Put back {shapeDefault(draft, row.field)}" size={24} onclick={() => themes.resetShape(row.field)}><Icon n="rotate-ccw" s={12} /></IconBtn>
					{:else}
						<span></span>
					{/if}
				</div>
			{/each}
			<div
				aria-hidden="true"
				style="margin-top:4px;padding:10px 12px;border:var(--line-width) solid var(--border-default);border-radius:var(--radius-sm);background:var(--surface-inset);color:var(--text-primary)"
			>
				<div style="font-family:{uiStack};font-size:13px;line-height:1.45">Cut the pause after “and so”, then 12 clips — Aa Bb Gg 0123456789</div>
				<div style="margin-top:4px;font-family:{monoStack};font-size:12px;color:var(--text-secondary)">00:01:23:12   -6.0 dB   1920×1080 · 30 fps</div>
			</div>
			<p style="margin:0;font-size:11px;line-height:1.5;color:var(--text-muted)">
				All of them ship inside Kerf; nothing is downloaded. “System” uses this computer’s own.
			</p>
		</div>
	</div>
{/if}
