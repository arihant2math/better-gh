# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 15:56 UTC (integration head fc37a0d).

| Pkg | Session | Status | Integrated |
|---|---|---|---|
| P1 P2 P3 P4 P6 P8 P9 P10 P11 P12 P13 P15 P17 P19 P21 P26 P32 | (archived) | done | yes |
| P5 | session_01CmrSgPubAQkvP7yknCbei2 | running (told to mark ready) | – |
| P7 | session_01XxCeLUQoB3PRQT5RkwZY2g | running | – |
| P14 | session_015oaJ6Zeqc1HcMmbx6Ji8Lx | running | – |
| P16 | session_01B4DTJATdqW1mSJHmmWb9Wa | running | – |
| P18 | session_01GCZQeiFKhZmCZ9kSSVYuhj | running | – |
| P22 | session_011DiZTuiTxtqETZPwkUXDBd | running | – |
| P23 | session_011a5XX8Do8JZq7A3nb5YxMQ | running | – |
| P24 | session_013V7SimtVzEK1wr1gPnzbp8 | running | – |
| P31 | session_0135E2gRphdqsuGh3NuM22bU | running | – |
| P33 | session_01R3zjuV5cFpQYvAK45NCBpL | running | – |
| P34 | session_018TWcu1B2pwqsZFho7XJ6H7 | running | – |
| P35 | session_01UihJSzCZ6gHPVHBoaVEYHL | running | – |
| P36 | session_017gT9WihAABZpQdE8oFpAoA | running | – |

Queue (wave order): W2 P35 P36; W3 P20(P19) P25 P27 P29 P37 P38 P39(P23,P26) P41 P42 P44(P19,P23) P46 P47 P50 P51(P18) P61 P65; W4 …; W5 …; W6 P77 P80–P86.

## Integration process (since 15:53 UTC)

Workers no longer push to the integration branch. They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".
