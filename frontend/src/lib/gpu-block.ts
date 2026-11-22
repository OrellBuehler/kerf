// What the page says when the GPU preview turned itself off on this machine (`GpuBlock`, `gpu_preview/block.rs`):
// once, quietly, and in words — the technical reason stays in Settings, where the preview's status is.

import type { GpuBlock } from './types';

export interface BlockNotice {
	message: string;
	description: string;
}

/** The notice for a block the page has not told yet, or null: no block, or one it already said. */
export function blockNotice(block: GpuBlock | null | undefined): BlockNotice | null {
	if (!block || block.told) return null;
	const back = 'Frames are drawn the usual way until then. Settings > Preview turns it on again.';
	if (block.kind === 'mismatch') {
		return {
			message: 'GPU preview is off on this computer',
			description: `This graphics card and driver draw frames differently from this copy of FFmpeg, so it was turned off for them. A driver or Kerf update is checked again. ${back}`
		};
	}
	return {
		message: 'GPU preview was turned off',
		description: `Kerf stopped while the GPU preview was running, which can be a graphics driver problem, so it is off on this computer. ${back}`
	};
}

/** The block as the page writes it back once it has said so (`told`); everything else is the backend's. */
export function told(block: GpuBlock): GpuBlock {
	return { ...block, told: true };
}
