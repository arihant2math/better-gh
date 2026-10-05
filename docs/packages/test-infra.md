# T1 test-infra — status

**Done.** Branch `bgh/test-infra`, merged with the integration branch.

## What changed

* **One integration-test binary per crate.** Every `crates/*/tests/*.rs`
  (each its own binary statically linking the whole server) moved to
  `crates/*/tests/it/<name>.rs` and is declared as a module in
  `crates/*/tests/it/main.rs`. Shared helpers moved with them
  (`tests/it/common/`, `bgh-repos` `tests/it/gitwork/`, `bgh-notify`
  `tests/it/support/`) and are declared once in `main.rs`; the former
  `mod common;` lines became `use crate::common;`. No test logic changed.
  Cargo auto-discovers `tests/it/main.rs` as test target `it`, so no
  `Cargo.toml` `[[test]]` entries are needed.
* **`[profile.dev] split-debuginfo = "unpacked"`** (workspace `Cargo.toml`,
  also used by `test`): DWARF stays in per-crate `.dwo` files instead of
  being copied into every linked binary. Backtraces still show
  `file:line` (checked). Existing settings (`debug = "line-tables-only"`,
  deps `opt-level = 1`/`debug = false`, argon2/blake2 `opt-level = 3`)
  kept. Argon2 params unchanged: the test harness already caches the test
  user's password hash, and hashing isn't a measurable cost.
* **Guard:** `scripts/check-test-layout.sh` (new CI step in the `scripts`
  job) fails on any top-level `crates/*/tests/*.rs` or any
  `tests/it/*.rs` / helper dir not declared in its `main.rs` (an
  undeclared file would silently never run).
* Docs: CLAUDE.md, `docs/BACKEND_PATTERNS.md` §13 (where tests go, how to
  run one file: `cargo test -p <crate> --test it <module>::`, no
  process-global state in tests), `docs/ARCHITECTURE.md` testing section,
  path references updated.

## Isolation audit

Tests of a crate now share one process. Checked for `env::set_var`,
`set_current_dir`, fixed ports, statics: none in tests (servers bind
`127.0.0.1:0`; `127.0.0.1:5555`/`:9` are only URLs never listened on;
`bgh-server` `infra.rs`'s `LISTENED` counter is only used by its own
`register`). `bgh_core::testing` was already per-process safe (DB per
`TestApp`, `OnceCell` template, cross-process advisory lock). Nothing
needed fixing.

## Measurements (4 cores, `CARGO_INCREMENTAL=0`, cold = empty `target/`)

| | Before | After |
|---|---|---|
| Test executables (`--no-run`) | 93 (75 integration) | 33 (15 integration) |
| `du -sh target` after `cargo test --workspace --no-run` | 25 G | 6.1 G |
| Cold `cargo test --workspace --no-run` | 567 s | 479 s (502 s without split-debuginfo) |
| `cargo test --workspace` run (already built) | 298 s | 186–246 s (DB-bound, noisy) |
| Cold build + full run | ~865 s | 625 s |
| Tests | 794 passed, 11 ignored, 0 failed | 794 passed, 11 ignored, 0 failed |

Without split-debuginfo the consolidated layout alone gave 6.8 G. Each
`it` binary is ~270 MB (vs ~320 MB with embedded DWARF). Remaining build
time is dominated by compiling dependencies and the workspace crates
themselves, not linking.

## Notes for other packages

* New test file: `crates/<crate>/tests/it/<name>.rs` + `mod <name>;` in
  `tests/it/main.rs`; use helpers via `use crate::common;`.
* Run one file: `cargo test -p <crate> --test it <name>::`.
* Don't mutate process-global state in tests (env vars, cwd, statics
  asserted by count); every test of the crate runs in the same process.
