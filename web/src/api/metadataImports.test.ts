import { describe, expect, it } from 'vitest';
import { apiUrlFor, parseUserMap, sourceRepoFrom } from './metadataImports';

describe('metadata import helpers', () => {
  it('parses login maps', () => {
    const { map, error } = parseUserMap('# c\nocto,alice\nhubot = bob\nmannequin-user,mannequin-id,target-user\n"dev","MDQ6","dave"\n');
    expect(error).toBeNull();
    expect(map).toEqual({ octo: 'alice', hubot: 'bob', dev: 'dave' });
    expect(parseUserMap('lonely').error).toMatch(/Line 1/);
  });

  it('derives API URLs', () => {
    expect(apiUrlFor('')).toBe('https://api.github.com');
    expect(apiUrlFor('https://github.com')).toBe('https://api.github.com');
    expect(apiUrlFor('ghe.example')).toBe('https://ghe.example/api/v3');
    expect(apiUrlFor('https://ghe.example/api/v3/')).toBe('https://ghe.example/api/v3');
  });

  it('extracts owner/name', () => {
    expect(sourceRepoFrom('https://github.com/octo-org/hello-world.git')).toBe('octo-org/hello-world');
    expect(sourceRepoFrom(' octo-org/hello-world ')).toBe('octo-org/hello-world');
  });
});
