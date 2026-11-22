// What a keyable number is called, in one place. The Inspector names a row by what it controls inside its section
// ("Position X" in Transform, "Warmth" in Color, "Level" in Volume, "Width" in Mask, "Zoom" in Crop motion); the
// Keyframes panel lists those numbers side by side, away from their sections, so it says the section where the row
// name alone would not (`rowName`). `property-labels.test.ts` reads the Inspector's source and fails when a row there
// is called something else, so the two vocabularies cannot drift apart.

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
