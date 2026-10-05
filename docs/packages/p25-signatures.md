Integration: landed
Commit/tag signature verification (GPG + SSH), SSH signing keys API/UI, web-flow signing of server commits, required_signatures on push/merge/ref API, Verified badges; gate green after merging the integration branch (c783bac, with P23).

# P25 — Commit and tag signature verification, SSH signing keys, required_signatures, web-flow signing

Branch `bgh/p25-signatures` (PHASE4_PLAN §3 P25). Migration
`migrations/3700_signatures.sql` (range 3700–3799).

## What's implemented

### Verification (`bgh_git::signing`, `bgh_repos::signatures`)

* Signature parsing: `Commit` / `Tag` now carry `payload` (the object
  without its `gpgsig` header / trailing signature; required in serialized
  form, so pre-P25 cached commit lists are recomputed).
* OpenPGP via rPGP (`pgp` 0.21): issuer key ids (issuer and issuer
  fingerprint subpackets) → `gpg_keys.key_id` (primary or subkey); the
  stored armored certificate (`raw_key` of the primary) verifies the
  signature; a subkey needs a valid binding signature. Expiry
  (`expires_at` of the subkey or primary) is judged at signature creation
  time, so results never go stale by themselves.
* SSH via `ssh-key` (`SshSig`, namespace `git`; ed25519, ECDSA, and RSA
  through the `rsa` crate) → `ssh_signing_keys.fingerprint`.
* E-mail: OpenPGP keys need the committer (tagger) e-mail among the key's
  identities, then (both kinds) the e-mail must be a verified e-mail (or the
  noreply address) of the key owner.
* Reasons: `valid`, `unsigned`, `unknown_key`, `bad_email`,
  `unverified_email`, `no_user`, `unknown_signature_type` (S/MIME and
  others), `malformed_signature`, `invalid`, `gpgverify_error`,
  `not_signing_key`, `expired_key`. `payload` and `verified_at` are filled.
* Cache: `signature_verifications` (one row per signed object SHA, with
  `signer_key`, `signer_id`, lowercased `email`). Invalidation
  (`bgh_core::signatures::{forget_keys, forget_email}`) on GPG key
  add/delete, SSH signing key add/delete, e-mail add/delete/verify and user
  creation; user deletion cascades.
* Wired into every REST verification: commits list/single/compare,
  `git/commits`, `git/tags`, contents API responses, PR commits
  (`bgh_pulls::commits` now reuses `bgh_repos::gitjson::Verification`).
  Batch: one cache query, one query per key kind, one e-mail query; crypto
  runs in `spawn_blocking`.
* GraphQL `Commit.signature`: `email, isValid, payload, signature, state
  (GitSignatureState), wasSignedByGitHub, verifiedAt`, batched by a
  `SignatureLoader`. (Concrete `GpgSignature`/`SshSignature` types and
  `signer` are not modelled.)

### Web-flow key

* Ed25519 OpenPGP key generated on first use in `{data_dir}/signing/`
  (`web-flow.key.asc`, 0600, race-safe via hard link; `web-flow.gpg`
  public half), cached per process. `GET /web-flow.gpg` serves it
  (`git verify-commit` with it imported passes — tested).
* `RepoStore::signer` (set by `RepoStore::from_config`) signs every commit
  created through `commit_changes` (web edits, contents API, template
  generation, suggestions), `merge::commit_tree` (merge, squash, rebase,
  update-branch) and `GitCli::commit_tree`. `GitCli::without_signing()` is
  used by `POST /git/commits` (unsigned, like GitHub). Signatures by the
  web-flow key verify as `valid` with no signer.

### SSH signing keys (`bgh-accounts` `keys.rs`)

`GET|POST /user/ssh_signing_keys`, `GET|DELETE
/user/ssh_signing_keys/{id}`, `GET /users/{username}/ssh_signing_keys`
(shape `ssh-signing-key`: `key, id, title, created_at`; Link pagination;
422 `SshSigningKey`/`key` errors; scopes
`read|write|admin:ssh_signing_key` added to the scope hierarchy, token
scope list and test harness). GPG keys' `emails[].verified` is now computed
live from the owner's verified e-mails.

### required_signatures

* Push: rulesets go through P23's `rule_eval`: `required_signatures` is
  an object-phase rule (`OBJECT_RULES`), evaluated by
  `rule_eval::signature_evals` inside P23's quarantine `ObjectCheck`, so
  GH013 reporting (`- Commits must have verified signatures.` + the
  unverified SHAs), bypass actors, `evaluate` mode and rule suites all come
  from P23's machinery. Classic protection (`Needs.signatures`, admins
  bypass unless `enforce_admins`) adds `signature_check`, which runs after
  any ruleset object check and reports GH006 (`with_classic_signatures` in
  `authorize_push`). Commits checked: `old..new`, or `new --not --all` for
  a new ref.
* API ref updates (`verify_needs`, used by the refs API and others): 422
  `Commits must have verified signatures.`; contents API commits pass
  because they are web-flow signed.
* Merge (`bgh_pulls::protection`): every PR commit (`base..head`) must be
  verified; blocker `Commits must have verified signatures.` (classic and
  ruleset sources, merge box requirements, 405 on merge, auto-merge waits).
  The merge/squash/rebase commits are web-flow signed.

### Web

* `pages/commits/Signature.tsx` (lazy, shared by the commits and pulls
  chunks): `useSignatures` + `SignatureBadge` (Verified / Unverified pill
  with a popover: explanation, signer, GPG key id / SSH fingerprint,
  web-flow key link). Shown in the commits list (the reserved slot), the
  commit page header and the PR Commits tab. Data from the new compact
  endpoint `GET /_bgh/repos/{o}/{r}/commit-signatures?sha=…` (≤ 100 SHAs,
  signed commits only: `verified, reason, key_type, key_id, signer,
  web_flow`).
