# Fleet 2 (phase 5) — foreman2 tracking

Foreman: session_01TnU6QngZQQ3epR16djXC76. Orchestrator: session_01U7ukQiQRpcMMA4n4VDVQR7.

## Bootstrap
| PR | Status |
|---|---|
| #44 (land prior work → main) | clippy red (result_large_err, rust 1.99); fixer session_01HBf9yWEgQS6UYyubf7XC61 pushing to claude/sleepy-cray-9jj0t3 |

## Workers
| Issue | Session | PR | Status |
|---|---|---|---|
| #45 viewport matrix | session_01BURtkf7ua5yBimS7SrhLvt | — | working (waits for #44 before PR) |

## Reviewers
| PR | Session | Status |
|---|---|---|

## QA
| Focus | Session | Status |
|---|---|---|
| issues/PRs/review → repo/code | session_01KK7aYBXALVtbFaqcevgYPC | running |
| dashboard/inbox/search → settings/admin/orgs | session_01DgtACGYUY364jRkpysuP9h | running |

Next QA rotation: actions/projects/wiki/packages; auth & onboarding.

## Standing rules (from orchestrator, 01:45)
Every worker/reviewer/QA prompt gets: "Follow docs/AGENT_WORKFLOW.md 'Operating principles' (context discipline)." Fresh reviewer per round; one issue per worker, archive on merge; track via labels/PR state, get_session only for liveness. Status: comment on #46 "Agent fleet status" every ~2h + message orchestrator. Design questions: Discussions if enabled, else issue T-Docs + S-Blocked.
Self-handoff: when own context > ~350k tokens (get_session on self → external_metadata.context_usage.used_tokens), write full handoff here, start successor foreman with the original foreman2 prompt (orchestrator session_01U7ukQiQRpcMMA4n4VDVQR7 holds it; key points: ~5 workers, reviewer per S-NeedsReview PR, 2 QA rotating areas, label C-Claimed on start, worker/reviewer/QA prompt templates in AGENT_WORKFLOW.md roles) + "Resume from docs/FLEET2.md on branch bgh/foreman2" + these standing rules; send orchestrator the successor id; cancel own send_later triggers; stop.
