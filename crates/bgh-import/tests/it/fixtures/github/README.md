# GitHub REST fixtures (`octo-org/hello-world`)

Hand-written api.github.com responses for the importer tests, matching
GitHub's REST shapes (not captured from live traffic; `../github-recorded/`
holds real responses that `recorded.rs` checks the importer's field paths
against). URLs use `https://api.github.com` / `https://github.com`, and
tests rewrite the base. `repo.json` has `clone_url` set to the placeholder
`{{CLONE_URL}}`.

| File | Endpoint |
|---|---|
| `repo.json` | `GET /repos/octo-org/hello-world` |
| `labels.json` | `GET /repos/octo-org/hello-world/labels` |
| `milestones.json` | `GET /repos/octo-org/hello-world/milestones?state=all` |
| `issues.json` | `GET /repos/octo-org/hello-world/issues?state=all&sort=created&direction=asc` (#3, #4 are PRs) |
| `issue_comments.json` | `GET /repos/octo-org/hello-world/issues/comments?sort=created&direction=asc` |
| `issue_events.json` | `GET /repos/octo-org/hello-world/issues/events` (newest first, as GitHub returns it) |
| `reactions_issue_1.json` | `GET /repos/octo-org/hello-world/issues/1/reactions` |
| `reactions_comment_1000000001.json` | `GET /repos/octo-org/hello-world/issues/comments/1000000001/reactions` |
| `releases.json` | `GET /repos/octo-org/hello-world/releases` |
| `users/{octocat,hubot,monalisa}.json` | `GET /users/{login}` |
| `teams.json` | `GET /repos/octo-org/hello-world/teams` |
| `team_members_core.json` | `GET /orgs/octo-org/teams/core/members` |

Ids: repo 1296269, org 9919, users octocat 583231 / hubot 7 / monalisa 2,
labels 208045946-8, milestone 1002604 (number 1), issues 2000000001-5,
comments 1000000001-4, events 3000000001-9, reactions 4000000001-4,
release 1, asset 10, team 1.
