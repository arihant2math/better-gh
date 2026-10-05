import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, {
    method,
    body: body === undefined ? undefined : JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  });
  const text = await res.text();
  return {
    status: res.status,
    headers: res.headers,
    body: text ? (JSON.parse(text) as Json & Json[]) : null,
  };
}

const R = '/api/v3/repos/acme/api';
const release = {
  name: 'Releases',
  target: 'branch',
  enforcement: 'active',
  conditions: { ref_name: { include: ['refs/heads/release/*'], exclude: [] } },
  rules: [
    { type: 'deletion' },
    {
      type: 'pull_request',
      parameters: { required_approving_review_count: 2 },
    },
  ],
  bypass_actors: [{ actor_id: 5, actor_type: 'RepositoryRole', bypass_mode: 'always' }],
};

describe('rulesets mock', () => {
  it('creates, reads, updates and deletes repository rulesets', async () => {
    const s = new MockServer(null, {});
    const created = await call(s, 'POST', `${R}/rulesets`, release);
    expect(created.status).toBe(201);
    const id = created.body!.id as number;
    expect(created.body).toMatchObject({
      name: 'Releases',
      source_type: 'Repository',
      source: 'acme/api',
      enforcement: 'active',
    });
    // Parameters are normalized with GitHub's defaults.
    expect((created.body!.rules as Json[])[1]!.parameters).toMatchObject({
      required_approving_review_count: 2,
      allowed_merge_methods: ['merge', 'squash', 'rebase'],
    });

    const list = await call(s, 'GET', `${R}/rulesets`);
    const names = list.body!.map((r) => r.name);
    expect(names).toContain('Releases');
    expect(names).toContain('Default branch baseline'); // org ruleset (includes_parents)
    expect(list.body![0]).not.toHaveProperty('rules');
    expect((await call(s, 'GET', `${R}/rulesets?includes_parents=false`)).body!.map((r) => r.name)).not.toContain('Default branch baseline');
    expect((await call(s, 'GET', `${R}/rulesets?targets=tag`)).body!.every((r) => r.target === 'tag')).toBe(true);

    const one = await call(s, 'GET', `${R}/rulesets/${id}`);
    expect(one.body!.bypass_actors).toEqual(release.bypass_actors);

    const upd = await call(s, 'PUT', `${R}/rulesets/${id}`, {
      ...release,
      enforcement: 'evaluate',
    });
    expect(upd.status).toBe(200);
    expect(upd.body!.enforcement).toBe('evaluate');

    expect((await call(s, 'POST', `${R}/rulesets`, release)).status).toBe(422); // duplicate name
    expect((await call(s, 'DELETE', `${R}/rulesets/${id}`)).status).toBe(204);
    expect((await call(s, 'GET', `${R}/rulesets/${id}`)).status).toBe(404);
  });

  it('validates like the backend', async () => {
    const s = new MockServer(null, {});
    const err = async (body: Json) => {
      const r = await call(s, 'POST', `${R}/rulesets`, body);
      expect(r.status).toBe(422);
      return ((r.body!.errors as Json[])[0] ?? {}) as Json;
    };
    expect((await err({ ...release, name: undefined })).code).toBe('missing_field');
    expect((await err({ ...release, enforcement: 'sometimes' })).field).toBe('enforcement');
    expect((await err({ ...release, rules: [{ type: 'nope' }] })).message).toBe("Invalid rule 'nope'");
    expect((await err({ ...release, target: 'push', rules: [{ type: 'deletion' }] })).message).toBe("Invalid rule 'deletion' for a push ruleset");
    expect(
      (
        await err({
          ...release,
          rules: [{ type: 'max_file_size', parameters: { max_file_size: 500 } }],
        })
      ).field,
    ).toBe('rules');
    expect(
      (
        await err({
          ...release,
          bypass_actors: [{ actor_type: 'Team', actor_id: 99999 }],
        })
      ).message,
    ).toMatch(/Unknown team/);
    expect(
      (
        await err({
          ...release,
          rules: [
            {
              type: 'commit_message_pattern',
              parameters: { operator: 'regex', pattern: '(' },
            },
          ],
        })
      ).message,
    ).toMatch(/regular expression/);
  });

  it('serves branch rules and marks branches protected', async () => {
    const s = new MockServer(null, {});
    await call(s, 'POST', `${R}/rulesets`, {
      ...release,
      conditions: { ref_name: { include: ['~ALL'], exclude: [] } },
    });
    const rules = await call(s, 'GET', `${R}/rules/branches/main`);
    expect(rules.status).toBe(200);
    expect(rules.body!.find((r) => r.type === 'deletion')).toMatchObject({
      ruleset_source_type: 'Repository',
      ruleset_source: 'acme/api',
    });
    const branches = await call(s, 'GET', `${R}/branches`);
    expect(branches.body!.every((b) => b.protected)).toBe(true);
  });

  it('manages organization rulesets with repository targeting', async () => {
    const s = new MockServer(null, {});
    const O = '/api/v3/orgs/acme/rulesets';
    expect((await call(s, 'POST', O, release)).status).toBe(422); // needs a repository condition
    const c = await call(s, 'POST', O, {
      ...release,
      conditions: {
        ...release.conditions,
        repository_name: { include: ['api'], exclude: [] },
      },
    });
    expect(c.status).toBe(201);
    expect(c.body).toMatchObject({
      source_type: 'Organization',
      source: 'acme',
    });
    expect((c.body!.conditions as Json).repository_name).toEqual({
      include: ['api'],
      exclude: [],
      protected: false,
    });
    expect((await call(s, 'GET', `${R}/rulesets`)).body!.map((r) => r.name)).toContain('Releases');
    expect((await call(s, 'GET', '/api/v3/repos/acme/web/rulesets')).body!.map((r) => r.name)).not.toContain('Releases');
    expect((await call(s, 'GET', O)).body!.length).toBe(2);
    expect((await call(s, 'DELETE', `${O}/${c.body!.id as number}`)).status).toBe(204);
    expect((await call(s, 'GET', '/api/v3/orgs/nobody-org/rulesets')).status).toBe(404);
  });

  it('serves rule suites with filters', async () => {
    const s = new MockServer(null, {});
    const all = await call(s, 'GET', `${R}/rulesets/rule-suites?time_period=month`);
    expect(all.status).toBe(200);
    expect(all.body!.length).toBeGreaterThan(3);
    expect(all.body![0]).not.toHaveProperty('rule_evaluations');
    const failed = await call(s, 'GET', `${R}/rulesets/rule-suites?time_period=month&rule_suite_result=fail`);
    expect(failed.body!.every((x) => x.result === 'fail')).toBe(true);
    const tag = await call(s, 'GET', `${R}/rulesets/rule-suites?time_period=month&ref=v1.2.0`);
    expect(tag.body!.map((x) => x.ref)).toEqual(['refs/tags/v1.2.0']);
    const detail = await call(s, 'GET', `${R}/rulesets/rule-suites/${failed.body![0]!.id as number}`);
    expect(detail.body!.rule_evaluations).toBeInstanceOf(Array);
    expect((await call(s, 'GET', `${R}/rulesets/rule-suites?time_period=year`)).status).toBe(422);
    const org = await call(s, 'GET', '/api/v3/orgs/acme/rulesets/rule-suites?time_period=month&repository_name=api');
    expect(org.body!.every((x) => x.repository_name === 'api')).toBe(true);
  });
});
