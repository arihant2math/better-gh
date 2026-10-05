#!/usr/bin/env python3
"""Generate the `github-pulls/` fixtures (GitHub REST shapes, docs.github.com).

Run from this directory: `python3 gen.py`. Commit SHAs are placeholders
(`{{SHA_*}}`) that the test fills in with real commits of its git source;
`https://api.github.com` / `https://github.com` are rewritten to the fake.
"""
import json

API = "https://api.github.com"
WEB = "https://github.com"
REPO = "octo-org/pulls-demo"
REPO_ID = 4242
R = f"{API}/repos/{REPO}"


def user(login, uid):
    u = f"{API}/users/{login}"
    return {
        "login": login, "id": uid, "node_id": f"MDQ6VXNlcj{uid}",
        "avatar_url": f"https://avatars.githubusercontent.com/u/{uid}?v=4",
        "gravatar_id": "", "url": u, "html_url": f"{WEB}/{login}",
        "followers_url": f"{u}/followers", "following_url": f"{u}/following{{/other_user}}",
        "gists_url": f"{u}/gists{{/gist_id}}", "starred_url": f"{u}/starred{{/owner}}{{/repo}}",
        "subscriptions_url": f"{u}/subscriptions", "organizations_url": f"{u}/orgs",
        "repos_url": f"{u}/repos", "events_url": f"{u}/events{{/privacy}}",
        "received_events_url": f"{u}/received_events", "type": "User",
        "user_view_type": "public", "site_admin": False,
    }


OCTO = user("octocat", 583231)
HUBOT = user("hubot", 7)
MONA = user("monalisa", 2)
ORG = dict(user("octo-org", 9919), type="Organization")


def repo_obj(full=REPO, rid=REPO_ID, owner=ORG, fork=False):
    u = f"{API}/repos/{full}"
    return {
        "id": rid, "node_id": f"R_{rid}", "name": full.split("/")[1], "full_name": full,
        "private": False, "owner": owner, "html_url": f"{WEB}/{full}",
        "description": "Pull request import fixture", "fork": fork, "url": u,
        "clone_url": "{{CLONE_URL}}", "git_url": f"git://github.com/{full}.git",
        "ssh_url": f"git@github.com:{full}.git", "homepage": "", "default_branch": "main",
        "visibility": "public", "has_issues": True, "has_projects": True,
        "has_wiki": True, "has_discussions": False, "archived": False, "disabled": False,
        "topics": ["import"], "created_at": "2024-01-01T00:00:00Z",
        "updated_at": "2024-03-01T00:00:00Z", "pushed_at": "2024-03-01T00:00:00Z",
    }


REPO_OBJ = repo_obj()
FORK_OBJ = repo_obj("monalisa/pulls-demo", 5151, MONA, fork=True)

LABEL = {"id": 300000001, "node_id": "LA_1", "url": f"{R}/labels/enhancement",
         "name": "enhancement", "color": "a2eeef", "default": True,
         "description": "New feature or request"}


def ref(label, refname, sha, repo, owner):
    return {"label": label, "ref": refname, "sha": sha, "user": owner, "repo": repo}


def pull(n, title, author, state, head, base, body, created, updated, *, closed=None,
         merged=None, merge_sha=None, draft=False, labels=(), assignees=(), reviewers=(),
         locked=False):
    u = f"{R}/pulls/{n}"
    return {
        "url": u, "id": 1000 + n, "node_id": f"PR_{n}", "html_url": f"{WEB}/{REPO}/pull/{n}",
        "diff_url": f"{WEB}/{REPO}/pull/{n}.diff", "patch_url": f"{WEB}/{REPO}/pull/{n}.patch",
        "issue_url": f"{R}/issues/{n}", "number": n, "state": state, "locked": locked,
        "title": title, "user": author, "body": body, "created_at": created,
        "updated_at": updated, "closed_at": closed, "merged_at": merged,
        "merge_commit_sha": merge_sha, "assignee": assignees[0] if assignees else None,
        "assignees": list(assignees), "requested_reviewers": list(reviewers),
        "requested_teams": [], "labels": list(labels), "milestone": None, "draft": draft,
        "commits_url": f"{u}/commits", "review_comments_url": f"{u}/comments",
        "review_comment_url": f"{R}/pulls/comments{{/number}}",
        "comments_url": f"{R}/issues/{n}/comments", "statuses_url": f"{R}/statuses/{head['sha']}",
        "head": head, "base": base,
        "_links": {"self": {"href": u}, "html": {"href": f"{WEB}/{REPO}/pull/{n}"}},
        "author_association": "MEMBER", "auto_merge": None, "active_lock_reason": None,
    }