* Settings → SSH and GPG keys: new "SSH signing keys" section (add with
  title/key, list with fingerprints, delete with confirmation).
* Mock backend: `/user/ssh_signing_keys` CRUD (developer mocks),
  `commit-signatures` + REST `verification` (deterministic per SHA), new
  scopes. Vitest: badge wording, mock endpoints.
* `web/scripts/signatures-smoke.mjs`: Playwright smoke (mock mode, light
  and dark) of the badges, popovers, commit page, PR commits and the
  signing-keys section.
* Bundle: initial JS unchanged except 39 B gzip in the index chunk's lazy
  preload map (the new shared lazy chunk); 144.2 / 150 KB, same as the
  integration base at 0.1 KB resolution.

## Tables / migrations

`3700_signatures.sql`: `ssh_signing_keys` (unique `fingerprint`, index
`(user_id, id)`), `signature_verifications` (PK `sha`; indexes on
`signer_key`, `email`, `signer_id`).

## Shared-code changes (additive)

* Workspace deps: `pgp` 0.21 (no default features), `ssh-key`
  0.7.0-rc.11 (already in the tree via russh; alloc/std/ed25519/p256/
  p384/p521), `libc` (as P23).
* `bgh-core`: `signatures.rs` (invalidation helpers); `auth.rs` two scope
  arms; `testing::ALL_SCOPES` + `admin:ssh_signing_key`;
  `models::db::NewUser::insert` forgets its e-mail.
* `bgh-git`: `signing.rs` (new); `objects::{Commit,Tag}::payload`;
  `RepoStore::signer`, `storage::web_flow_signer`; `GitCli::{signer,
  without_signing, cat_objects_with, pushed_commits}`; commit-tree paths
  sign (test merges excepted).
* `bgh-repos/src/protection.rs` (on top of P23): `Needs::{signatures,
  signatures_ruleset}` (the latter for API updates via `check_update`),
  `SignedRef`, `signature_check`, `with_classic_signatures`,
  `unverified_pushed`, `SIGNATURES_REQUIRED`. `rule_eval.rs`:
  `required_signatures` in `OBJECT_RULES` + `signature_evals`, called next
  to `object_evals` in P23's object check.
* `bgh-pulls/src/protection.rs`: `Facts::unverified_commits`, blocker in
  `evaluate_source` (the P25 hook comment).
* `bgh-graphql`: `model/git.rs` `GitSignature` fields,
  `GitSignatureState`, `SignatureLoader`; `Loaders::signatures`.
* `web/src/api/scopes.ts`: SSH signing key scopes.

## Tests

* `bgh-git` unit: web-flow sign/verify (direct and via the public cert),
  key persistence, SSH sign/verify/namespace, malformed; commit/tag
  payloads.
* `bgh-repos` unit: every reason code of the decision logic.
* `bgh-repos` `tests/it/signatures.rs` (real `gpg` and `ssh-keygen`,
  skipped with a message if missing): `git commit -S` with GPG and SSH →
  `valid`; `unknown_key` before upload and invalidation after;
  `bad_email`, `no_user`, `unverified_email`; cache rows; key deletion;
  signed tags (`git tag -s`); web-flow signed contents commits, unsigned
  git-database commits, `/web-flow.gpg` + `git verify-commit`;
  `commit-signatures` shape; required_signatures on push (classic GH006,
  ruleset GH013, signed GPG+SSH accepted, other branches free, refs API
  422, web edit passes).
* `bgh-pulls` `tests/it/signatures.rs`: ruleset and classic
  required_signatures block unsigned PRs (requirements, 405), signed PRs
  merge and the merge commit verifies; PR commits carry verification.
* `bgh-accounts` `tests/it/signing_keys.rs`: CRUD, shape, validation,
  scopes, pagination, public list.
* `bgh-graphql` `tests/it/signatures.rs`: `Commit.signature`.

## Gate (after merging `origin/claude/sleepy-cray-9jj0t3` at c783bac)

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace` (1165 passed, 14 ignored), web
`typecheck && lint && test (513) && build` (budget OK, 144.8 KB), `api-smoke.sh`
(45/45), `gh-compat.sh` (63/63), Playwright `web/scripts/signatures-smoke.mjs`
(mock, light + dark).

Behaviour changes other tests saw: server-made commits (auto_init,
contents API, merges, `commit_changes` fixtures, templates) now verify as
`valid`; a non-parseable signature is `malformed_signature` (was
`unknown_key`). `refs/pull/N/merge` test merges stay unsigned (they must be
reproducible: bgh-pulls and Actions compute the same SHA). Updated
assertions: `bgh-pulls` `pulls::files_and_commits`, `bgh-repos`
`gitdb::commits`.

## Known gaps

* `vigilant mode` (showing "Unverified" for unsigned commits) is not
  implemented; unsigned commits show no badge.
* S/MIME (x509) signatures are reported as `unknown_signature_type`
  (deferred in the plan).
* GraphQL: no `GpgSignature`/`SshSignature` concrete types or `signer`.
* Webhook/push payload commits (`bgh-notify`) don't carry verification
  (GitHub's push payload doesn't either).
* `GET /git/commits/{sha}` keeps its immutable caching for full SHAs, so a
  browser may keep an older verification of that response after a key
  upload (API clients are unaffected).
* Pushes creating a protected ref from commits already in the repository
  don't re-check those commits (only commits new to the repository).
