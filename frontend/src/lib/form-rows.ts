// The form the export dialog and the Deliver panel share (`FormRow`): a column for the name, a gap, a column for
// the control. A `SliderRow` in the same form starts its track where the control column does, so it is handed
// the two widths added together.

/** px: the column a row's name is in. */
export const FORM_LABEL_W = 120;
/** px: the gap between the name and the control. */
export const FORM_GAP = 12;
/** px: where the control column begins, counted from the row's left edge. */
export const FORM_TRACK_AT = FORM_LABEL_W + FORM_GAP;
