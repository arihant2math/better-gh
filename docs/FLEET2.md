# Fleet 2 (phase 5) — foreman2 tracking

Foreman: session_01TnU6QngZQQ3epR16djXC76. Orchestrator: session_01U7ukQiQRpcMMA4n4VDVQR7. Status issue: #46.

## Mode
03:23 FULL SPEED (user via orchestrator): up to ~10 workers + 4 QA, ignore rate-limit warnings; reviewer per S-NeedsReview PR immediately; fixer if S-ChangesRequested not picked up in 15 min; cycles every 10–15 min. On hard rate-limit failures: back off and report to orchestrator. Priority: P-Critical/P-High, then O-QA T-Bug A-Responsive, then rest; spread across areas.

## History
#44 merged by user into main (9ef62fb) ~02:33; claude/sleepy-cray-9jj0t3 retired. Old QA sessions filed #47–#78 (archived).

## Workers
| Issue | Session | PR | Status |
|---|---|---|---|
| #45 viewport matrix | session_01BURtkf7ua5yBimS7SrhLvt | — | working |
| main Rust tests fixer | session_01E96iLFFFxFuaZXgU4hayJ6 | #87 | in review — reviewer creation DENIED by classifier (self-approval); needs user |
| #73 modal focus (+#63?) | session_013HG6yn6H9NYMAmM4iHzJ51 | — | working |
| #60+#66 topbar breadcrumbs | session_0142iYEJAmNc4sKsZMCnhhYQ | #99 | in review (session_01ES6c9RyJjKa6meDUUJbvV3) |
| #41 PR diff commit range | session_01QkzGhN3PPqNiRUBQ9YnH5C | #83 | in review (session_01PAC94sDt4XhXqoRgzXLhpd) |
| #17 backup/admin CLI | session_01CBqx1eAjHqzrzArwDfoRUs | — | working (multi-PR) |
| #67 list filter bar | session_01DqkE15WspmiTrQ5N81q11Y | #94 | in review (session_01JHfbu2yrPNBqxLXUmsXc4z) |
| #74+#75 admin/org settings | session_013CH7MYMktXWDWWDncSZdRV | #93 | in review (session_0138qu5VQL9FTrSUSvCkrMCm) |
| #61+#47 inbox pane | session_01JjNrzyZ2B5SRuDho11YYeU | #88 | in review (session_01By5xV53cQckCS8sQpYfvSC) |
| #49+#50 dashboard long names | session_01MnuxWsCvwWYqJvjYw2JrUw | #82 | in review (session_01VvN6wT3FDMVjv4NGHkQsDB) |
| #42 bundle budget | session_01EqGtoarvzLipstEVahMUUk | — | working |
| #52+#51+#53 command palette | session_013fSYvtqc39XjY3386rsMor | — | working |
| #68+#69+#70 PR timeline | session_01QpEnrtN98FRjCg8vGgLuZj | — | working |
| #76 slash branch names | session_01ULxkjyqgpub6jn2a4ZR4oB | — | working |
| #48+#59+#55 search polish | session_012zz7GcEfQELQFVfMLhtL37 | — | working |
| CI path filters (orchestrator-started; issue #84) | session_01Em5rrQBMpzEpHHvsmaNzqG | #86 | in review (session_01X8kuDPNCg7kzgRAaPnUFNY) |
| CI caching analysis (read-only; files issue "CI caching analysis and recommendations") | session_011amGqNaCwcHg8XeTafyqfq | — | working; archive when issue filed |

CI rule: no other ci.yml work until the path-filter PR merges; then queue the caching issue's checklist items as worker tasks.

Queue: #63 (check after #73), #65+#64 diff toolbar/header (after #83 merges), #22 web perf (after #42), #23 mobile/a11y, #54/#57/#58/#62 small polish, #56 inbox polish (after #88), #71/#72 PR polish, #77/#78 code polish, then P-Medium features (#1–#31; not ci.yml).

## Reviewers
| PR | Session | Status |
|---|---|---|
| #81 branch cleanup (changes → fixer session) | fixer session_019mR7rcWSJ6vanJSsU95Gdw (pipefail); re-review 3 session_012zodeH8NboDNrmkJU12FC4 | foreman merges with MERGE then runs branch-cleanup.yml min_age_hours=12 |

## QA
| Focus (pass 1 → pass 2) | Session | Status |
|---|---|---|
| repo/code browsing → wiki | session_01KfNriN3BgXNqpcuz9vki6a | running |
| settings/admin/orgs → Actions UI | session_012WCiQPEEZjiJJF6N8PiNvR | running |
| projects/packages/releases → issue detail | session_01V2UQXBRnk8zvBZenyJNhPo | running |
| auth/onboarding → API/gh compat | session_01CNLoLz9eJUdS9dDdekDadC | running |

Next QA rotation: issues & PRs & review; dashboard/inbox/search/command palette.

## Standing rules
Every worker/reviewer/QA prompt: "Follow docs/AGENT_WORKFLOW.md 'Operating principles' (context discipline)." Fresh reviewer per round; one issue (or tightly coupled pair) per worker, archive on merge; track via labels/PR state, get_session only for liveness. Status: comment on #46 every ~2h + message orchestrator. Design questions: Discussions if enabled, else issue T-Docs + S-Blocked.
Branches: reviewers squash-merge, branch-cleanup workflow deletes heads (once #81 lands); workers delete abandoned branches; on handoff copy FLEET2.md into a docs PR and delete bgh/foreman2 (or leave for successor).
Self-handoff: when own context > ~350k tokens, write full handoff here, start successor foreman with the original foreman2 prompt (orchestrator holds it) + "Resume from docs/FLEET2.md on branch bgh/foreman2" + these rules; send orchestrator the id; cancel own send_later triggers; stop.
Orchestrator is coordination-only (03:31): all code/doc changes go through worker/fixer sessions.
