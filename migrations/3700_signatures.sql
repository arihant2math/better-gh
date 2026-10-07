-- P25: commit/tag signature verification and SSH signing keys.

-- `/user/ssh_signing_keys`: SSH keys that verify commit signatures (separate
-- from authentication keys, like GitHub; the same key may be in both).
CREATE TABLE ssh_signing_keys (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id      BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title        TEXT NOT NULL DEFAULT '',
    key          TEXT NOT NULL,
    -- "SHA256:<base64>" fingerprint (same form as ssh_keys.fingerprint).
    fingerprint  TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ssh_signing_keys_fingerprint_key ON ssh_signing_keys (fingerprint);
CREATE INDEX ssh_signing_keys_user_idx ON ssh_signing_keys (user_id, id);

-- Verification results per signed object (commit or tag SHA). Objects are
-- immutable, so a row stays valid until the keys or e-mails it depended on
-- change: key and e-mail writes delete the rows naming that key
-- (`signer_key`) or e-mail (bgh_core::signatures).
CREATE TABLE signature_verifications (
    sha          TEXT PRIMARY KEY,
    verified     BOOLEAN NOT NULL,
    reason       TEXT NOT NULL,
    -- Key the signature names: OpenPGP key id (uppercase hex) or SSH
    -- fingerprint; NULL when the signature could not be parsed.
    signer_key   TEXT,
    -- Owner of the matching key (web-flow: NULL).
    signer_id    BIGINT REFERENCES users (id) ON DELETE CASCADE,
    -- Lowercased committer / tagger e-mail.
    email        TEXT NOT NULL DEFAULT '',
    verified_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX signature_verifications_signer_key_idx ON signature_verifications (signer_key);
CREATE INDEX signature_verifications_email_idx ON signature_verifications (email);
CREATE INDEX signature_verifications_signer_idx ON signature_verifications (signer_id);
