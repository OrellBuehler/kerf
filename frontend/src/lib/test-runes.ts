// Stands in for the few runes `state.svelte.ts` uses, so a bun test can construct the
// `editor` singleton and drive its async actions (bun cannot compile `.svelte.ts`).
// Plain values: nothing is reactive, which is all such a test needs. Import it
// for its side effect, before importing the module under test.

type Rune = ((v: unknown) => unknown) & { by?: (f: () => unknown) => unknown; snapshot?: (v: unknown) => unknown };
const g = globalThis as unknown as Record<string, Rune>;
g.$state = Object.assign((v: unknown) => v, { snapshot: (v: unknown) => structuredClone(v) });
g.$derived = Object.assign((v: unknown) => v, { by: (f: () => unknown) => f() });
