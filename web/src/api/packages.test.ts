import { describe, expect, it } from 'vitest';
import { defaultTag, packageHref, packageRestPath, packagesHref, pullCommand, shortDigest } from './packages';

const meta = (tags: string[]) => ({ metadata: { package_type: 'container', container: { tags } } });

describe('pullCommand', () => {
  it('uses the tag and lowercases owner/name', () => {
    expect(pullCommand('localhost:3000', 'Acme', 'API', { tag: 'v1.2.0' })).toBe('docker pull localhost:3000/acme/api:v1.2.0');
  });
  it('falls back to the digest for untagged versions', () => {
    expect(pullCommand('ghcr.example', 'ada', 'base/node', { tag: null, digest: 'sha256:abc' })).toBe('docker pull ghcr.example/ada/base/node@sha256:abc');
  });
  it('defaults to :latest', () => {
    expect(pullCommand('r', 'o', 'n')).toBe('docker pull r/o/n:latest');
  });
});

describe('package paths', () => {
  const org = { owner: { login: 'acme', type: 'Organization' }, package_type: 'container', name: 'base-images/node' };
  const user = { owner: { login: 'ada', type: 'User' }, package_type: 'container', name: 'aoc' };
  it('encodes slashes in names', () => {
    expect(packageHref(org)).toBe('/orgs/acme/packages/container/package/base-images%2Fnode');
    expect(packageHref(user)).toBe('/users/ada/packages/container/package/aoc');
  });
  it('builds REST and list paths by owner type', () => {
    expect(packageRestPath(org)).toBe('/api/v3/orgs/acme/packages/container/base-images%2Fnode');
    expect(packageRestPath(user)).toBe('/api/v3/users/ada/packages/container/aoc');
    expect(packagesHref({ login: 'acme', type: 'Organization' })).toBe('/orgs/acme/packages');
  });
});

describe('helpers', () => {
  it('shortDigest strips the algorithm', () => {
    expect(shortDigest('sha256:0123456789abcdef0123')).toBe('0123456789ab');
  });
  it('defaultTag prefers latest', () => {
    expect(defaultTag([meta(['v2']), meta(['v1', 'latest'])])).toBe('latest');
    expect(defaultTag([meta([]), meta(['v1'])])).toBe('v1');
    expect(defaultTag([meta([])])).toBeNull();
  });
});