main = lambda sha: ref("octo-org:main", "main", sha, REPO_OBJ, ORG)
P3 = pull(3, "Add merged.txt", OCTO, "closed",
          ref("octo-org:feature-merged", "feature-merged", "{{SHA_F1}}", REPO_OBJ, ORG),
          main("{{SHA_M1}}"), "Adds a file.", "2024-02-01T10:00:00Z", "2024-02-02T12:00:00Z",
          closed="2024-02-02T12:00:00Z", merged="2024-02-02T12:00:00Z",
          merge_sha="{{SHA_MERGE}}", labels=[LABEL])
P4 = pull(4, "Experiment from a fork", MONA, "closed",
          ref("monalisa:experiment", "experiment", "{{SHA_C1}}", FORK_OBJ, MONA),
          main("{{SHA_M1}}"), "Not going anywhere.", "2024-02-03T10:00:00Z",
          "2024-02-04T09:00:00Z", closed="2024-02-04T09:00:00Z", merge_sha="{{SHA_C1}}")
P6 = pull(6, "Shout TWO, add four", OCTO, "open",
          ref("octo-org:feature-open", "feature-open", "{{SHA_O1}}", REPO_OBJ, ORG),
          main("{{SHA_M1}}"), "Work in progress.", "2024-02-10T10:00:00Z",
          "2024-02-12T10:00:00Z", draft=True, assignees=[OCTO], reviewers=[HUBOT],
          merge_sha="0000000000000000000000000000000000000000")

P3_FULL = dict(P3, merged=True, mergeable=None, merged_by=HUBOT, comments=0,
               review_comments=0, commits=1, additions=1, deletions=0, changed_files=1,
               maintainer_can_modify=False)


def issue(n, title, author, state, created, updated, *, closed=None, is_pull=None,
          reactions=0, body=None, state_reason=None):
    u = f"{R}/issues/{n}"
    out = {
        "url": u, "repository_url": R, "labels_url": f"{u}/labels{{/name}}",
        "comments_url": f"{u}/comments", "events_url": f"{u}/events",
        "html_url": f"{WEB}/{REPO}/{'pull' if is_pull else 'issues'}/{n}",
        "id": 2000 + n, "node_id": f"I_{n}", "number": n, "title": title, "user": author,
        "labels": [], "state": state, "locked": False, "assignee": None, "assignees": [],
        "milestone": None, "comments": 0, "created_at": created, "updated_at": updated,
        "closed_at": closed, "author_association": "MEMBER", "type": None,
        "active_lock_reason": None, "body": body, "closed_by": None,
        "reactions": {"url": f"{u}/reactions", "total_count": reactions, "+1": reactions,
                      "-1": 0, "laugh": 0, "hooray": 0, "confused": 0, "heart": 0,
                      "rocket": 0, "eyes": 0},
        "timeline_url": f"{u}/timeline", "performed_via_github_app": None,
        "state_reason": state_reason,
    }
    if is_pull:
        out["draft"] = is_pull.get("draft", False)
        out["pull_request"] = {"url": f"{R}/pulls/{n}", "html_url": f"{WEB}/{REPO}/pull/{n}",
                               "diff_url": f"{WEB}/{REPO}/pull/{n}.diff",
                               "patch_url": f"{WEB}/{REPO}/pull/{n}.patch",
                               "merged_at": is_pull.get("merged_at")}
    return out


