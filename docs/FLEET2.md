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
| main Rust tests fixer | session_01E96iLFFFxFuaZXgU4hayJ6 | (agent/fix-main-tests) | working |
| #73 modal focus (+#63?) | session_013HG6yn6H9NYMAmM4iHzJ51 | — | working |
| #60+#66 topbar breadcrumbs | session_0142iYEJAmNc4sKsZMCnhhYQ | — | working |
| #41 PR diff commit range | session_01QkzGhN3PPqNiRUBQ9YnH5C | — | working |
| #17 backup/admin CLI | session_01CBqx1eAjHqzrzArwDfoRUs | — | working (multi-PR) |
| #67 list filter bar | session_01DqkE15WspmiTrQ5N81q11Y | — | working |
| #74+#75 admin/org settings | session_013CH7MYMktXWDWWDncSZdRV | — | working |
| #61+#47 inbox pane | session_01JjNrzyZ2B5SRuDho11YYeU | — | working |
| #49+#50 dashboard long names | session_01MnuxWsCvwWYqJvjYw2JrUw | — | working |

Queue: #42 bundle budget (P-High perf), #63 (check after #73), #22 web perf, #23 mobile/a11y, #65+#64 diff toolbar/header, #52+#51+#53 command palette, #68+#69 PR timeline, #76 branch names with /, #48 search clipping, then P-Medium features (#1–#31).

## Reviewers
| PR | Session | Status |
|---|---|---|
| #81 branch cleanup (orchestrator authored; further changes → fixer session, not orchestrator) | session_01JeQnHGDV7zqsAiRgxDE9pU (re-review 2) | running; foreman merges with MERGE then runs branch-cleanup.yml min_age_hours=12 |

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
