# Fleet 2 (phase 5) — foreman2 tracking

Foreman: session_011BS1twjVWtvUAmNtnSQ52s (foreman3, since 08:12; foreman2 was session_01TnU6QngZQQ3epR16djXC76). Orchestrator: session_01U7ukQiQRpcMMA4n4VDVQR7. Status issue: #46.

## HANDOFF (08:12, foreman2 session_01TnU6QngZQQ3epR16djXC76 → successor; context >700k)

### Current direction (from the user via orchestrator session_01U7ukQiQRpcMMA4n4VDVQR7)
- WIND DOWN starts about 08:52; the orchestrator will send details then. Until then, PRs from orchestrator-dispatched workers still get reviewed.
- Every PR needing review gets a FRESH reviewer session, one per round. Reviewers must NOT merge: they approve, set S-Approved, update the branch from main, and send the foreman the PR#, approved head SHA and CI status. The foreman forwards each ready PR to the orchestrator, which merges after checking the APPROVE and a green `CI result` on the exact head. Never merge before a verdict is posted (#274 incident). The classifier blocked adopting a relayed "agents merge themselves" policy, so don't adopt it.
- S-ChangesRequested: message the author. The author's session id is in the claude.ai link in the PR body. Start a fresh reviewer when the author reports the fix is pushed.
- Archive reviewer sessions as soon as they report. The orchestrator archives workers after merge.
- Ignore bgh-audit sessions and O-Audit issues.
- #150 belongs to the user's own session; don't touch it. Its macOS blocker is fixed on main via #273.
- Security PRs get adversarial security reviewers that actually run the attacks.

### Open PRs at 08:12
- #304 (search index predicates, #277+#278). Labelled S-Approved, but reviewer session_01RxVGUE3TRBk6FJNA5bpijT hasn't reported yet. When it does, forward the PR# and approved SHA to the orchestrator. If it has gone silent, check that session.
- #316 (merge queue service, #288). Reviewer session_012M6KVSN9CuUn2yHTB1ZWtY is running. Lead is session_01Kq9BfEn4RqooYm2643Ah6e (#2 merge queue; it opens several sub-PRs).
- #315 (merge queue GraphQL + auto-merge, #290). ChangesRequested with 4 findings: auto-merge must wait then enqueue, disabling auto-merge must dequeue, private-repo NOT_FOUND leak, N+1. The lead is fixing them and will ping; then start a fresh reviewer.
- #314 (actions streaming, #218+#223). ChangesRequested: over-cap upload returns 500 instead of 413 (web.rs:258 double-wrap); a test is needed. Author session_01AoPFjmaiac4HttSRmN6gJB will ping; then a quick re-review.
- #312 (SECURITY #138, signup emails start unverified). Now ChangesRequested; the security reviewer session_0194YYMCkbdiMibSpxPFzgZG may not have reported to me yet. Read the review, message author session_015sx68MN6HVyxjdYqcwsVzi, then a fresh security reviewer.
- #309 (SECURITY #275, GraphQL cost/node limit). ChangesRequested: 2 bypasses (fragments nested >8 levels count as 0; nodes(ids:) unbounded). Author session_01PoLx449ncVkpwQdp2GCpmb will ping; then a fresh adversarial reviewer.
- #294 (#78 empty repo states). ChangesRequested: merge conflict in CommitsPage.tsx plus untested compare/new-PR/releases/file-finder. Author session_01WXLPuawpcP7jUeYDATVYdD.
- #150: the user's own.

### Pending PRs that will arrive
- #317 (P-High SECURITY: POST /_bgh/auth/signup skips settings::check_signup). The orchestrator dispatched the fixer "bgh fix #317 signup policy bypass", which will message the foreman with its PR. Assign an adversarial security reviewer.
- More #2 merge-queue sub-PRs from lead session_01Kq9BfEn4RqooYm2643Ah6e.
- Other orchestrator-dispatched workers whose PRs haven't appeared yet: #245/#241, #253 (done as #296), and others.

