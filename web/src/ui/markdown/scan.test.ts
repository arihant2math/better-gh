import { describe, expect, it } from 'vitest';
import { anchorizer, scan, shortUrl } from './scan';

const ctx = { base: '', repo: ['o', 'r'] as const, emoji: { tada: '🎉' } };

describe('reference scan (server parity)', () => {
  it('links refs and keeps plain words', () => {
    const segs = scan('see #1, GH-2, a/b#3 and cafe123 (no) but abc1234 yes :tada:', ctx);
    expect(segs.filter((s) => s.kind !== 'text')).toEqual([
      { kind: 'link', url: '/o/r/issues/1', text: '#1', cls: 'issue-link' },
      { kind: 'link', url: '/o/r/issues/2', text: 'GH-2', cls: 'issue-link' },
      { kind: 'link', url: '/a/b/issues/3', text: 'a/b#3', cls: 'issue-link' },
      { kind: 'link', url: '/o/r/commit/cafe123', text: 'cafe123', cls: 'commit-link' },
      { kind: 'link', url: '/o/r/commit/abc1234', text: 'abc1234', cls: 'commit-link' },
      { kind: 'emoji', name: 'tada', emoji: '🎉' },
    ]);
  });

  it('applies the longest autolink prefix first, case-insensitively', () => {
    const autolinks = [
      { key_prefix: 'TICKET-X-', url_template: 'https://t/x/<num>', is_alphanumeric: false },
      { key_prefix: 'TICKET-', url_template: 'https://t/<num>', is_alphanumeric: false },
    ];
    const segs = scan('ticket-12 TICKET-X-7 TICKET-ab', { ...ctx, autolinks });
    expect(segs.filter((s) => s.kind === 'link').map((s) => (s.kind === 'link' ? s.url : ''))).toEqual(['https://t/12', 'https://t/x/7']);
  });

  it('does not link references when disabled, but still emoji', () => {
    expect(scan('#1 :tada:', { ...ctx, references: false }).map((s) => s.kind)).toEqual(['text', 'emoji']);
  });

  it('shortens instance URLs', () => {
    expect(shortUrl('http://h', ['o', 'r'], 'http://h/o/r/pull/4')).toEqual({ text: '#4', cls: 'issue-link' });
    expect(shortUrl('http://h', ['o', 'r'], 'http://h/x/y/commit/abcdef0123')).toEqual({ text: 'x/y@abcdef0', cls: 'commit-link' });
    expect(shortUrl('http://h', ['o', 'r'], 'http://h/o/r/issues/4#foo')).toBeNull();
    expect(shortUrl('http://h', ['o', 'r'], 'http://other/o/r/issues/4')).toBeNull();
  });

  it('slugs headings like comrak', () => {
    const slug = anchorizer();
    expect(slug('Hello World')).toBe('hello-world');
    expect(slug('Hello World')).toBe('hello-world-1');
    expect(slug('Café & Crème: v1.2!')).toBe('café--crème-v12');
  });
});
