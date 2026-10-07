import { describe, expect, it } from 'vitest';
import { returnTo, sameOriginPath } from './index';

const ORIGIN = 'https://bgh.test';
const ret = (target: string) => returnTo(`?return_to=${encodeURIComponent(target)}`, ORIGIN);

describe('return_to (open redirect)', () => {
  it.each(['/\\x', '/\t/x', '/\n/x', '\\\\x', '//x', 'https://x', 'https://x/acme', 'javascript:alert(1)', 'data:text/html,x', '/..//evil.com', '/.//evil.com', '/a/..//evil.com', '/%2e%2e//evil.com', `${ORIGIN}//evil.com`])('rejects %j', (target) => {
    expect(sameOriginPath(target, ORIGIN)).toBeNull();
    expect(ret(target)).toBe('/');
  });

  it('keeps same-origin paths', () => {
    expect(ret('/acme/api?x=1#y')).toBe('/acme/api?x=1#y');
    expect(ret(`${ORIGIN}/acme/api?x=1#y`)).toBe('/acme/api?x=1#y');
    expect(ret('/settings')).toBe('/settings');
  });

  it('defaults to home', () => {
    expect(returnTo('', ORIGIN)).toBe('/');
    expect(returnTo('?return_to=', ORIGIN)).toBe('/');
  });
});
