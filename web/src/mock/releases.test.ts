import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../test/mockServer';

describe('mock releases backend', () => {
  const s = newServer({ now: Date.UTC(2026, 9, 1) });
  const writable = new Set([...s.db.tables.viewerRepo.values()].filter((v) => ['admin', 'maintain', 'write'].includes(v.permission)).map((v) => v.id));
  const repo = [...s.db.tables.repo.values()].find((r) => writable.has(r.id))!;
  const base = `/api/v3/repos/${repo.owner}/${repo.name}/releases`;

  it('lists seeded releases newest first with an html body on request', async () => {
    const list = await call(s, 'GET', `${base}?per_page=2`, undefined, { accept: 'application/vnd.github.html+json' });
    expect(list.status).toBe(200);
    expect(list.body.map((r: Json) => r.tag_name)).toEqual(['v0.3.1-rc.1', 'v0.3.0']);
    expect(list.body[1].body_html).toContain('<h2');
    const latest = await call(s, 'GET', `${base}/latest`);
    expect(latest.body.tag_name).toBe('v0.3.0'); // the pre-release is skipped
  });

  let id = 0;
  it('creates a draft, rejects duplicates and publishes with a new tag', async () => {
    expect((await call(s, 'POST', base, { name: 'x' })).status).toBe(422);
    const dup = await call(s, 'POST', base, { tag_name: 'v0.3.0' });
    expect(dup.status).toBe(422);
    expect(dup.body.errors[0]).toMatchObject({ field: 'tag_name', code: 'already_exists' });

    const draft = await call(s, 'POST', base, { tag_name: 'v1.0.0', target_commitish: repo.defaultBranch, name: 'One', body: 'Hello', draft: true });
    expect(draft.status).toBe(201);
    expect(draft.body.draft).toBe(true);
    expect(draft.body.published_at).toBeNull();
    id = draft.body.id;
    // Drafts don't create the tag and aren't served by tag.
    expect((await call(s, 'GET', `${base}/tags/v1.0.0`)).status).toBe(404);
    const tagsBefore = await call(s, 'GET', `/api/v3/repos/${repo.owner}/${repo.name}/tags`);
    expect(tagsBefore.body.map((t: Json) => t.name)).not.toContain('v1.0.0');

    const pub = await call(s, 'PATCH', `${base}/${id}`, { draft: false });
    expect(pub.status).toBe(200);
    expect(pub.body.published_at).toBeTruthy();
    const tags = await call(s, 'GET', `/api/v3/repos/${repo.owner}/${repo.name}/tags`);
    expect(tags.body.map((t: Json) => t.name)).toContain('v1.0.0');
    expect((await call(s, 'GET', `${base}/tags/v1.0.0`)).body.id).toBe(id);
  });

  it('computes latest with make_latest', async () => {
    expect((await call(s, 'GET', `${base}/latest`)).body.id).toBe(id);
    // An explicit "true" on an older release wins.
    const old = (await call(s, 'GET', `${base}/tags/v0.2.0`)).body;
    await call(s, 'PATCH', `${base}/${old.id}`, { make_latest: 'true' });
    expect((await call(s, 'GET', `${base}/latest`)).body.id).toBe(old.id);
    await call(s, 'PATCH', `${base}/${id}`, { make_latest: 'true' });
    expect((await call(s, 'GET', `${base}/latest`)).body.id).toBe(id);
    // "false" excludes a release from the computed latest.
    await call(s, 'PATCH', `${base}/${id}`, { make_latest: 'false' });
    expect((await call(s, 'GET', `${base}/latest`)).body.tag_name).toBe('v0.3.0');
  });

  it('uploads, lists and deletes assets', async () => {
    const blob = new Blob(['hello world'], { type: 'text/plain' });
    const up = await call(s, 'POST', `/api/uploads/repos/${repo.owner}/${repo.name}/releases/${id}/assets?name=notes.txt`, blob);
    expect(up.status).toBe(201);
    expect(up.body).toMatchObject({ name: 'notes.txt', size: 11, content_type: 'text/plain', download_count: 0, state: 'uploaded' });
    expect(up.body.browser_download_url).toContain('/releases/download/v1.0.0/notes.txt');
    const again = await call(s, 'POST', `/api/uploads/repos/${repo.owner}/${repo.name}/releases/${id}/assets?name=notes.txt`, blob);
    expect(again.status).toBe(422);
    const rel = await call(s, 'GET', `${base}/${id}`);
    expect(rel.body.assets.map((a: Json) => a.name)).toEqual(['notes.txt']);
    const dl = await call(s, 'GET', `/${repo.owner}/${repo.name}/releases/download/v1.0.0/notes.txt`);
    expect(dl.body).toBe('hello world');
    expect((await call(s, 'GET', `${base}/assets/${up.body.id}`)).body.download_count).toBe(1);
    expect((await call(s, 'DELETE', `${base}/assets/${up.body.id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${base}/${id}/assets`)).body).toEqual([]);
  });

  it('generates release notes between tags', async () => {
    const notes = await call(s, 'POST', `${base}/generate-notes`, { tag_name: 'v0.3.0', previous_tag_name: 'v0.2.0' });
    expect(notes.status).toBe(200);
    expect(notes.body.name).toBe('v0.3.0');
    expect(notes.body.body).toContain("## What's Changed");
    expect(notes.body.body).toContain('v0.2.0...v0.3.0');
    expect(notes.body.body.match(/^\* /gm)!.length).toBeGreaterThan(0);
    // Auto previous tag for a new tag on the default branch.
    const auto = await call(s, 'POST', `${base}/generate-notes`, { tag_name: 'v2.0.0', target_commitish: repo.defaultBranch });
    expect(auto.body.body).toContain('...v2.0.0');
  });

  it('deletes a release but keeps its tag', async () => {
    expect((await call(s, 'DELETE', `${base}/${id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${base}/${id}`)).status).toBe(404);
    const tags = await call(s, 'GET', `/api/v3/repos/${repo.owner}/${repo.name}/tags`);
    expect(tags.body.map((t: Json) => t.name)).toContain('v1.0.0');
  });
});
