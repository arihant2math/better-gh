# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 17:16 UTC (integration head 7ac1b95).

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1–P19 P21 P22 P24 P26 P29 P31–P35 | (archived) | done | yes |
| P20 | session_01YKLKPRAJnD9iwrqFqwFppG | ready | – |
| P23 | session_011a5XX8Do8JZq7A3nb5YxMQ | ready (resumed after nudge) | – |
| P27 | session_01DhMk89Z1C3StA4YVVnw7nm | ready | – |
| P37 | session_011o3zDh28KNGENgEM7MooWB | ready | – |
| P47 | session_011LfNiSENea9TZ1ajuzWFWH | ready | – |
| P36 | session_017gT9WihAABZpQdE8oFpAoA | bounced 17:25 (conflict w/ P14/P7), fixing | – |
| P25 | session_01TU99LstbiDzVfq5YrKsfhR | running | – |
| P38 | session_01JD3SCGswJFpG9KW6rU73Pi | running | – |
| P46 | session_01Fdy1y7wDZ4h259dMZKmFUn | running | – |
| P41 | session_019G1WTXr3Mz4YC5iKhUBtG5 | running | – |
| P42 | session_01CjtG4VR13kvpeAxtZtxVE2 | running | – |
| P51 | session_01Dfzr6SecvGXLsxpcFHL7Zb | running | – |
| P65 | session_011JdkwUVtjabABsRtPAjgBx | running | – |
| P50 | session_01DAJAyPoAe9eeuKtZHeAemr | running | – |
| P61 | session_01MJEBcx6auSQf7p1TLMCmFf | running | – |
| P49 | session_01GKWEGkpY6svMi9tt5fJRB3 | running | – |
| P28 | session_0175PRhG1uGz8ANg7JsaM4kv | running | – |
| integrator | session_0199JgoqhKWUQwuSikBwmDCv | running; 6 batches, 12 pkgs landed, head 7ac1b95 (rust 1066, web 459) | – |
Queue (wave order): W3 P39(P23) P44(P23); W4 P30 P40 P43 P45(P38,P41,P42) P48 P52 P53 P54 P55 P56 P58 P60 P62 P63 P66(P65); W5 …; W6 P77 P80–P86.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
