import { describe, expect, it } from 'vitest';
import { MockServer } from './server';

const call = async (s: MockServer, method: string, path: string, body?: unknown) => {
  const r = await s.fetch(path, { method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  const ct = r.headers.get('content-type') ?? '';
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- loose JSON in tests
  const data: any = ct.includes('json') ? await r.json() : await r.text();
  return { status: r.status, body: data };
};

const b64 = (text: string) => {
  const bytes = new TextEncoder().encode(text);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
};

describe('mock contents writes', () => {
  const s = new MockServer(null, { now: Date.UTC(2026, 9, 1) });
  const repo = [...s.db.tables.repo.values()].find((r) => {
    const p = s.db.tables.viewerRepo.get(r.id)?.permission;
    return (p === 'admin' || p === 'write' || p === 'maintain') && !r.name.includes('protected');
  })!;
  const api = `/api/v3/repos/${repo.owner}/${repo.name}`;
  const browse = `/_bgh/repos/${repo.owner}/${repo.name}`;
  const branch = repo.defaultBranch;

  const firstFile = async () => {
    const tree = await call(s, 'GET', `${browse}/files/${branch}`);
    const path: string = tree.body.paths[0];
    const blob = await call(s, 'GET', `${browse}/blob/${branch}/${path}`);
    return { path, sha: blob.body.sha as string };
  };

  it('updates a file and rejects a stale sha with 409', async () => {
    const { path, sha } = await firstFile();
    const ok = await call(s, 'PUT', `${api}/contents/${path}`, { message: 'Update', content: b64('héllo\n'), sha, branch });
    expect(ok.status).toBe(200);
    expect(ok.body.commit.sha).toMatch(/^[0-9a-f]{40}$/);
    const raw = await call(s, 'GET', `/${repo.owner}/${repo.name}/raw/${branch}/${path}`);
    expect(raw.body).toBe('héllo\n');
    const blob = await call(s, 'GET', `${browse}/blob/${branch}/${path}`);
    expect(blob.body.sha).toBe(ok.body.content.sha);

    const stale = await call(s, 'PUT', `${api}/contents/${path}`, { message: 'Again', content: b64('x'), sha, branch });
    expect(stale.status).toBe(409);
    expect(stale.body.message).toBe(`${path} does not match ${sha}`);
    const noSha = await call(s, 'PUT', `${api}/contents/${path}`, { message: 'Again', content: b64('x'), branch });
    expect(noSha.status).toBe(422);
    expect(noSha.body.message).toContain('"sha" wasn\'t supplied.');
  });

  it('creates and deletes a file', async () => {
    const created = await call(s, 'PUT', `${api}/contents/docs/new%20file.md`, { message: 'Create docs/new file.md', content: b64('# Hi\n'), branch });
    expect(created.status).toBe(201);
    expect(created.body.content.path).toBe('docs/new file.md');
    const blob = await call(s, 'GET', `${browse}/blob/${branch}/docs/new%20file.md`);
    expect(blob.status).toBe(200);

    const del = await call(s, 'DELETE', `${api}/contents/docs/new%20file.md`, { message: 'Delete', sha: created.body.content.sha, branch });
    expect(del.status).toBe(200);
    expect(del.body.content).toBeNull();
    const gone = await call(s, 'GET', `${browse}/blob/${branch}/docs/new%20file.md`);
    expect(gone.status).toBe(404);
  });

  it('commits several files at once through the git data API', async () => {
    const refs = await call(s, 'GET', `${browse}/refs`);
    const tip: string = refs.body.branches.find((b: { name: string }) => b.name === branch).sha;
    const { path: victim } = await firstFile();
    const commit = await call(s, 'GET', `${api}/git/commits/${tip}`);
    expect(commit.status).toBe(200);
    const blob = await call(s, 'POST', `${api}/git/blobs`, { content: b64('binary-ish ✓'), encoding: 'base64' });
    expect(blob.status).toBe(201);
    const tree = await call(s, 'POST', `${api}/git/trees`, {
      base_tree: commit.body.tree.sha,
      tree: [
        { path: 'uploads/a.txt', mode: '100644', type: 'blob', sha: blob.body.sha },
        { path: 'uploads/b.txt', mode: '100644', type: 'blob', content: 'bee\n' },
        { path: victim, mode: '100644', type: 'blob', sha: null },
      ],
    });
    expect(tree.status).toBe(201);
    const created = await call(s, 'POST', `${api}/git/commits`, { message: 'Add files via upload', tree: tree.body.sha, parents: [tip] });
    expect(created.status).toBe(201);
    // Not a fast-forward from an unrelated commit.
    const orphan = await call(s, 'POST', `${api}/git/commits`, { message: 'orphan', tree: tree.body.sha, parents: [] });
    const rejected = await call(s, 'PATCH', `${api}/git/refs/heads/${branch}`, { sha: orphan.body.sha, force: false });
    expect(rejected.status).toBe(422);
    const moved = await call(s, 'PATCH', `${api}/git/refs/heads/${branch}`, { sha: created.body.sha, force: false });
    expect(moved.status).toBe(200);

    const files = await call(s, 'GET', `${browse}/files/${branch}`);
    expect(files.body.paths).toContain('uploads/a.txt');
    expect(files.body.paths).toContain('uploads/b.txt');
    expect(files.body.paths).not.toContain(victim);
    const raw = await call(s, 'GET', `/${repo.owner}/${repo.name}/raw/${branch}/uploads/a.txt`);
    expect(raw.body).toBe('binary-ish ✓');
    const detail = await call(s, 'GET', `${api}/commits/${created.body.sha}`);
    expect(detail.body.files).toHaveLength(3);
  });
});
