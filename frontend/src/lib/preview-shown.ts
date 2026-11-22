// What the Preview does, or does not, when it is not on show. It is a panel like the others: closed it is unmounted, and
// behind another tab it is still mounted with its content out of the document (`Preview.svelte`'s `shown`, dockview's
// `isVisible`). What it asks of the backend for a picture nobody sees — a frame per playhead move, a stream, a GPU
// session — is work for nothing, and what it says about the renderer of the frame on screen is false. The clock and the
// sound are the editor state's and are not decided here. Pure, so the decisions are tested without a panel.

/** The width the frame pump is told it may ask for: none while the panel is not on show (the pump then asks nothing, and
 *  asks again the moment it has a width). */
export function pumpWidth(shown: boolean, width: number | null): number | null {
	return shown ? width : null;
}

/** Whether the panel plays the picture of the cut: a forward 1x play, of a cut with clips, in a panel on show. Anything
 *  else ends the stream or GPU session it had; the sound goes on whatever this says. */
export function picturePlays(o: { playing: boolean; rate: number; hasClips: boolean; shown: boolean }): boolean {
	return o.playing && o.rate === 1 && o.hasClips && o.shown;
}

/** What the status bar is to say about the renderer of the frame on screen. `clear`: nothing (the setting is off, or no
 *  frame is on screen because the panel is not); `jpeg`: the frame is FFmpeg's by the page's own choice, for `why`;
 *  `leave`: what the last answer said stands. */
export type RendererNote = { kind: 'clear' } | { kind: 'jpeg'; why: string } | { kind: 'leave' };

export function rendererNote(o: { enabled: boolean; shown: boolean; via: 'gpu' | 'jpeg'; why: string | null }): RendererNote {
	if (!o.enabled || !o.shown) return { kind: 'clear' };
	if (o.via === 'jpeg' && o.why) return { kind: 'jpeg', why: o.why };
	return { kind: 'leave' };
}