### #85 CI caching track (one worker PR at a time, ci.yml)
- Item 1 (#152) is merged. Item 2 (#311, test cache: main-only save-if + cache-on-failure + --no-run) appears MERGED (no longer open). Verify, then archive worker session_015QY2gRLnM8MU2bjXzNzHUL and reviewer session_01CnGM7nUSt7UMu6WKTSqu83 if they're still alive.
- NEXT: start the item 3 worker: fold compat into the tests job, or `needs: rust-test` on the same target, or give it its own shared-key with main-only save. 'Part of #85', labels T-Perf A-Ops, branch agent/85-<slug>, no merge. Then items 4–10 in issue order, each reviewed and merged before the next. Close #85 when all are done. Respect the wind-down details the orchestrator sends at about 08:52 (the user earlier said the CI track continues).

### Merged this session (highlights)
#81, #82, #83, #86, #88, #93, #94, #99, #120, #137, #141, #143, #147, #149, #151 (main tests fix), #152, #273, #274 (merged before review, which was the bug), #292, #293, #295 (#96 security fix), #296, #297, #298, #299, #300, #301, #302, #306, #311.
Follow-up issues filed: #153 (flake, fixed by #293), #305, #307, #310, #313, #317.

### Finally (when everything is done)
Archive all sessions; post a final status on #46; copy FLEET2.md into a docs PR if useful, else just delete bgh/foreman2; report to the orchestrator; cancel triggers.

## Mode
04:10 WIND DOWN (user via orchestrator; supersedes FULL SPEED):
1. No new workers/QA/reviewers for feature/polish/QA work; no new claims; queue frozen. In-flight workers finish their current claimed issue/PR (review + fixer rounds on already-open PRs OK). Archive each session when its PR merges. QA told (04:10) to finish current pass, stop filing, report.
2. CI track stays FULL SPEED: main red Rust tests (fixer session_01E96iLFFFxFuaZXgU4hayJ6 root-cause PR), #86 path filters, #81 branch cleanup (merge method "merge", then branch-cleanup.yml min_age_hours=12). Fixers/reviewers allowed; classifier blocks → orchestrator.
3. After CI work merged + main green: "CI caching analysis and recommendations" issue checklist, one worker PR at a time, merged before next; close issue at end.
4. When all done: archive all sessions (incl. QA), final #46 status, delete bgh/foreman2, report to orchestrator.
(Previous: 03:23 FULL SPEED.)

## History
08:30: new #322 (merge_group trigger + webhook, #289; STACKED on #316, merges only after #316 and a retarget) → adversarial reviewer session_01CW7onm3wAGmdxS1T4VGB4G. The lead is fixing #315 and #316.
08:30: orchestrator MERGED #294 (1c37832) and archived its worker.
08:29: #294 READY (APPROVE on 34144b6; head 79dbe28 is a main merge; CI result green). Forwarded to the orchestrator; r2 reviewer archived.
08:28: filed #312 follow-ups at the orchestrator's request (backlog, no workers): #324 (P-High SAML JIT links by login), #325 (unverified email squat DoS), #326 (backfill for pre-fix verified=true).
08:21: #316 REQUEST_CHANGES (main conflict; dequeue/head-push race on merge_prefix; double-merge on retry after CAS; PostReceive enqueued outside the tx). Lead session_01Kq9BfEn4RqooYm2643Ah6e told; reviewer archived. #309 fixed (d1cafe7) → adversarial security r2 reviewer session_01Mgvsx8ABCZHLFeU4x5VMdn. Alias-cap follow-up is #321.
08:15: orchestrator MERGED #304 (0c5db5f) and archived its worker. I confirmed the takeover, so the orchestrator archives foreman2.
08:15: #314 fixed (413 propagation + test, head ab338fc) → r2 reviewer (sonnet) session_0186Ho5AEnrVzi1soBuCWL7e.
08:15: #304 READY (APPROVE at 4fb8add; head 31df601 is a main merge only; CI result green). Sent to the orchestrator; reviewer archived. Foreman2 has stopped.
08:14 (foreman3 session_011BS1twjVWtvUAmNtnSQ52s): took over. #311 (#85 item 2) MERGED; its worker session_015QY2gRLnM8MU2bjXzNzHUL and reviewer session_01CnGM7nUSt7UMu6WKTSqu83 are archived. #85 item 3 worker: session_01Pjosi2axSP1tmSuN6os4Bk (agent/85-compat-cache). #294 r2 reviewer (sonnet): session_01Ko9xJJssjZh63Uc3Q3Hk1o. #312: ChangesRequested (unverified email can be published as the public email; SSO reclaim doesn't clear it); author session_015sx68MN6HVyxjdYqcwsVzi was told; reviewer session_0194YYMCkbdiMibSpxPFzgZG archived. #304 APPROVE at 4fb8add, head now 31df601 with Rust tests running; reviewer session_01RxVGUE3TRBk6FJNA5bpijT will report. #316 reviewer session_012M6KVSN9CuUn2yHTB1ZWtY is running. Waiting on authors: #315 (lead session_01Kq9BfEn4RqooYm2643Ah6e), #314 (session_01AoPFjmaiac4HttSRmN6gJB), #309 (session_01PoLx449ncVkpwQdp2GCpmb). All of them, plus the orchestrator, were told the new foreman id.
07:54: The orchestrator merged #273, #293, #296, #297, #299, #300 and #301. Approved and awaiting the orchestrator's merge: #302, #306; #298 is in merge-resolution re-review (session_01VZSEq8qPWeiU71aD2p1VGL). In review: #304 session_01RxVGUE3TRBk6FJNA5bpijT, #309 (GraphQL cost) session_01SSVSaQxfbb5sxsSR5H8LaA, #311 (#85 item 2) session_01CnGM7nUSt7UMu6WKTSqu83, #312 (#138 SECURITY) session_0194YYMCkbdiMibSpxPFzgZG, #314 (actions streaming) session_013KhaT4PGLFLxH7cs6ic9PB, #315 (merge queue GraphQL) session_012CNkiefTrCu5WXhacob7gR. ChangesRequested: #294 (author session_01WXLPuawpcP7jUeYDATVYdD), #150 (user; macOS fix now on main). #85: items 1 (#152) and 2 (#311 in review) done; items 3–10 remain, one at a time. Follow-up issues: #305, #307, #310, #313.
07:37: The orchestrator merged #295 (0bc3a4c; #96 closed). Approved and sent to the orchestrator (it merges after verifying the SHA and CI): #273, #293, #296, #297, #298, #301. In review: #299 r2 session_01E8rHgiMqz3awsoFouHU87D, #300 session_01QebwZ2gS7zXodBiZL4hZ5M, #302 session_01G9WVEwyWcMDRTJ1FkXrzSt, #304 session_01RxVGUE3TRBk6FJNA5bpijT, #306 session_01JCY6ma2KCCXZJiae1nufHX. ChangesRequested: #294 (author session_01WXLPuawpcP7jUeYDATVYdD), #150 (user's own). Follow-up issues filed: #305 (return_to nits), #307 (PR form nits). #85 item 2 starts after #273 merges.
07:20: In review: #295 (#96 follow-up, P-Critical) security reviewer session_01232aT83FrzPsUN9C4arAGU; #273 r2 session_01758RbXZ9ezPJBhbSsTMDQw; #293 (#153) session_015qqjZAqCNoDHw4g42aAPAA; #294 (#78) session_01QCjSABrM3ftY5BB9K32DRi; #299 (merge queue UI) session_014rVqBVc4Bo6CGde97exu64; #298 (#11 PR form) session_01DnQZkWUTJe3FR8rGZFJoAq; #297 (pager) session_01LEV78q99TdARyNehRgShvU; #296 (error boundaries) session_01KqNNiH7CAXyxpdimRePC82. #292 was merged by the orchestrator (1e4c876). Merge flow: reviewers don't merge; I send the orchestrator the PR# plus approved head SHA and CI status, and it merges. (The classifier blocked adopting the relayed 'agents merge' policy.)
06:58 SECURITY: #274 was MERGED by the user at 06:47:32, before its REQUEST_CHANGES review (06:48). main has the /..//evil.com open-redirect regression and #96 auto-closed. Told fixer session_01DgJJNCbJJ5a4x5bB3VAP6R to open an urgent follow-up PR from main (agent/96-open-redirect-followup, P-Critical) and alerted the orchestrator (the user may revert). When it's up, start a fresh adversarial security reviewer at once. Open: #273 (ChangesRequested, author fixing macOS), #150.
06:55 USER (via orchestrator): 12 more workers dispatched by the orchestrator. Each opens a PR, drives it to green, then messages me → I start a reviewer and add it to the ready batches. Fixes: #138, #275, #277+#278, #259+#234, #268+#269, #253, #242, #222 and #229 (2 PRs), #245 and #241 (2 PRs), #218+#223. Features: #11 (PR creation form) and #2 (merge queue; the lead opens several sub-issue PRs over time). Don't claim those issues myself. Audits are done (O-Audit #154–#285). #274 (#96) got REQUEST_CHANGES: still exploitable via /..//evil.com, fix sent to session_01DgJJNCbJJ5a4x5bB3VAP6R. #273 got REQUEST_CHANGES: macOS pid_alive parse error, fix sent to session_01HYiEYiEvuMEGsVXo5KMkSf.
06:42: #152 (#85 item 1) and #120 MERGED; their sessions are archived. Open: #273 (cross-platform fmt/clippy, plus the smart_http fix for #150) → reviewer session_015VbCpaKBJBUWU92o5XtB4e; #274 (#96 open redirect) → security reviewer session_01UCQJzBFwmnCEGoFAH1w14a; #150 (user's own, ChangesRequested). #153 flake fixer session_01Fb2HTWPQwtRmKswDc1jmpt, approved by the orchestrator. #78 fixer PR not up yet. #85 item 2 starts after #273 merges.
06:24: #149 MERGED (re-run green), worker archived. Main CI green (630d18b #86, 94defb8 #99). Open PRs: #152 (approved; Rust tests red on updated head, worker checking whether it's the #153 flake), #120 (round-3 reviewer), #150 (user's own, ChangesRequested macOS). Waiting on: cross-platform PR (session_01HYiEYiEvuMEGsVXo5KMkSf) and the #96/#78 fixer PRs.
06:15 USER: session_01HYiEYiEvuMEGsVXo5KMkSf adds fmt+clippy jobs for linux-x64/linux-arm64/macOS to ci.yml (wired into CI result) and fixes the smart_http.rs read_write call (unblocks #150). It messages me when its PR is green → start a reviewer. ci.yml ORDER: #152 (#85 item 1, approved) merges first, then the cross-platform PR, then #85 item 2 etc. Each ci.yml PR merges origin/main before review. #149's red is flaky test #153 (camo); re-ran the failed jobs once (run 37577630599). #120 fixed (fbea139) → round-3 reviewer session_01XWw7rtrXA7xDaXYbkYXwZv.
06:13 USER (via orchestrator): exceptions to the wind-down. Orchestrator dispatched fixers for #96 (open redirect, session_01DgJJNCbJJ5a4x5bB3VAP6R) and #78 (empty repo states, session_01WXLPuawpcP7jUeYDATVYdD). They'll message me their PR numbers, and I treat those PRs normally: reviewer, report ready, archive after merge. There are also 23 bgh-audit sessions (issue-filing only, O-Audit); do NOT claim, assign or track them, the orchestrator owns them. #152 (#85 item 1) is approved and awaiting the user's merge.
06:06: #99, #94, #143 merged; workers archived. Updated heads: #149 (f60d4fe) and #120 (185b7d0) are RED on Rust (tests); their authors (session_01ULxkjyqgpub6jn2a4ZR4oB, session_01CBqx1eAjHqzrzArwDfoRUs) are diagnosing whether it's their code or main. #150 is CI-green but still ChangesRequested (macOS). #152 (#85 item 1) reviewer session_017Xzj9s5zTNRKf7jdsAgqZd. Main CI on 630d18b is still in progress (later main pushes were cancelled by concurrency; 94defb8 #99 is pending).
05:48: #85 item 1 (skip Docker on PRs) worker session_01HeHGLR9MGkSJhjJpvmNnbh started (branch agent/85-skip-docker-on-prs, 'Part of #85'). Main CI on 630d18b is still queued; don't merge item 1 until main is green. The updated PRs' CI is running.
05:43: #86 MERGED (630d18b); #86 worker and the #85 analysis session archived. Per user direction, all open PRs were updated from main via the update-branch API, with no conflicts: #150, #149, #143, #120, #99, #94. Waiting for their CI, then I send the refreshed ready list to the orchestrator. Main CI on 630d18b is pending. CI caching = issue #85 (10 checklist items). Start ONE worker on item 1 (skip Docker on PRs unless Dockerfile/Cargo.lock/workflows change; note #86's changes job already exists) once main is green. Sequential: reviewer, user merge, then the next item.
05:31: Actions restored ~05:06. Merged: #151 (fb0a756, main tests fix), #137, #93, #141. Archived: #151 fixer and workers for #137/#93/#141. Approved, awaiting user: #86 (author merging main), #99, #94, #149, #120. #143 relabelled NeedsReview → round-2 reviewer session_011hTmGhYWRmzFoPuhoVFXKu. #150 waits on its owner. Remaining workers: #86 session_01Em5rrQBMpzEpHHvsmaNzqG, #99 session_0142iYEJAmNc4sKsZMCnhhYQ, #94 session_01DqkE15WspmiTrQ5N81q11Y, #149 session_01ULxkjyqgpub6jn2a4ZR4oB, #120 session_01CBqx1eAjHqzrzArwDfoRUs, #143 session_01QpEnrtN98FRjCg8vGgLuZj.
05:00: all PRs are reviewed. Approved, awaiting user: #137 #86 #99 #94 #93 #141 #143 #149 #120, plus #151 (needs a real CI run). #143 carries a cherry-pick of #151 (9de65b4). #86's author also cherry-picked #151 (3398b28); asked it to revert. #150 waits on its owner (macOS). Reviewers all archived. Cycles every 30 min while Actions is blocked.
04:56 BLOCKER: GitHub Actions dead since ~04:55Z: all jobs fail in ~3s with no runner (runs 37573736650 #151, 37573790815 #143); older runs stuck in_progress. Account-level (billing/minutes?), so it's the user's to fix; escalated to the orchestrator. No reruns. #151 APPROVED (reviewer archived) but Rust tests unverified on the runner. #143 branch contains a cherry-pick of #151.
04:54 cycle: no new merges. Ready awaiting user: #137 #86 #99 #94 #93 #141. #150 REQUEST_CHANGES (macOS tokio pipe, smart_http.rs:783), author not mine → routed to orchestrator. #120 fixes pushed → round-2 reviewer session_012V8qxuzzJxWQYo7uEnC1pV. #151 Rust (tests) still running after 27m (all other checks green); reviewer nudged to post its verdict. #143 and #149 reviewers working.
04:36 cycle: user merged #88 (04:20) and #147; branch-cleanup dispatch run 37570815639 succeeded. Ready-to-merge (sent to orchestrator): #137, #86, #99, #94, #93. #141 re-labelled NeedsReview after an author push → re-review session_011nypHRxkJGcQF3YWNKHsmp. New PRs: #149 (#76 slash refs, worker session_01ULxkjyqgpub6jn2a4ZR4oB) → reviewer session_01CTK6PEXpcMSvnwXXEpTNmd; #150 (release binaries, from session_01RD8fYn8p8XXbH4NZ9TdqbY, not mine) → reviewer session_012D9ADMqXzktnS8haQtJqDT. #120 REQUEST_CHANGES (restore validate-first, 0700/0600 perms, refs-before-objects) → author session_01CBqx1eAjHqzrzArwDfoRUs notified. #151 reviewer session_01VoEAMaVcskHH3zXbM5dyuA; #143 reviewer session_01NeG1iWFeXURfNUS5c5jfrW. Archived: reviewers #86/#88/#93/#94/#99/#120/#137/#141/#147, workers #42/#61. Still to check: workers #45 (session_01BURtkf7ua5yBimS7SrhLvt), #73 (session_013HG6yn6H9NYMAmM4iHzJ51; #73 may have landed as #80).
04:25 USER OVERRIDE: reviewers for ALL PRs needing review (workers/QA stay frozen). Reviewers must NOT merge (classifier 'Merge Without Review'); they approve, set S-Approved, update the branch, report 'ready to merge' to the foreman, and the foreman batches them to the orchestrator for the user to merge. #147 approved (reviewer archived), ready to merge. #151 = main-tests root-cause PR; reviewer session_01VoEAMaVcskHH3zXbM5dyuA.
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
| #42 bundle budget | session_01EqGtoarvzLipstEVahMUUk | #147 | in review (session_018z5wVqspWwUoapxgfs2FHb, started by orchestrator) |
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
