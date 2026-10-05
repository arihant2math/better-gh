# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 16:20 UTC (integration head ce257f2).

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1–P4 P6 P8–P13 P15 P17–P19 P21 P24 P26 P32 | (archived) | done | yes |
| P5 | session_01CmrSgPubAQkvP7yknCbei2 | ready, in integration queue | – |
| P7 | session_01XxCeLUQoB3PRQT5RkwZY2g | ready, in integration queue | – |
| P14 | session_015oaJ6Zeqc1HcMmbx6Ji8Lx | ready, in integration queue | – |
| P16 | session_01B4DTJATdqW1mSJHmmWb9Wa | ready, in integration queue | – |
| P22 | session_011DiZTuiTxtqETZPwkUXDBd | ready, in integration queue | – |
| P31 | session_0135E2gRphdqsuGh3NuM22bU | ready, in integration queue | – |
| P33 | session_01R3zjuV5cFpQYvAK45NCBpL | ready, in integration queue | – |
| P34 | session_018TWcu1B2pwqsZFho7XJ6H7 | ready, in integration queue | – |
| P23 | session_011a5XX8Do8JZq7A3nb5YxMQ | running | – |
| P35 | session_01UihJSzCZ6gHPVHBoaVEYHL | running | – |
| P36 | session_017gT9WihAABZpQdE8oFpAoA | running | – |
| P20 | session_01YKLKPRAJnD9iwrqFqwFppG | running | – |
| P25 | session_01TU99LstbiDzVfq5YrKsfhR | running | – |
| P27 | session_01DhMk89Z1C3StA4YVVnw7nm | running | – |
| P29 | session_01KNLVYQ8fVEkFBNYQB8Z8qU | running | – |
| P37 | session_011o3zDh28KNGENgEM7MooWB | running | – |
| P38 | session_01JD3SCGswJFpG9KW6rU73Pi | running | – |
| P46 | session_01Fdy1y7wDZ4h259dMZKmFUn | running | – |
| P47 | session_011LfNiSENea9TZ1ajuzWFWH | running | – |
| integrator | session_0199JgoqhKWUQwuSikBwmDCv | running (batch 1: P18 P24 → ce257f2) | – |
Queue (wave order): W3 P39(P23) P41 P42 P44(P23) P50 P51 P61 P65; W4 …; W5 …; W6 P77 P80–P86.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