ISSUES = [
    issue(1, "First issue", OCTO, "open", "2024-01-05T10:00:00Z", "2024-01-05T10:00:00Z"),
    issue(2, "Second issue", HUBOT, "closed", "2024-01-06T10:00:00Z", "2024-01-07T10:00:00Z",
          closed="2024-01-07T10:00:00Z", state_reason="completed"),
    issue(3, P3["title"], OCTO, "closed", P3["created_at"], P3["updated_at"],
          closed=P3["closed_at"], is_pull=P3),
    issue(4, P4["title"], MONA, "closed", P4["created_at"], P4["updated_at"],
          closed=P4["closed_at"], is_pull=P4),
    issue(5, "Third issue", MONA, "open", "2024-02-05T10:00:00Z", "2024-02-05T10:00:00Z"),
    issue(6, P6["title"], OCTO, "open", P6["created_at"], P6["updated_at"], is_pull=P6,
          reactions=1),
]


def review(rid, n, author, state, body, sha, at):
    return {
        "id": rid, "node_id": f"PRR_{rid}", "user": author, "body": body, "state": state,
        "html_url": f"{WEB}/{REPO}/pull/{n}#pullrequestreview-{rid}",
        "pull_request_url": f"{R}/pulls/{n}", "author_association": "MEMBER",
        "_links": {"html": {"href": f"{WEB}/{REPO}/pull/{n}#pullrequestreview-{rid}"},
                   "pull_request": {"href": f"{R}/pulls/{n}"}},
        "submitted_at": at, "commit_id": sha,
    }


REVIEWS = {
    3: [review(80000001, 3, HUBOT, "APPROVED", "Ship it", "{{SHA_F1}}", "2024-02-02T11:00:00Z")],
    4: [],
    6: [
        review(80000002, 6, MONA, "CHANGES_REQUESTED", "Please keep it lowercase",
               "{{SHA_O1}}", "2024-02-11T10:00:00Z"),
        review(80000003, 6, OCTO, "COMMENTED", "", "{{SHA_O1}}", "2024-02-11T12:00:00Z"),
        review(80000004, 6, HUBOT, "COMMENTED", "Thoughts on the whole file", "{{SHA_O1}}",
               "2024-02-12T09:00:00Z"),
    ],
}

HUNK = "@@ -1,3 +1,4 @@\n one\n-two\n+TWO"


def rcomment(cid, rid, author, body, at, *, path="hello.txt", position=3, original_position=3,
             line=2, original_line=2, side="RIGHT", start_line=None, original_start_line=None,
             start_side=None, reply_to=None, commit="{{SHA_O1}}", original="{{SHA_O1}}",
             hunk=HUNK, subject="line", reactions=0):
    u = f"{R}/pulls/comments/{cid}"
    out = {
        "url": u, "pull_request_review_id": rid, "id": cid, "node_id": f"PRRC_{cid}",
        "diff_hunk": hunk, "path": path, "position": position,
        "original_position": original_position, "commit_id": commit,
        "original_commit_id": original, "user": author, "body": body, "created_at": at,
        "updated_at": at, "html_url": f"{WEB}/{REPO}/pull/6#discussion_r{cid}",
        "pull_request_url": f"{R}/pulls/6", "author_association": "MEMBER",
        "_links": {"self": {"href": u}, "html": {"href": f"{WEB}/{REPO}/pull/6#discussion_r{cid}"},
                   "pull_request": {"href": f"{R}/pulls/6"}},
        "start_line": start_line, "original_start_line": original_start_line,
        "start_side": start_side, "line": line, "original_line": original_line,
        "side": side, "subject_type": subject,
        "reactions": {"url": f"{u}/reactions", "total_count": reactions, "+1": 0, "-1": 0,
                      "laugh": 0, "hooray": reactions, "confused": 0, "heart": 0,
                      "rocket": 0, "eyes": 0},
    }
    if reply_to:
        out["in_reply_to_id"] = reply_to
    return out


