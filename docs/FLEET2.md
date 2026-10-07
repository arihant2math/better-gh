# Fleet 2 (phase 5) — foreman2 tracking

Foreman: session_01TnU6QngZQQ3epR16djXC76. Orchestrator: session_01U7ukQiQRpcMMA4n4VDVQR7. Status issue: #46.

## Mode
04:10 WIND DOWN (user via orchestrator; supersedes FULL SPEED):
1. No new workers/QA/reviewers for feature/polish/QA work; no new claims; queue frozen. In-flight workers finish their current claimed issue/PR (review + fixer rounds on already-open PRs OK). Archive each session when its PR merges. QA told (04:10) to finish current pass, stop filing, report.
2. CI track stays FULL SPEED: main red Rust tests (fixer session_01E96iLFFFxFuaZXgU4hayJ6 root-cause PR), #86 path filters, #81 branch cleanup (merge method "merge", then branch-cleanup.yml min_age_hours=12). Fixers/reviewers allowed; classifier blocks → orchestrator.
3. After CI work merged + main green: "CI caching analysis and recommendations" issue checklist, one worker PR at a time, merged before next; close issue at end.
4. When all done: archive all sessions (incl. QA), final #46 status, delete bgh/foreman2, report to orchestrator.
(Previous: 03:23 FULL SPEED.)

## History
04:15–04:17 user merged #81 (merge), #82, #83. 04:19 branch-cleanup.yml dispatched on main (min_age_hours=12). Archived: reviewers #81/#82/#83, workers #82/#83, all 4 QA (filed #89–#140, 47 issues incl. P-High security #96, #138). Classifier denied archiving #81 fixer session_019mR7rcWSJ6vanJSsU95Gdw → left to user/orchestrator. #147 (#42 bundle) S-NeedsReview, no reviewer: asked orchestrator whether wind-down allows one (holding).
#44 merged by user into main (9ef62fb) ~02:33; claude/sleepy-cray-9jj0t3 retired. Old QA sessions filed #47–#78 (archived).

## Workers
| Issue | Session | PR | Status |
|---|---|---|---|
| #45 viewport matrix | session_01BURtkf7ua5yBimS7SrhLvt | — | working |
| main Rust tests fixer | session_01E96iLFFFxFuaZXgU4hayJ6 | #87 merged (diagnostics) | 04:18 gate test running; PR from agent/fix-main-tests-2 next |
| #73 modal focus (+#63?) | session_013HG6yn6H9NYMAmM4iHzJ51 | — | working |
| #60+#66 topbar breadcrumbs | session_0142iYEJAmNc4sKsZMCnhhYQ | #99 | in review (session_01ES6c9RyJjKa6meDUUJbvV3) |
| #41 PR diff commit range | session_01QkzGhN3PPqNiRUBQ9YnH5C | #83 | MERGED, archived |
| #17 backup/admin CLI | session_01CBqx1eAjHqzrzArwDfoRUs | — | working (multi-PR) |
| #67 list filter bar | session_01DqkE15WspmiTrQ5N81q11Y | #94 | in review (session_01JHfbu2yrPNBqxLXUmsXc4z) |
| #74+#75 admin/org settings | session_013CH7MYMktXWDWWDncSZdRV | #93 | in review (session_0138qu5VQL9FTrSUSvCkrMCm) |
| #61+#47 inbox pane | session_01JjNrzyZ2B5SRuDho11YYeU | #88 | in review (session_01By5xV53cQckCS8sQpYfvSC) |
| #49+#50 dashboard long names | session_01MnuxWsCvwWYqJvjYw2JrUw | #82 | MERGED, archived |
| #42 bundle budget | session_01EqGtoarvzLipstEVahMUUk | #147 | S-NeedsReview; reviewer held pending orchestrator |
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
| #120 backup/restore (part of #17) | session_01WvB8xk1fePvCrby2JQxZN4 | running |
| #137 search polish | session_016HDgJFk5Ltk7TRyVZVuVLw | running |
| #141 command palette | session_01HpUhagrFazohxYLVU8rMnw | running |
| #143 PR timeline | session_01NeG1iWFeXURfNUS5c5jfrW | running |
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

Classifier: creating reviewers for fixer-authored CI PRs is denied for foreman (self-approval); ask orchestrator to start those reviewers (it will). 04:05: #81/#88/#93/#94/#99 approved; branches updated from main after #87.
