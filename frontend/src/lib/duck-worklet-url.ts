// The URL of the bundled duck worklet. Its own module, imported only on demand, because the
// `?worker&url` suffix is Vite's and bun (which loads `audio.ts` in tests) cannot resolve it.
export { default } from './duck-worklet.ts?worker&url';