REVIEW_COMMENTS = [
    # A thread on the changed line 2 (position 3 of the hunk) ...
    rcomment(90000001, 80000002, MONA, "Why uppercase?", "2024-02-11T10:00:00Z", reactions=1),
    # ... with a reply (its own COMMENTED review, like GitHub's).
    rcomment(90000002, 80000003, OCTO, "Emphasis!", "2024-02-11T12:00:00Z", reply_to=90000001),
    # A multi-line comment (lines 1-3 on the right side).
    rcomment(90000003, 80000002, MONA, "This block reads oddly", "2024-02-11T10:00:30Z",
             position=4, original_position=4, line=3, original_line=3, start_line=1,
             original_start_line=1, start_side="RIGHT",
             hunk="@@ -1,3 +1,4 @@\n one\n-two\n+TWO\n three"),
    # Outdated: made on an earlier head; GitHub reports position null.
    rcomment(90000004, 80000002, MONA, "Old remark on two", "2024-02-11T10:01:00Z",
             position=None, original_position=2, line=None, original_line=2, side="LEFT",
             commit="{{SHA_O1}}", original="{{SHA_O0}}", hunk="@@ -1,3 +1,3 @@\n one\n-two"),
    # A file-level comment.
    rcomment(90000005, 80000004, HUBOT, "Rename this file?", "2024-02-12T09:00:00Z",
             position=None, original_position=None, line=None, original_line=None,
             side=None, hunk="", subject="file"),
]


def comment(cid, n, author, body, at):
    return {
        "url": f"{R}/issues/comments/{cid}", "html_url": f"{WEB}/{REPO}/pull/{n}#issuecomment-{cid}",
        "issue_url": f"{R}/issues/{n}", "id": cid, "node_id": f"IC_{cid}", "user": author,
        "created_at": at, "updated_at": at, "author_association": "MEMBER", "body": body,
        "reactions": {"url": f"{R}/issues/comments/{cid}/reactions", "total_count": 0, "+1": 0,
                      "-1": 0, "laugh": 0, "hooray": 0, "confused": 0, "heart": 0,
                      "rocket": 0, "eyes": 0},
        "performed_via_github_app": None,
    }


COMMENTS = [
    comment(70000001, 1, HUBOT, "On the first issue", "2024-01-05T11:00:00Z"),
    comment(70000002, 3, HUBOT, "Merging, thanks!", "2024-02-02T11:30:00Z"),
    comment(70000003, 6, MONA, "Looking at this now", "2024-02-11T09:00:00Z"),
]


def event(eid, n, actor, kind, at, issue_obj, **extra):
    out = {"id": eid, "node_id": f"E_{eid}", "url": f"{R}/issues/events/{eid}", "actor": actor,
           "event": kind, "commit_id": None, "commit_url": None, "created_at": at,
           "performed_via_github_app": None, "issue": issue_obj}
    out.update(extra)
    return out


by_n = {i["number"]: i for i in ISSUES}
EVENTS = [
    event(60000006, 6, OCTO, "review_requested", "2024-02-10T10:05:00Z", by_n[6],
          review_requester=OCTO, requested_reviewer=HUBOT),
    event(60000005, 4, MONA, "head_ref_deleted", "2024-02-04T09:01:00Z", by_n[4]),
    event(60000004, 4, MONA, "closed", "2024-02-04T09:00:00Z", by_n[4], state_reason=None),
    event(60000003, 3, HUBOT, "closed", "2024-02-02T12:00:00Z", by_n[3],
          commit_id="{{SHA_MERGE}}", state_reason=None),
    event(60000002, 3, HUBOT, "merged", "2024-02-02T12:00:00Z", by_n[3],
          commit_id="{{SHA_MERGE}}"),
    event(60000001, 3, OCTO, "labeled", "2024-02-01T10:01:00Z", by_n[3],
          label={"name": "enhancement", "color": "a2eeef"}),
    event(60000000, 6, OCTO, "subscribed", "2024-02-10T10:00:00Z", by_n[6]),
]

REACTIONS_6 = [{"id": 50000001, "node_id": "REA_1", "user": HUBOT, "content": "+1",
                "created_at": "2024-02-10T11:00:00Z"}]
