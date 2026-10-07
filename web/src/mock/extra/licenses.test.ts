import { describe, expect, it } from 'vitest';
import { call, newServer, type Json } from '../../test/mockServer';

describe('template mocks', () => {
  it('serves .gitignore templates and licenses', async () => {
    const s = newServer();
    const names = (await call(s, 'GET', '/api/v3/gitignore/templates')).body as unknown as string[];
    expect(names).toContain('Go');
    const go = await call(s, 'GET', '/api/v3/gitignore/templates/Go');
    expect(go.body).toMatchObject({ name: 'Go' });
    expect(go.body!.source).toContain('go.work');
    expect((await call(s, 'GET', '/api/v3/gitignore/templates/Nope')).status).toBe(404);

    const licenses = (await call(s, 'GET', '/api/v3/licenses?per_page=100')).body as unknown as Json[];
    expect(licenses.map((l) => l.key)).toEqual(['agpl-3.0', 'apache-2.0', 'bsd-2-clause', 'bsd-3-clause', 'bsl-1.0', 'cc0-1.0', 'epl-2.0', 'gpl-2.0', 'gpl-3.0', 'lgpl-2.1', 'mit', 'mpl-2.0', 'unlicense']);
    expect(licenses.find((l) => l.key === 'mit')).toMatchObject({ name: 'MIT License', spdx_id: 'MIT' });
    expect(((await call(s, 'GET', '/api/v3/licenses?per_page=5&page=3')).body as unknown as Json[]).map((l) => l.key)).toEqual(['mit', 'mpl-2.0', 'unlicense']);
    const mit = await call(s, 'GET', '/api/v3/licenses/mit');
    expect(mit.body).toMatchObject({ key: 'mit', featured: true });
    expect(mit.body!.body).toContain('[year]');
    expect((await call(s, 'GET', '/api/v3/licenses/wtfpl')).status).toBe(404);
  });

  it('creates repositories from templates', async () => {
    const s = newServer();
    expect((await call(s, 'POST', '/api/v3/user/repos', { name: 'bad-tpl', gitignore_template: 'Cobol' })).status).toBe(422);
    expect((await call(s, 'POST', '/api/v3/user/repos', { name: 'bad-lic', license_template: 'wtfpl' })).status).toBe(422);
    const created = await call(s, 'POST', '/api/v3/user/repos', { name: 'templated', gitignore_template: 'Go', license_template: 'apache-2.0' });
    expect(created.status).toBe(201);
    expect(created.body!.pushed_at).toBeTruthy();
    const full = await call(s, 'GET', '/api/v3/repos/ada/templated');
    expect(full.body!.license).toMatchObject({ key: 'apache-2.0', spdx_id: 'Apache-2.0' });
    const raw = await s.fetch('/ada/templated/raw/main/.gitignore');
    expect(await raw.text()).toContain('go.work');

    const team = [...s.db.tables.team.values()].find((t) => s.db.tables.org.get(t.orgId)?.login === 'acme')!;
    const org = await call(s, 'POST', '/api/v3/orgs/acme/repos', { name: 'with-team', team_id: team.id });
    expect(org.status).toBe(201);
    expect(s.db.tables.team.get(team.id)!.repoIds).toContain(org.body!.id);
    expect((await call(s, 'POST', '/api/v3/orgs/acme/repos', { name: 'bad-team', team_id: 999999 })).status).toBe(422);
  });
});
