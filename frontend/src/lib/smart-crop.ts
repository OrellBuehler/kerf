/**
 * Smart crop, the shape half.
 *
 * `kerf_core::model` decides the real thing in the app: it samples where a
 * shot's content sits and picks the crop window that keeps it. Only the
 * *shape* arithmetic — does this footage have to be cropped at all, and how
 * much of which axis survives — is mirrored here, for the browser harness,
 * which has no decoder to sample with and so always lands on the centre
 * window. Same split as `platforms.ts`: one decision, made in Rust, with just
 * enough of it in TS to keep `bun run dev` drivable.
 */

/** Aspects within this relative tolerance are the same shape. Mirrors `model.rs`. */
export const ASPECT_TOLERANCE = 0.01;

export interface CropFrame {
	left: number;
	right: number;
	top: number;
	bottom: number;
	/** How far the window sits from a plain centre crop (0 = dead centre). */
	offset: number;
}

/** Whether footage of this shape loses part of itself filling `targetAspect`. */
export function needsCrop(sourceWidth: number, sourceHeight: number, targetAspect: number): boolean {
	const source = sourceWidth / Math.max(sourceHeight, 1);
	if (!isFinite(source) || source <= 0 || !isFinite(targetAspect) || targetAspect <= 0) return false;
	return Math.abs((source - targetAspect) / targetAspect) > ASPECT_TOLERANCE;
}

/**
 * The centred crop for `targetAspect` — what a `Cover` fit does on its own,
 * and what the harness proposes in place of a sampled window. `null` when the
 * footage is already that shape.
 */
export function centeredCrop(sourceWidth: number, sourceHeight: number, targetAspect: number): CropFrame | null {
	if (!needsCrop(sourceWidth, sourceHeight, targetAspect)) return null;
	const source = sourceWidth / Math.max(sourceHeight, 1);
	const horizontal = source > targetAspect;
	const keep = Math.min(1, Math.max(0.01, horizontal ? targetAspect / source : source / targetAspect));
	const edge = (1 - keep) / 2;
	return horizontal
		? { left: edge, right: edge, top: 0, bottom: 0, offset: 0 }
		: { left: 0, right: 0, top: edge, bottom: edge, offset: 0 };
}
