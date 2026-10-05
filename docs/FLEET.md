# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 18:22 UTC (integration head 6209ca1).

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1–P19 P21–P24 P26 P27 P29 P31–P38 P41 P42 P47 P51 P61 P65 (41) | (archived) | done | yes, head 6209ca1 |
| P20 | session_01YKLKPRAJnD9iwrqFqwFppG | ready again after bounce (rulesets.rs vs P23) | – |
| P46 | session_01Fdy1y7wDZ4h259dMZKmFUn | ready again after bounce (auth/perms vs P47) | – |
| P25 | session_01TU99LstbiDzVfq5YrKsfhR | bounced 18:40 (protection.rs vs P23), fixing | – |
| P28 | session_0175PRhG1uGz8ANg7JsaM4kv | ready | – |
| P50 | session_01DAJAyPoAe9eeuKtZHeAemr | ready | – |
| P49 | session_01GKWEGkpY6svMi9tt5fJRB3 | running | – |
| integrator | session_0199JgoqhKWUQwuSikBwmDCv | draining; 9 batches; rust 1180 / web 517 at 6209ca1 | – |
Not started (dropped from this phase by decision): P39 P40 P43 P44 P45 P48 P52–P60 P62–P64 P66–P86 and P30.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
