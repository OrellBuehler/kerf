import { describe, expect, test } from 'bun:test';
import { describeReport, reportWarns } from './export-report';

describe('export reports', () => {
	test('warnings come first and repeated losses carry their count', () => {
		const report = {
			entries: [
				{ level: 'info' as const, message: 'audio tracks are not written to the EDL', count: 1 },
				{ level: 'warning' as const, message: 'clip fades are not carried by an EDL', count: 3 }
			]
		};
		expect(describeReport(report)).toBe('clip fades are not carried by an EDL (×3)\naudio tracks are not written to the EDL');
		expect(reportWarns(report)).toBe(true);
		expect(reportWarns({ entries: [report.entries[0]] })).toBe(false);
		expect(describeReport({ entries: [] })).toBe('');
	});
});
