import { describe, expect, test } from 'bun:test';
import { FORM_GAP, FORM_LABEL_W, FORM_SLIDER_LABEL_W, FORM_TRACK_AT, SLIDER_INSET } from './form-rows';

describe('the form columns', () => {
	test('the control column starts after the label column and the gap', () => {
		expect(FORM_TRACK_AT).toBe(FORM_LABEL_W + FORM_GAP);
	});

	test('a slider’s visible track starts where the control column does: its label column is short of it by what the row keeps', () => {
		// Measured in the dialog: the select at x=809, the CRF track at x=826.
		expect(SLIDER_INSET).toBe(17);
		expect(FORM_SLIDER_LABEL_W + SLIDER_INSET).toBe(FORM_TRACK_AT);
		expect(FORM_SLIDER_LABEL_W).toBeGreaterThan(90);
	});
});
