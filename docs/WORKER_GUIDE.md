# Worker guide (parallel package sessions)

You are one of several parallel Claude sessions, each owning one package from
`docs/WORKPLAN.md`. An orchestrator session merges your branch into the
integration branch `claude/sleepy-cray-9jj0t3`.

## Rules

1. Read `CLAUDE.md`, `docs/ARCHITECTURE.md`, `docs/BACKEND_PATTERNS.md`
   (backend) or `docs/FRONTEND.md` (web) first. Follow existing patterns.
2. Work on branch `bgh/<package>` created from
   `origin/claude/sleepy-cray-9jj0t3`. Push it often
   (`git push -u origin bgh/<package>`). Never push to any other branch.
3. Every hour or so, and before finishing: `git fetch origin
   claude/sleepy-cray-9jj0t3 && git merge origin/claude/sleepy-cray-9jj0t3`
   and resolve conflicts (merge commits, no rebase/force-push).
4. Stay inside your crate(s) and your migration range. Edits to shared
   files (`bgh-core`, workspace `Cargo.toml`, `events.rs`, `models::api`)
   must be **small and additive** (new variants/fns/structs, new deps) so
   parallel merges stay trivial. Never rename or remove shared items.
5. If another package must provide something you need, implement the
   minimal piece yourself in the most natural place (usually `bgh-core`),
   additive only, and note it in your status doc.
6. GitHub compatibility is the spec: match docs.github.com REST shapes,
   status codes, pagination and error formats. Write integration tests that
   assert the JSON shape for every endpoint.
7. Performance: no N+1 queries (batch loaders), indexes for every filter
   you add, stream large bodies, cache immutable git-derived data by SHA.
8. Use subagents (Agent tool) to parallelize within your package when
   useful, but this container has only 4 cores: at most one cargo build at a
   time (cargo locks `target/`), prefer `cargo check -p <crate>` while
   iterating.
9. Before each push: `cargo fmt --all`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo test -p <your crates>`. Before
   finishing: full `cargo test --workspace`.
10. Maintain `docs/packages/<package>.md`: status, implemented endpoints,
    tables/migrations added, shared-code changes, known gaps/TODOs. Update
    it with every push — the orchestrator reads it instead of your
    transcript.
11. Commit messages: imperative, scoped; end each with
    `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
12. Do not open pull requests. Finish when your package's scope is
    complete, tested, merged with the latest integration branch and pushed.
