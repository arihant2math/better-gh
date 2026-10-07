import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { probeLatestRelease } from './AboutSidebar';
import { EmptyRepoState } from './EmptyRepoState';

describe('probeLatestRelease', () => {
  it('skips the probe for an empty repository', () => {
    expect(probeLatestRelease(true, true, null)).toBe(false);
    expect(probeLatestRelease(true, true, '2026-01-01T00:00:00Z')).toBe(false);
  });

  it('waits for the tree when the repository was never pushed', () => {
    expect(probeLatestRelease(false, undefined, null)).toBe(false);
    expect(probeLatestRelease(true, false, null)).toBe(true);
  });

  it('fetches immediately for a pushed repository', () => {
    expect(probeLatestRelease(false, undefined, '2026-01-01T00:00:00Z')).toBe(true);
  });

  it('fetches when the tree failed (older server without the empty signal)', () => {
    expect(probeLatestRelease(true, undefined, null)).toBe(true);
  });
});

describe('EmptyRepoState', () => {
  it('is a neutral state linking back to Quick setup', () => {
    const html = renderToStaticMarkup(<EmptyRepoState repo={{ owner: 'acme', name: 'web' }} />);
    expect(html).toContain('This repository is empty');
    expect(html).toMatch(/<button[^>]*>.*Quick setup.*<\/button>/);
    expect(html).not.toMatch(/doesn.t exist/);
  });
});
