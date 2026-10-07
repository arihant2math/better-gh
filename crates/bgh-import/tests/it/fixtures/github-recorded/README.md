# Recorded GitHub responses

Real api.github.com responses, captured on 2026-10-05 (unmodified, from the
repository this environment is allowed to read):

| File | Endpoint |
|---|---|
| `repo.json` | `GET /repos/{owner}/{repo}` |
| `labels.json` | `GET /repos/{owner}/{repo}/labels?per_page=100` |

`../../recorded.rs` asserts that every field the importer reads exists with
the expected type. Issues, comments, events, reactions, releases and users
could not be recorded here (the repository has none, and other repositories
and `/users/*` are outside this environment's GitHub access); they are
covered by the hand-written fixtures in `../github/` and by
`self_import.rs`, which imports from this server's own GitHub-compatible
API.
