# Phase 4 fleet (foreman-maintained, branch bgh/foreman only)

Integration branch: `claude/sleepy-cray-9jj0t3`. Updated 2026-10-05 19:24 UTC (integration head 98abd45). They mark `Integration: ready` in their status doc and push their own branch; the integrator session `session_0199JgoqhKWUQwuSikBwmDCv` (branch `bgh/integrator`, log `docs/INTEGRATION_LOG.md`) lands batches of up to 4 with one gate run per batch. See docs/WORKER_GUIDE.md rule 14 + "Integration queue".

## Standing launch notes

- Bundle: integration b02d3ec initial JS is 142.8 KB gzip of 150 KB (orchestrator, 15:54). Every web-touching prompt adds: "new UI must be lazy-loaded route chunks; do not grow the initial bundle". P68 must bring initial JS back under ~120 KB.
- Gate baseline at b02d3ec: 950 Rust tests, 341 web tests, green.
