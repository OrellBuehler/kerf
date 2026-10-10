import { contrastRatio } from '/home/orell/github/kerf/frontend/src/lib/contrast.ts';
for (const c of ['#718090', '#7a8696', '#8493a3', '#8a97a6']) console.log(c, 'on active', contrastRatio(c, '#2c3540').toFixed(2), 'on hover', contrastRatio(c, '#222932').toFixed(2));
