// What a keyable number is called, in one place. The Inspector names a row by what it controls inside its section
// ("Position X" in Transform, "Warmth" in Color, "Level" in Volume, "Width" in Mask, "Zoom" in Crop motion); the
// Keyframes panel lists those numbers side by side, away from their sections, so it says the section where the row
// name alone would not (`rowName`). `property-labels.test.ts` reads the Inspector's source and fails when a row there
// is called something else, so the two vocabularies cannot drift apart.

import { PROPERTIES } from './channels';
import type { Property } from './types';

export type PropertyGroup = 'Transform' | 'Color' | 'Volume' | 'Mask' | 'Crop';

export interface PropertyName {
	/** The Inspector section the number's row is in (`Crop` is the Crop motion rows of Framing). */
	group: PropertyGroup;
	/** The row's own label there. */
	label: string;
}

export const PROPERTY_NAMES: Record<Property, PropertyName> = {
	scale: { group: 'Transform', label: 'Scale' },
	pos_x: { group: 'Transform', label: 'Position X' },
	pos_y: { group: 'Transform', label: 'Position Y' },
	rotation: { group: 'Transform', label: 'Rotation' },
	opacity: { group: 'Transform', label: 'Opacity' },
	brightness: { group: 'Color', label: 'Brightness' },
	contrast: { group: 'Color', label: 'Contrast' },
	saturation: { group: 'Color', label: 'Saturation' },
	gamma: { group: 'Color', label: 'Gamma' },
	temperature: { group: 'Color', label: 'Warmth' },
	volume: { group: 'Volume', label: 'Level' },
	mask_x: { group: 'Mask', label: 'Center X' },
	mask_y: { group: 'Mask', label: 'Center Y' },
	mask_width: { group: 'Mask', label: 'Width' },
	mask_height: { group: 'Mask', label: 'Height' },
	mask_feather: { group: 'Mask', label: 'Feather' },
	crop_zoom: { group: 'Crop', label: 'Zoom' },
	crop_x: { group: 'Crop', label: 'Pan X' },
	crop_y: { group: 'Crop', label: 'Pan Y' }
};

/** The Inspector's label for the number's row. */
export const inspectorLabel = (p: Property): string => PROPERTY_NAMES[p].label;

/** A row of the Keyframes panel: the Inspector's label, with its section where the label alone would not say what it
 *  is (`Mask · Width`, not `Width`); volume is just `Volume`. */
export function rowName(p: Property): string {
	const { group, label } = PROPERTY_NAMES[p];
	if (group === 'Transform' || group === 'Color') return label;
	if (group === 'Volume') return 'Volume';
	return `${group} · ${label}`;
}

/** What decides which numbers a clip has rows for in the Inspector (`clipBlocks`): something is drawn from it (its footage
 *  has a picture and it sits on a picture track), its footage has sound, and that sound was not detached to a clip of its own. */
export interface ClipKind {
	picture: boolean;
	hasAudio: boolean;
	soundDetached: boolean;
}

/**
 * The keyable numbers a clip of this kind has: a picture the transform, colour, mask and crop numbers; a clip with sound of
 * its own the volume. A sound clip (the audio half of a linked pair, music, a voiceover) has the volume and nothing else, so
 * keys of the picture's numbers cannot be put on it (they would be channels the clip has no use for).
 */
export function clipProperties(k: ClipKind): Property[] {
	const sound = k.hasAudio && !k.soundDetached;
	return PROPERTIES.filter((p) => (p === 'volume' ? sound : k.picture));
}
