import { expect, test } from 'bun:test';
import { cspViolationLine, describeError, watchCspViolations } from './log';

test('describeError reads strings, errors and objects', () => {
	expect(describeError('plain')).toBe('plain');
	expect(describeError(new Error('boom'))).toBe('boom');
	expect(describeError({ a: 1 })).toBe('{"a":1}');
	expect(describeError(undefined)).toBe('undefined');
});

test('a CSP violation is one line: what was blocked, under which directive, from where', () => {
	expect(cspViolationLine({ effectiveDirective: 'script-src-elem', blockedURI: 'blob:http://tauri.localhost/1', disposition: 'enforce', sourceFile: 'http://tauri.localhost/_app/a.js', lineNumber: 3 })).toBe(
		'CSP blocked blob:http://tauri.localhost/1 (script-src-elem) at http://tauri.localhost/_app/a.js:3'
	);
	expect(cspViolationLine({ effectiveDirective: 'script-src-elem', blockedURI: 'inline', disposition: 'report' })).toBe('CSP reported inline (script-src-elem)');
	expect(cspViolationLine({ effectiveDirective: 'img-src', blockedURI: '', disposition: 'enforce' })).toBe('CSP blocked an inline resource (img-src)');
	// The payload of a data: URL is not logged.
	expect(cspViolationLine({ effectiveDirective: 'font-src', blockedURI: 'data:font/woff2;base64,AAAA', disposition: 'enforce' })).toBe(
		'CSP blocked data:font/woff2;base64,… (font-src)'
	);
	expect(cspViolationLine({ effectiveDirective: 'connect-src', blockedURI: `https://x.test/${'a'.repeat(300)}`, disposition: 'enforce' }).length).toBeLessThan(260);
});

test('the watch is on the window it is given, and comes off', () => {
	const added: string[] = [];
	const removed: string[] = [];
	const win = {
		addEventListener: (type: string) => added.push(type),
		removeEventListener: (type: string) => removed.push(type)
	} as unknown as Window;
	const stop = watchCspViolations(win);
	expect(added).toEqual(['securitypolicyviolation']);
	stop();
	expect(removed).toEqual(['securitypolicyviolation']);
});
