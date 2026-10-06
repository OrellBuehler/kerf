import { expect, test } from 'bun:test';
import viteConfig from '../../vite.config';

// The release workflow puts the updater signing key (TAURI_SIGNING_PRIVATE_KEY…)
// in the environment of the step that runs `vite build`, and Vite inlines every
// variable matching an `envPrefix` into the bundle on an `import.meta.env`
// reference. No prefix may therefore reach a TAURI_ variable.

test('no Vite envPrefix exposes the TAURI_ signing secrets', () => {
	const config = viteConfig as { envPrefix?: string | string[] };
	const prefixes = ([] as string[]).concat(config.envPrefix ?? 'VITE_');
	for (const secret of ['TAURI_SIGNING_PRIVATE_KEY', 'TAURI_SIGNING_PRIVATE_KEY_PASSWORD']) {
		const exposedBy = prefixes.filter((p) => secret.startsWith(p));
		expect(exposedBy, `${secret} would be inlined via ${JSON.stringify(exposedBy)}`).toEqual([]);
	}
});
