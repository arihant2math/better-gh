# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 16:48 UTC (integration head 58386b7).

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1–P13 P15–P19 P21 P22 P24 P26 P31–P34 | (archived) | done | yes |
| P14 | session_015oaJ6Zeqc1HcMmbx6Ji8Lx | ready (bounced once, fixed e5cb309) | – |
| P36 | session_017gT9WihAABZpQdE8oFpAoA | ready | – |
| P23 | session_011a5XX8Do8JZq7A3nb5YxMQ | stalled at gate 15:54; nudged 16:47 | – |
| P35 | session_01UihJSzCZ6gHPVHBoaVEYHL | running | – |
| P20 | session_01YKLKPRAJnD9iwrqFqwFppG | running | – |
| P25 | session_01TU99LstbiDzVfq5YrKsfhR | running | – |
| P27 | session_01DhMk89Z1C3StA4YVVnw7nm | running | – |
| P29 | session_01KNLVYQ8fVEkFBNYQB8Z8qU | running | – |
| P37 | session_011o3zDh28KNGENgEM7MooWB | running | – |
| P38 | session_01JD3SCGswJFpG9KW6rU73Pi | running | – |
| P46 | session_01Fdy1y7wDZ4h259dMZKmFUn | running | – |
| P47 | session_011LfNiSENea9TZ1ajuzWFWH | running | – |
| P41 | session_019G1WTXr3Mz4YC5iKhUBtG5 | running | – |
| P42 | session_01CjtG4VR13kvpeAxtZtxVE2 | running | – |
| P51 | session_01Dfzr6SecvGXLsxpcFHL7Zb | running | – |
| P65 | session_011JdkwUVtjabABsRtPAjgBx | running | – |
| integrator | session_0199JgoqhKWUQwuSikBwmDCv | running; 3 batches, 9 pkgs landed, head 58386b7 | – |
Queue (wave order): W3 P39(P23) P44(P23) P50 P61; W4 P28 P30 P40 P43 P45(P38,P41,P42) P48 P49 P52 P53 P54 P55 P56 P58 P60 P62 P63 P66(P65); W5 …; W6 P77 P80–P86.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
