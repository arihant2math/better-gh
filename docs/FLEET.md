# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 17:45 UTC (integration head 7ac1b95). STOPPED NEW LAUNCHES (orchestrator/user decision 17:48): only P25 P28 P49 P50 P51 finish; integrator drains the queue, then a final full gate.

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1–P19 P21 P22 P24 P26 P29 P31–P35 | (archived) | done | yes |
| P20 | session_01YKLKPRAJnD9iwrqFqwFppG | ready | – |
| P23 | session_011a5XX8Do8JZq7A3nb5YxMQ | ready (in batch 7) | – |
| P27 | session_01DhMk89Z1C3StA4YVVnw7nm | ready (in batch 7) | – |
| P37 | session_011o3zDh28KNGENgEM7MooWB | ready | – |
| P47 | session_011LfNiSENea9TZ1ajuzWFWH | ready (in batch 7) | – |
| P36 | session_017gT9WihAABZpQdE8oFpAoA | ready (bounce fixed, in batch 7) | – |
| P25 | session_01TU99LstbiDzVfq5YrKsfhR | running | – |
| P38 | session_01JD3SCGswJFpG9KW6rU73Pi | ready | – |
| P46 | session_01Fdy1y7wDZ4h259dMZKmFUn | ready | – |
| P41 | session_019G1WTXr3Mz4YC5iKhUBtG5 | ready | – |
| P42 | session_01CjtG4VR13kvpeAxtZtxVE2 | ready | – |
| P51 | session_01Dfzr6SecvGXLsxpcFHL7Zb | running | – |
| P65 | session_011JdkwUVtjabABsRtPAjgBx | ready | – |
| P50 | session_01DAJAyPoAe9eeuKtZHeAemr | running | – |
| P61 | session_01MJEBcx6auSQf7p1TLMCmFf | ready | – |
| P49 | session_01GKWEGkpY6svMi9tt5fJRB3 | running | – |
| P28 | session_0175PRhG1uGz8ANg7JsaM4kv | running | – |
| integrator | session_0199JgoqhKWUQwuSikBwmDCv | running; 6 batches, 12 pkgs landed, head 7ac1b95 (rust 1066, web 459) | – |
Not started (dropped from this phase by decision): P39 P40 P43 P44 P45 P48 P52–P60 P62–P64 P66–P86 and P30.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
