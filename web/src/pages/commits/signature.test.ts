import { describe, expect, it } from 'vitest';
import type { CommitSignature } from './Signature';
import { describeSignature } from './signature';

const sig = (p: Partial<CommitSignature>): CommitSignature => ({
  verified: true,
  reason: 'valid',
  key_type: 'gpg',
  key_id: 'B683A848506CA6DB',
  signer: { login: 'ada', avatar_url: '' },
  web_flow: false,
  ...p,
});

describe('describeSignature', () => {
  it('describes verified GPG, SSH and web-flow signatures', () => {
    expect(describeSignature(sig({}))).toEqual({
      label: 'Verified',
      detail: 'This commit was signed with the committer’s verified signature.',
      key: 'GPG Key ID: B683A848506CA6DB',
    });
    expect(describeSignature(sig({ key_type: 'ssh', key_id: 'SHA256:abc' })).key).toBe('SSH Key Fingerprint: SHA256:abc');
    expect(describeSignature(sig({ web_flow: true, signer: null })).detail).toContain('web-flow');
  });

  it('explains why a signature is unverified', () => {
    const t = describeSignature(sig({ verified: false, reason: 'unknown_key', signer: null }));
    expect(t.label).toBe('Unverified');
    expect(t.detail).toContain('isn’t registered');
    expect(describeSignature(sig({ verified: false, reason: 'bad_email' })).detail).toContain('committer email');
    expect(describeSignature(sig({ verified: false, reason: 'weird', key_id: null }))).toEqual({
      label: 'Unverified',
      detail: 'This signature couldn’t be verified (weird).',
      key: null,
    });
  });
});
