import { contrastRatio, mixSrgb } from '/home/orell/github/kerf/frontend/src/lib/contrast.ts';
import { PRESETS } from '/home/orell/github/kerf/frontend/src/lib/theme.ts';
const rows: [string, (c: any) => [string, string]][] = [
  ['text-disabled on surface-app (status bar 10px, gear/bell/version)', (c) => [c['text-disabled'], c['surface-app']]],
  ['text-disabled on surface-raised (menu disabled item)', (c) => [c['text-disabled'], c['surface-raised']]],
  ['text-disabled on surface-panel (panel hints)', (c) => [c['text-disabled'], c['surface-panel']]],
  ['text-muted on surface-raised (menu accel 10.5px)', (c) => [c['text-muted'], c['surface-raised']]],
  ['text-muted on surface-app (inactive dock tab text / ws tab)', (c) => [c['text-muted'], c['surface-app']]],
  ['text-muted on surface-inset (ws tab idle)', (c) => [c['text-muted'], c['surface-inset']]],
  ['text-secondary on surface-app (menu tops)', (c) => [c['text-secondary'], c['surface-app']]],
  ['kerf-400 on surface-raised (menu check)', (c) => [c['kerf-400'], c['surface-raised']]],
  ['kerf-400 on surface-app (focus ring vs bg)', (c) => [c['kerf-400'], c['surface-app']]],
  ['kerf-400 on surface-panel (focus ring in panel)', (c) => [c['kerf-400'], c['surface-panel']]],
  ['text-on-accent on kerf-500', (c) => [c['text-on-accent'], c['kerf-500']]],
  ['text-on-accent on kerf-500 (bell badge 9px)', (c) => [c['text-on-accent'], c['kerf-500']]],
  ['orange-400 on warning-surface/app (Unsaved badge 10px)', (c) => [c['orange-400'], mixSrgb(c['orange-500'], c['surface-app'], 0.12)]],
  ['green-400 on success-surface/app (Saved badge 10px)', (c) => [c['green-400'], mixSrgb(c['green-500'], c['surface-app'], 0.12)]],
  ['kerf-300 on selection-fill/app (kerf badge)', (c) => [c['kerf-300'], mixSrgb(c['kerf-500'], c['surface-app'], 0.16)]],
  ['agent-300 on agent-surface/app', (c) => [c['agent-300'], mixSrgb(c['agent-500'], c['surface-app'], 0.10)]],
  ['red-400 on danger-surface/app', (c) => [c['red-400'], mixSrgb(c['red-500'], c['surface-app'], 0.12)]],
  ['text-secondary on surface-hover (disabled-hint)', (c) => [c['text-secondary'], c['surface-hover']]],
  ['text-muted on surface-hover (unread notice)', (c) => [c['text-muted'], c['surface-hover']]],
  ['text-disabled on surface-hover', (c) => [c['text-disabled'], c['surface-hover']]],
  ['text-disabled on surface-active (selected ws tab idle?)', (c) => [c['text-disabled'], c['surface-active']]],
  ['text-primary on surface-active', (c) => [c['text-primary'], c['surface-active']]],
  ['kerf-400 icon on surface-active (rail active)', (c) => [c['kerf-400'], c['surface-active']]],
  ['text-secondary on input', (c) => [c['text-secondary'], c['input']]],
  ['agent-500 on surface-app (link/icon)', (c) => [c['agent-500'], c['surface-app']]],
  ['border-strong (16% border-base on surface-panel) vs panel', (c) => [mixSrgb(c['border-base'], c['surface-panel'], 0.16), c['surface-panel']]],
  ['border-default (10%) vs panel', (c) => [mixSrgb(c['border-base'], c['surface-panel'], 0.10), c['surface-panel']]],
  ['border-subtle (6%) vs panel', (c) => [mixSrgb(c['border-base'], c['surface-panel'], 0.06), c['surface-panel']]],
  ['border-default on surface-app (statusbar top)', (c) => [mixSrgb(c['border-base'], c['surface-app'], 0.10), c['surface-app']]],
  ['surface-panel vs surface-app (panel edge)', (c) => [c['surface-panel'], c['surface-app']]],
  ['surface-raised vs surface-panel (menu vs panel)', (c) => [c['surface-raised'], c['surface-panel']]],
  ['surface-active vs surface-inset (ws tab pill)', (c) => [c['surface-active'], c['surface-inset']]],
];
const out: string[] = [];
out.push('pair | dark | light | hc');
for (const [name, f] of rows) {
  const vals = ['kerf-dark', 'kerf-light', 'high-contrast'].map((id) => { const [a, b] = f((PRESETS as any)[id].colors); return contrastRatio(a, b).toFixed(2); });
  out.push(`${name} | ${vals.join(' | ')}`);
}
console.log(out.join('\n'));
// disabled context menu: text-disabled at .55 opacity over raised
for (const id of ['kerf-dark', 'kerf-light', 'high-contrast']) {
  const c = (PRESETS as any)[id].colors;
  const eff = mixSrgb(c['text-disabled'], c['surface-raised'], 0.55);
  console.log('ctx-menu disabled eff', id, eff, contrastRatio(eff, c['surface-raised']).toFixed(2));
  const eff2 = mixSrgb(c['text-muted'], c['surface-raised'], 0.55);
  // button:disabled opacity .5 of a text-secondary button
  const e3 = mixSrgb(c['text-secondary'], c['surface-panel'], 0.5);
  console.log('  button:disabled text-secondary@.5 on panel', e3, contrastRatio(e3, c['surface-panel']).toFixed(2));
  const e4 = mixSrgb(c['text-primary'], c['surface-hover'], 0.5);
  console.log('  secondary Btn disabled primary@.5 on hover', contrastRatio(e4, c['surface-hover']).toFixed(2));
  const e5 = mixSrgb(c['text-on-accent'], c['kerf-500'], 0.5);
  console.log('  primary Btn disabled: label vs fill (both @.5 of panel)', 'n/a');
}
