import { describe, expect, it } from 'vitest';
import { splitRefPath } from './util';

const refs = {
  branches: [
    { name: 'main', sha: 'a1' },
    { name: 'feature/big-refactor', sha: 'b2' },
    { name: 'release', sha: 'c3' },
  ],
  tags: [
    { name: 'v1.0', sha: 'd4' },
    { name: 'release/v2', sha: 'e5' },
    { name: 'main', sha: 'f6' },
  ],
};

describe('splitRefPath', () => {
  it('keeps slash branch names whole', () => {
    expect(splitRefPath(refs, 'feature', 'big-refactor')).toEqual({ ref: 'feature/big-refactor', path: '', commit: 'b2', kind: 'branch' });
    expect(splitRefPath(refs, 'feature', 'big-refactor/src/server.rs')).toEqual({ ref: 'feature/big-refactor', path: 'src/server.rs', commit: 'b2', kind: 'branch' });
  });

  it('prefers the longest ref across branches and tags', () => {
    expect(splitRefPath(refs, 'release', 'v2/src')).toMatchObject({ ref: 'release/v2', path: 'src', kind: 'tag' });
    expect(splitRefPath(refs, 'release', 'src')).toMatchObject({ ref: 'release', path: 'src', kind: 'branch' });
  });

  it('prefers a branch over a tag of the same name', () => {
    expect(splitRefPath(refs, 'main', 'README.md')).toEqual({ ref: 'main', path: 'README.md', commit: 'a1', kind: 'branch' });
    expect(splitRefPath(refs, 'v1.0', '')).toMatchObject({ ref: 'v1.0', kind: 'tag' });
  });

  it('returns null for unknown refs', () => {
    expect(splitRefPath(refs, 'feature', 'src')).toBeNull();
  });
});
