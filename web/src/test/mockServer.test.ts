import { describe, expect, it } from 'vitest';
import { mockFeatures } from '../mock/features';
import { call, get, newServer, post } from './mockServer';

describe('mock server test helpers', () => {
  it('loads the mock feature chunks on import', () => {
    expect(() => mockFeatures()).not.toThrow();
  });

  it('call() JSON-encodes the body, parses JSON and surfaces status and headers', async () => {
    const s = newServer();
    const ok = await call<{ name: string }>(s, 'POST', '/api/v3/user/repos', { name: 'helper-test' });
    expect(ok.status).toBe(201);
    expect(ok.body.name).toBe('helper-test');
    expect(ok.headers.get('content-type')).toContain('json');
    const bad = await post(s, '/api/v3/user/repos', { name: 'helper-test' });
    expect(bad.status).toBe(422);
    const missing = await get(s, '/api/v3/repos/nobody/nothing');
    expect(missing.status).toBe(404);
    expect(missing.body).toMatchObject({ message: 'Not Found' });
  });

  it('returns null for empty bodies and text for non-JSON responses', async () => {
    const s = newServer();
    expect((await call(s, 'DELETE', '/api/v3/user/starred/acme/api')).body).toBeNull();
    const repo = [...s.db.tables.repo.values()][0]!;
    const sha = (await get<{ sha: string }[]>(s, `/api/v3/repos/${repo.owner}/${repo.name}/commits`)).body[0]!.sha;
    const diff = await get(s, `/api/v3/repos/${repo.owner}/${repo.name}/commits/${sha}`, { accept: 'application/vnd.github.diff' });
    expect(typeof diff.body).toBe('string');
  });

  it('newServer() can start signed out', () => {
    expect(newServer().signedIn).toBe(true);
    expect(newServer({ signedIn: false }).signedIn).toBe(false);
  });
});
