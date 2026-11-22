import { describe, expect, test } from 'bun:test';
import { FORM_GAP, FORM_LABEL_W, FORM_TRACK_AT } from './form-rows';

describe('the form columns', () => {
	test('a slider’s track starts where the control column does', () => {
		expect(FORM_TRACK_AT).toBe(FORM_LABEL_W + FORM_GAP);
	});
});
