/** Wording of commit signature badges (GitHub's), kept pure for tests. */
import type { CommitSignature } from './Signature';

export interface SignatureText {
  label: 'Verified' | 'Unverified';
  /** One-sentence explanation shown in the badge popover. */
  detail: string;
  /** `GPG Key ID: …` / `SSH Key Fingerprint: …`, when known. */
  key: string | null;
}

const REASONS: Record<string, string> = {
  unknown_key: 'The key that made this signature isn’t registered with any account.',
  bad_email: 'The committer email isn’t an identity of the key, or belongs to another account.',
  unverified_email: 'The committer email isn’t verified on the signer’s account.',
  no_user: 'No account is associated with the committer email.',
  unknown_signature_type: 'This signature type isn’t supported (only GPG and SSH signatures are verified).',
  malformed_signature: 'The signature couldn’t be parsed.',
  invalid: 'The signature doesn’t match the commit contents.',
  expired_key: 'The key that made this signature had expired.',
  not_signing_key: 'The key that made this signature isn’t allowed to sign.',
  gpgverify_error: 'The signer’s key couldn’t be used to check this signature.',
  gpgverify_unavailable: 'Signature verification is unavailable right now.',
};

export function describeSignature(s: CommitSignature): SignatureText {
  const key = !s.key_id ? null : s.key_type === 'ssh' ? `SSH Key Fingerprint: ${s.key_id}` : `GPG Key ID: ${s.key_id}`;
  if (s.verified) {
    return {
      label: 'Verified',
      detail: s.web_flow
        ? 'This commit was created on this site and signed with its verified web-flow signature.'
        : 'This commit was signed with the committer’s verified signature.',
      key,
    };
  }
  return { label: 'Unverified', detail: REASONS[s.reason] ?? `This signature couldn’t be verified (${s.reason}).`, key };
}
