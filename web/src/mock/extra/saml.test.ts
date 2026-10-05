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
  return { status: res.status, body: text ? (JSON.parse(text) as Json) : null };
}

const SCIM = '/api/v3/scim/v2/enterprises/enterprise';

describe('SAML / SCIM mock', () => {
  it('advertises SAML on the site info and keeps the SP key write-only', async () => {
    const s = new MockServer(null, {});
    expect((await call(s, 'GET', '/_bgh/site')).body!.saml).toEqual({ display_name: 'Okta', login_url: '/_bgh/saml/login' });
    const kp = await call(s, 'POST', '/_bgh/admin/saml/keypair');
    expect(kp.status).toBe(201);
    const { certificate, private_key } = kp.body as { certificate: string; private_key: string };
    const settings = (await call(s, 'GET', '/_bgh/admin/settings')).body!;
    const ap = settings.auth_providers as Json;
    const saml = { ...(ap.saml as Json), sp_certificate: certificate, sp_private_key: private_key };
    const saved = await call(s, 'PATCH', '/_bgh/admin/settings', { auth_providers: { ...ap, saml } });
    expect(saved.status).toBe(200);
    expect(((saved.body!.auth_providers as Json).saml as Json).sp_private_key).toBe('********');
    // Sending the placeholder back keeps the key.
    const again = await call(s, 'PATCH', '/_bgh/admin/settings', { auth_providers: { ...ap, saml: { ...saml, sp_private_key: '********', require_encrypted_assertions: true } } });
    expect(again.status).toBe(200);
    const info = (await call(s, 'GET', '/_bgh/admin/saml')).body!;
    expect(info.sp_private_key_set).toBe(true);
    expect((info.sp_certificate as Json).fingerprint_sha256).toMatch(/^([0-9A-F]{2}:){31}[0-9A-F]{2}$/);
    expect(info.idp_certificates).toHaveLength(1);
  });

  it('validates SAML settings like the backend', async () => {
    const s = new MockServer(null, {});
    const ap = (await call(s, 'GET', '/_bgh/admin/settings')).body!.auth_providers as Json;
    const r = await call(s, 'PATCH', '/_bgh/admin/settings', { auth_providers: { ...ap, saml: { ...(ap.saml as Json), idp_sso_url: 'nope' } } });
    expect(r.status).toBe(422);
    expect((r.body!.errors as Json[])[0]!.field).toBe('auth_providers.saml.idp_sso_url');
  });

  it('parses IdP metadata', async () => {
    const s = new MockServer(null, {});
    const xml = `<md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="https://idp.test"><md:IDPSSODescriptor>
      <md:KeyDescriptor use="signing"><ds:KeyInfo><ds:X509Data><ds:X509Certificate>MIIBAAAA</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor>
      <md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="https://idp.test/post"/>
      <md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="https://idp.test/sso"/>
    </md:IDPSSODescriptor></md:EntityDescriptor>`;
    const r = await call(s, 'POST', '/_bgh/admin/saml/idp_metadata', { metadata: xml });
    expect(r.body).toEqual({
      idp_entity_id: 'https://idp.test',
      idp_sso_url: 'https://idp.test/sso',
      idp_slo_url: null,
      idp_certificate: '-----BEGIN CERTIFICATE-----\nMIIBAAAA\n-----END CERTIFICATE-----\n',
    });
    expect((await call(s, 'POST', '/_bgh/admin/saml/idp_metadata', { metadata: '<x/>' })).status).toBe(422);
  });

  it('lists SCIM users and groups with filters and paging, 404 when disabled', async () => {
    const s = new MockServer(null, {});
    const all = (await call(s, 'GET', `${SCIM}/Users?startIndex=1&count=2`)).body!;
    expect(all).toMatchObject({ totalResults: 4, itemsPerPage: 2, startIndex: 1 });
    const next = (await call(s, 'GET', `${SCIM}/Users?startIndex=3&count=2`)).body!;
    expect((next.Resources as Json[]).map((u) => u.userName)).toEqual(['linus', 'ken']);
    const one = (await call(s, 'GET', `${SCIM}/Users?filter=${encodeURIComponent('userName eq "Grace"')}`)).body!;
    expect((one.Resources as Json[]).map((u) => u.userName)).toEqual(['grace']);
    const groups = (await call(s, 'GET', `${SCIM}/Groups`)).body!;
    expect(((groups.Resources as Json[])[0]!.members as Json[]).length).toBe(3);
    const ap = (await call(s, 'GET', '/_bgh/admin/settings')).body!.auth_providers as Json;
    await call(s, 'PATCH', '/_bgh/admin/settings', { auth_providers: { ...ap, scim: { enabled: false } } });
    expect((await call(s, 'GET', `${SCIM}/Users`)).status).toBe(404);
  });
});
