# Integration log (phase 4)

Kept by the integrator session on `bgh/integrator` only. One entry per
batch landed on `claude/sleepy-cray-9jj0t3` (see `docs/WORKER_GUIDE.md`,
"Integration queue").

| Time (UTC) | Packages | Result | Pushed commit | Tests |
|---|---|---|---|---|
| 2026-10-05 16:00 | — | queue opened at `fc37a0d`; no branch marked ready | — | — |
| 2026-10-05 16:20 | P18 metadata-import, P24 rulesets-ui | green (P24: additive conflicts in routes.ts, mock/code.ts, OrgSettingsLayout.tsx resolved; Rulesets nav kept in its group) | `ce257f2` | rust 967 passed / 12 ignored; web 407 passed |
| 2026-10-05 16:45 | P16 reusable-workflows, P34 api-compat, P07 access-policy, P31 insights | green, clean merges | `d7e5e85` | rust 1020 passed / 12 ignored; web 422 passed |
| 2026-10-05 16:55 | P22 pulls-rest, P33 repo-metadata, P05 invitations | green (P33: additive conflicts in bgh-repos create.rs/jobs.rs and mock/extra/index.ts; P05: mock index). P14 ldap-auth bounced (non-additive conflict with P07 in bgh-core auth.rs, settings.rs, LoginPage.tsx); worker fixed at e5cb309 | `58386b7` | rust 1041 passed / 12 ignored; web 432 passed |
| 2026-10-05 17:15 | P14 ldap-auth (re-queued at e5cb309) | green (additive: accounts tests/it mod list; Cargo.lock regenerated) | `8448855` | rust 1051 passed / 13 ignored; web 434 passed |
| 2026-10-05 17:25 | P36 account-security | bounced: non-additive conflicts with P14/P07 (bgh-core auth.rs, settings.rs; bgh-accounts lib.rs/Cargo.toml; LoginPage, admin settings; Cargo.lock x509 stack versions). Worker session_017gT9Wi notified. Also: disk hit 99%, ran cargo clean (29 GiB) | — | — |
| 2026-10-05 17:40 | P35 markdown, P29 runners | green, clean merges (lazy chunks 299→409, largest 138 KB, within budget) | `7ac1b95` | rust 1066 passed / 13 ignored; web 459 passed |
| 2026-10-05 18:00 | P27 actions-cache, P23 rulesets, P47 fine-grained-pats | green (additive: bgh-actions web_router, mock index, OrgSettings.module.css). P36 dropped: security::token_expiry_reminders_and_header failed under load (FOR UPDATE SKIP LOCKED vs last_used_at touch); worker fixed at 9ae9fbc. First rerun died on disk full; pruned stale target/debug/deps | `aed4997` | rust 1114 passed / 13 ignored; web 472 passed |
| 2026-10-05 18:25 | P36 account-security (9ae9fbc), P37 diff-viewer, P38 review-workflow, P41 issue-types, P42 comment-moderation | green after integration fixes: org Issue types shortcut g y→g e (clash with P36); merged P38+P42 duplicate .linkButton in Review.module.css (first gate: CSS syntax error from git splice); fmt of merged tests/it mod list. Bounced: P20 (rulesets.rs vs P23), P46 (bgh-core auth/perms vs P47). FYI to P38: P37 DiffSource ignores commit range | `c783bac` | rust 1150 passed / 14 ignored; web 509 passed |
