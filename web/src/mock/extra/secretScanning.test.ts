import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

type Json = Record<string, unknown>;

async function call(s: MockServer, method: string, path: string, body?: unknown) {
  const res = await s.fetch(path, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { 'Content-Type': 'application/json' } });
  const text = await res.text();
  return { status: res.status, body: text ? (JSON.parse(text) as unknown) : null, headers: res.headers };
}

describe('secret scanning mocks', () => {
  it('lists, filters, closes and reopens alerts', async () => {
    const s = new MockServer(null, {});
    const open = await call(s, 'GET', '/api/v3/repos/acme/api/secret-scanning/alerts?state=open');
    expect(open.status).toBe(200);
    const alerts = open.body as Json[];
    expect(alerts.map((a) => a.number)).toEqual([3, 2]);
    expect(alerts[0]!.push_protection_bypassed).toBe(true);
    expect((alerts[1]!.first_location_detected as Json).path).toBe('config/settings.toml');
    const aws = await call(s, 'GET', '/api/v3/repos/acme/api/secret-scanning/alerts?secret_type=aws_access_key_id');
    expect((aws.body as Json[]).map((a) => a.secret)).toEqual(['AKIAQ3EGRXPZ7K2LMN4D']);
    const locs = await call(s, 'GET', '/api/v3/repos/acme/api/secret-scanning/alerts/2/locations');
    expect((locs.body as Json[]).length).toBe(2);

    expect((await call(s, 'PATCH', '/api/v3/repos/acme/api/secret-scanning/alerts/2', { state: 'resolved', resolution: 'nope' })).status).toBe(422);
    const closed = await call(s, 'PATCH', '/api/v3/repos/acme/api/secret-scanning/alerts/2', { state: 'resolved', resolution: 'revoked', resolution_comment: 'Rotated' });
    expect(closed.body).toMatchObject({ state: 'resolved', resolution: 'revoked', resolution_comment: 'Rotated' });
    const reopened = await call(s, 'PATCH', '/api/v3/repos/acme/api/secret-scanning/alerts/2', { state: 'open' });
    expect(reopened.body).toMatchObject({ state: 'open', resolution: null, resolved_by: null });
  });

  it('answers 404 with a message when disabled, and enables through security_and_analysis', async () => {
    const s = new MockServer(null, {});
    const off = await call(s, 'GET', '/api/v3/repos/acme/design-system/secret-scanning/alerts');
    expect(off.status).toBe(404);
    expect((off.body as Json).message).toBe('Secret scanning is disabled on this repository.');
    const bad = await call(s, 'PATCH', '/api/v3/repos/acme/design-system', { security_and_analysis: { secret_scanning_push_protection: { status: 'enabled' } } });
    expect(bad.status).toBe(422);
    const on = await call(s, 'PATCH', '/api/v3/repos/acme/design-system', {
      security_and_analysis: { secret_scanning: { status: 'enabled' }, secret_scanning_push_protection: { status: 'enabled' } },
    });
    expect(on.status).toBe(200);
    expect((on.body as Json).security_and_analysis).toMatchObject({ secret_scanning: { status: 'enabled' }, secret_scanning_push_protection: { status: 'enabled' } });
    const settings = await call(s, 'GET', '/_bgh/repos/acme/design-system/secret-scanning/settings');
    expect(settings.body).toMatchObject({ secret_scanning: true, push_protection: true, non_provider_patterns: false });
    expect((await call(s, 'GET', '/api/v3/repos/acme/design-system/secret-scanning/alerts')).status).toBe(200);
    const history = await call(s, 'GET', '/api/v3/repos/acme/design-system/secret-scanning/scan-history');
    expect(((history.body as Json).backfill_scans as Json[])[0]!.status).toBe('pending');
  });

  it('bypasses a push block once and records an alert for "fix later"', async () => {
    const s = new MockServer(null, {});
    const P = '/_bgh/repos/acme/api/secret-scanning/push-blocks/2mQ8xVnR1kPq7sT3A9';
    const block = await call(s, 'GET', P);
    expect(block.body).toMatchObject({ secret_type: 'aws_access_key_id', path: 'deploy/terraform.tfvars', bypassed_at: null });
    expect(JSON.stringify(block.body)).not.toContain('AKIAZ7TQ4MNB2XK9LRPE');
    const B = '/api/v3/repos/acme/api/secret-scanning/push-protection-bypasses';
    const r = await call(s, 'POST', B, { reason: 'will_fix_later', placeholder_id: '2mQ8xVnR1kPq7sT3A9' });
    expect(r.status).toBe(200);
    expect((r.body as Json).expire_at).toBeTruthy();
    expect((await call(s, 'POST', B, { reason: 'will_fix_later', placeholder_id: '2mQ8xVnR1kPq7sT3A9' })).status).toBe(422);
    expect(((await call(s, 'GET', P)).body as Json).bypassed_at).toBeTruthy();
    const open = await call(s, 'GET', '/api/v3/repos/acme/api/secret-scanning/alerts?state=open');
    expect((open.body as Json[])[0]).toMatchObject({ number: 4, push_protection_bypassed: true });
  });

  it('manages and tests custom patterns, and lists org alerts', async () => {
    const s = new MockServer(null, {});
    const t = await call(s, 'POST', '/_bgh/secret-scanning/custom-patterns/test', { pattern: 'key_[0-9]+', test_string: 'a key_12 b key_9' });
    expect(t.body).toEqual({ valid: true, error: null, matches: [{ start: 2, end: 8, text: 'key_12' }, { start: 11, end: 16, text: 'key_9' }] });
    expect(((await call(s, 'POST', '/_bgh/secret-scanning/custom-patterns/test', { pattern: '(', test_string: '' })).body as Json).valid).toBe(false);
    const O = '/_bgh/orgs/acme/secret-scanning/custom-patterns';
    const c = await call(s, 'POST', O, { name: 'Internal', pattern: 'int_[a-z]{8}', push_protection: true });
    expect(c.status).toBe(201);
    expect((await call(s, 'POST', O, { name: 'internal', pattern: 'x' })).status).toBe(422);
    expect(((await call(s, 'GET', O)).body as Json[]).length).toBe(1);
    expect((await call(s, 'DELETE', `${O}/${(c.body as Json).id}`)).status).toBe(204);
    const org = await call(s, 'GET', '/api/v3/orgs/acme/secret-scanning/alerts?state=open');
    const full = (org.body as Json[]).map((a) => (a.repository as Json).full_name);
    expect(full).toContain('acme/api');
    expect(full).toContain('acme/web');
  });
});
