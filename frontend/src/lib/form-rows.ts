// The form the export dialog and the Deliver panel share (`FormRow`): a column for the name, a gap, a column for
// the control. A `SliderRow` in the same form starts its track where the control column does, so it is handed
// the two widths added together less what the slider keeps for itself (`SLIDER_INSET`).

/** px: the column a row's name is in. */
export const FORM_LABEL_W = 120;
/** px: the gap between the name and the control. */
export const FORM_GAP = 12;
/** px: where the control column begins, counted from the row's left edge. */
export const FORM_TRACK_AT = FORM_LABEL_W + FORM_GAP;

/** px a `SliderRow` keeps between its label column and the visible start of its track: its column gap (8) and the room for the
 *  thumb at the end of the travel (the thumb's radius, 9). */
export const SLIDER_INSET = 8 + 9;
/** px of label column that makes the visible track start where the control column (the selects) does. */
export const FORM_SLIDER_LABEL_W = FORM_TRACK_AT - SLIDER_INSET;
