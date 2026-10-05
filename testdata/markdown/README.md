# Markdown parity corpus (P35)

Each `NAME.md` is rendered by the server (`bgh_core::markdown::render`) and
the web client (`web/src/ui/markdown/render.ts`) with the same context:
base URL `https://bgh.example`, repository `octo/demo`, and one custom
autolink `JIRA-` → `https://jira.example/browse/<num>` (alphanumeric).

`NAME.html` is the server output (snapshot). The Rust test
(`markdown::tests::golden_corpus`) asserts the server still produces it
exactly; regenerate with `BGH_UPDATE_GOLDEN=1 cargo test -p bgh-core
golden_corpus`. The web test (`render.golden.test.ts`) asserts the client
renders the same DOM after canonicalization (attributes sorted, `rel` and
`target` dropped, whitespace-only text between blocks removed).
