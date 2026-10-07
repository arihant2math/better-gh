import { describe, expect, it } from 'vitest';
import { MIN_CURRENT_CRUMB, MIN_VISIBLE_CRUMB, crumbsToHide } from './crumbs';

// acme / api / Pull requests / #23 <long title>
const base = { ancestors: [60, 40, 100], separator: 16, ellipsis: 24 };
const full = 60 + 40 + 100 + 3 * 16; // 248

describe('crumbsToHide', () => {
  it('keeps every ancestor when they and the full current crumb fit', () => {
    expect(crumbsToHide({ ...base, available: full + 50, current: 50 })).toBe(0);
  });

  it('lets a long current crumb ellipsize instead of collapsing ancestors', () => {
    expect(crumbsToHide({ ...base, available: full + MIN_CURRENT_CRUMB, current: 900 })).toBe(0);
  });

  it('hides leading ancestors first, replacing them with "…"', () => {
    // without acme: 40 + 100 + 2*16 + "…" 24 + 16 = 212
    expect(crumbsToHide({ ...base, available: full + MIN_CURRENT_CRUMB - 1, current: 900 })).toBe(1);
    expect(crumbsToHide({ ...base, available: 212 + MIN_CURRENT_CRUMB, current: 900 })).toBe(1);
    // without acme and api: 100 + 16 + 24 + 16 = 156
    expect(crumbsToHide({ ...base, available: 156 + MIN_CURRENT_CRUMB, current: 900 })).toBe(2);
  });

  it('drops every ancestor (and the "…") when nothing else fits', () => {
    expect(crumbsToHide({ ...base, available: 155 + MIN_CURRENT_CRUMB, current: 900 })).toBe(3);
    expect(crumbsToHide({ ...base, available: MIN_VISIBLE_CRUMB, current: 900 })).toBe(3);
  });

  it('drops the breadcrumb entirely rather than rendering a sliver', () => {
    expect(crumbsToHide({ ...base, available: MIN_VISIBLE_CRUMB - 1, current: 900 })).toBe(4);
    expect(crumbsToHide({ ...base, available: 0, current: 900 })).toBe(4);
    // a short current crumb that still fits whole stays
    expect(crumbsToHide({ ...base, available: 30, current: 30 })).toBe(3);
  });

  it('only reserves the current crumb its natural width when that is shorter', () => {
    expect(crumbsToHide({ ...base, available: full + 30, current: 30 })).toBe(0);
    expect(crumbsToHide({ ...base, available: full + 29, current: 30 })).toBe(1);
  });

  it('handles pages without ancestors', () => {
    expect(crumbsToHide({ ...base, ancestors: [], available: 100, current: 900 })).toBe(0);
    expect(crumbsToHide({ ...base, ancestors: [], available: 10, current: 900 })).toBe(1);
  });
});