REACTIONS_RC = [{"id": 50000002, "node_id": "REA_2", "user": OCTO, "content": "hooray",
                 "created_at": "2024-02-11T10:05:00Z"}]

HOOKS = [{
    "type": "Repository", "id": 12345678, "name": "web", "active": True,
    "events": ["push", "pull_request"],
    "config": {"content_type": "json", "insecure_ssl": "0", "url": "https://ci.example.com/hook",
               "secret": "********"},
    "updated_at": "2024-01-02T00:00:00Z", "created_at": "2024-01-02T00:00:00Z",
    "url": f"{R}/hooks/12345678", "test_url": f"{R}/hooks/12345678/test",
    "ping_url": f"{R}/hooks/12345678/pings", "deliveries_url": f"{R}/hooks/12345678/deliveries",
    "last_response": {"code": 200, "status": "active", "message": "OK"},
}]

BRANCHES = [{"name": "main", "commit": {"sha": "{{SHA_MAIN}}", "url": f"{R}/commits/{{{{SHA_MAIN}}}}"},
             "protected": True}]

PROTECTION = {
    "url": f"{R}/branches/main/protection",
    "required_status_checks": {"url": f"{R}/branches/main/protection/required_status_checks",
                               "strict": True, "contexts": ["ci/build"],
                               "contexts_url": f"{R}/branches/main/protection/required_status_checks/contexts",
                               "checks": [{"context": "ci/build", "app_id": None}]},
    "required_pull_request_reviews": {
        "url": f"{R}/branches/main/protection/required_pull_request_reviews",
        "dismiss_stale_reviews": True, "require_code_owner_reviews": False,
        "require_last_push_approval": False, "required_approving_review_count": 2},
    "required_signatures": {"url": f"{R}/branches/main/protection/required_signatures", "enabled": False},
    "enforce_admins": {"url": f"{R}/branches/main/protection/enforce_admins", "enabled": True},
    "required_linear_history": {"enabled": True},
    "allow_force_pushes": {"enabled": False},
    "allow_deletions": {"enabled": False},
    "block_creations": {"enabled": False},
    "required_conversation_resolution": {"enabled": True},
    "lock_branch": {"enabled": False},
    "allow_fork_syncing": {"enabled": False},
}

RULESET = {
    "id": 77, "name": "release tags", "target": "tag", "source_type": "Repository",
    "source": REPO, "enforcement": "active",
    "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}],
    "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
    "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}],
    "node_id": "RRS_77", "_links": {"self": {"href": f"{R}/rulesets/77"}},
    "created_at": "2024-01-03T00:00:00Z", "updated_at": "2024-01-03T00:00:00Z",
}
RULESETS = [{k: RULESET[k] for k in ("id", "name", "target", "source_type", "source",
                                     "enforcement", "node_id", "_links", "created_at",
                                     "updated_at")}]

FILES = {
    "repo.json": REPO_OBJ,
    "labels.json": [LABEL],
    "issues.json": ISSUES,
    "pulls.json": [P3, P4, P6],
    "pull_3.json": P3_FULL,
    "reviews_3.json": REVIEWS[3],
    "reviews_4.json": REVIEWS[4],
    "reviews_6.json": REVIEWS[6],
    "pull_comments.json": REVIEW_COMMENTS,
    "issue_comments.json": COMMENTS,
    "issue_events.json": EVENTS,
    "reactions_issue_6.json": REACTIONS_6,
    "reactions_pull_comment_90000001.json": REACTIONS_RC,
    "hooks.json": HOOKS,
    "branches_protected.json": BRANCHES,
    "protection_main.json": PROTECTION,
    "rulesets.json": RULESETS,
    "ruleset_77.json": RULESET,
    "empty.json": [],
    "users/octocat.json": dict(OCTO, name="The Octocat", email="octocat@github.com"),
    "users/hubot.json": dict(HUBOT, name="Hubot", email=None),
    "users/monalisa.json": dict(MONA, name="Mona Lisa Octocat", email=None),
}

for name, body in FILES.items():
    with open(name, "w") as f:
        json.dump(body, f, indent=2)
        f.write("\n")
