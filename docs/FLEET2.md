# Fleet 2 (phase 5) — foreman2 tracking

Foreman: session_01TnU6QngZQQ3epR16djXC76. Orchestrator: session_01U7ukQiQRpcMMA4n4VDVQR7.

## Bootstrap
#44 merged by user into main (9ef62fb) at ~02:33. claude/sleepy-cray-9jj0t3 retired.

## Pacing
02:34: rate_limit_info allowed_warning (seven_day) seen → cap 3 workers (incl. fixers) + 1 QA. Re-check on each cycle.
Worker queue (next, by priority): #60 topbar breadcrumbs (note #66), #41 PR diff commit range, #17 backup/admin CLI, #42 bundle budget, #63 review popover focus (may be fixed by #73).

## Workers
| Issue | Session | PR | Status |
|---|---|---|---|
| #45 viewport matrix | session_01BURtkf7ua5yBimS7SrhLvt | — | working |
| main Rust tests fixer | session_01E96iLFFFxFuaZXgU4hayJ6 | (agent/fix-main-tests) | working |
| #73 modal focus | session_013HG6yn6H9NYMAmM4iHzJ51 | — | working |

## Reviewers
| PR | Session | Status |
|---|---|---|

## QA
| Focus | Session | Status |
|---|---|---|
| issues/PRs/review → repo/code | session_01KK7aYBXALVtbFaqcevgYPC | running |
| dashboard/inbox/search | session_01DgtACGYUY364jRkpysuP9h | told to wrap up (pacing); archive when idle |

Next QA rotation: actions/projects/wiki/packages; auth & onboarding.

## Standing rules (from orchestrator, 01:45)
Every worker/reviewer/QA prompt gets: "Follow docs/AGENT_WORKFLOW.md 'Operating principles' (context discipline)." Fresh reviewer per round; one issue per worker, archive on merge; track via labels/PR state, get_session only for liveness. Status: comment on #46 "Agent fleet status" every ~2h + message orchestrator. Design questions: Discussions if enabled, else issue T-Docs + S-Blocked.
Self-handoff: when own context > ~350k tokens (get_session on self → external_metadata.context_usage.used_tokens), write full handoff here, start successor foreman with the original foreman2 prompt (orchestrator session_01U7ukQiQRpcMMA4n4VDVQR7 holds it; key points: ~5 workers, reviewer per S-NeedsReview PR, 2 QA rotating areas, label C-Claimed on start, worker/reviewer/QA prompt templates in AGENT_WORKFLOW.md roles) + "Resume from docs/FLEET2.md on branch bgh/foreman2" + these standing rules; send orchestrator the successor id; cancel own send_later triggers; stop.
