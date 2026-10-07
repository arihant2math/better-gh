import { describe, expect, it } from 'vitest';
import { splitRefPath } from '../../pages/code/util';
import { blameUrl, blobUrl, codeUrl, historyUrl, parseCodeUrl, rawUrl, treeUrl } from './urls';

const t = { owner: 'acme', repo: 'api' };

describe('code URL builders', () => {
  it('keeps a ref’s slashes literal and encodes each segment', () => {
    expect(treeUrl(t, 'feature/x')).toBe('/acme/api/tree/feature/x');
    expect(blobUrl(t, 'feature/x', 'docs/a#b.md')).toBe('/acme/api/blob/feature/x/docs/a%23b.md');
    expect(rawUrl(t, 'main', '50%.txt')).toBe('/acme/api/raw/main/50%25.txt');
    expect(blameUrl(t, 'v1.0', 'src/what?.rs')).toBe('/acme/api/blame/v1.0/src/what%3F.rs');
    expect(historyUrl(t, 'main', 'my dir/f.ts')).toBe('/acme/api/commits/main/my%20dir/f.ts');
    expect(historyUrl(t, 'main')).toBe('/acme/api/commits/main');
    expect(codeUrl(t, 'edit', 'fix/#1', 'a.ts')).toBe('/acme/api/edit/fix/%231/a.ts');
  });

  it('drops leading and trailing slashes of the path', () => {
    expect(treeUrl(t, 'main', '/src/')).toBe('/acme/api/tree/main/src');
    expect(treeUrl(t, 'main', '')).toBe('/acme/api/tree/main');
  });
});

describe('parseCodeUrl', () => {
  it('reads the view by position, not by substring', () => {
    expect(parseCodeUrl('/acme/blob/tree/main')).toEqual({ owner: 'acme', repo: 'blob', view: 'tree', ref: 'main', rest: '' });
    expect(parseCodeUrl('/blame/r/tree/main/src')).toMatchObject({ owner: 'blame', repo: 'r', view: 'tree' });
    expect(parseCodeUrl('/acme/blame/blob/main/README.md')).toMatchObject({ repo: 'blame', view: 'blob', rest: 'README.md' });
    expect(parseCodeUrl('/o/r/tree/main/src/blob/x')).toMatchObject({ view: 'tree', ref: 'main', rest: 'src/blob/x' });
    expect(parseCodeUrl('/o/r/blob/main/blame/blob/a.ts')).toMatchObject({ view: 'blob', rest: 'blame/blob/a.ts' });
    expect(parseCodeUrl('/o/raw/tree/main')).toMatchObject({ repo: 'raw', view: 'tree' });
  });

  it('matches routes case-insensitively', () => {
    expect(parseCodeUrl('/o/r/BLOB/main/x')?.view).toBe('blob');
  });

  it('is null for non-code URLs', () => {
    expect(parseCodeUrl('/acme/api')).toBeNull();
    expect(parseCodeUrl('/acme/api/issues/1')).toBeNull();
    expect(parseCodeUrl('/acme/api/tree')).toBeNull();
    expect(parseCodeUrl('/acme/api/blob/%E0%A4%A')).toBeNull();
  });

  it('round-trips refs with slashes and paths with #, % and /blob/', () => {
    const refs = { branches: [{ name: 'feature/x', sha: 'b1' }, { name: 'v1', sha: 'c1' }], tags: [{ name: 'v1/x', sha: 'd1' }] };
    for (const [ref, path] of [
      ['feature/x', 'a#b/50%.txt'],
      ['feature/x', 'src/blob/main.rs'],
      ['v1/x', 'README.md'],
      ['main', 'blame/blob/tree'],
    ] as const) {
      const p = parseCodeUrl(blobUrl(t, ref, path))!;
      expect(p.view).toBe('blob');
      const split = splitRefPath({ ...refs, branches: [...refs.branches, { name: 'main', sha: 'a1' }] }, p.ref, p.rest);
      expect(split).toMatchObject({ ref, path });
    }
  });
});
