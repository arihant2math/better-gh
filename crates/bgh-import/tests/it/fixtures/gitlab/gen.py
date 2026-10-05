#!/usr/bin/env python3
"""Generate the `gitlab/` fixtures (GitLab REST API v4 shapes, docs.gitlab.com).

Run from this directory: `python3 gen.py`. `https://gitlab.example` is
rewritten to the fake; `{{SHA_*}}` / `{{CLONE_URL}}` are filled in by the test.
"""
import json

WEB = "https://gitlab.example"
PATH = "group/sub/glproj"
PID = 99


def user(uid, username, name):
    return {"id": uid, "username": username, "name": name, "state": "active",
            "locked": False, "avatar_url": f"{WEB}/uploads/-/system/user/avatar/{uid}/avatar.png",
            "web_url": f"{WEB}/{username}"}


OCTO = user(11, "octocat", "The Octocat")
HUBOT = user(12, "hubot", "Hubot")
MONA = user(13, "mona", "Mona")

PROJECT = {
    "id": PID, "description": "GitLab import fixture", "name": "glproj",
    "name_with_namespace": "group / sub / glproj", "path": "glproj",
    "path_with_namespace": PATH, "created_at": "2024-01-01T00:00:00.000Z",
    "default_branch": "main", "tag_list": ["legacy"], "topics": ["gitlab", "import"],
    "ssh_url_to_repo": f"git@gitlab.example:{PATH}.git", "http_url_to_repo": "{{CLONE_URL}}",
    "web_url": f"{WEB}/{PATH}", "readme_url": f"{WEB}/{PATH}/-/blob/main/README.md",
    "visibility": "public", "issues_enabled": True, "merge_requests_enabled": True,
    "wiki_enabled": True, "archived": False, "last_activity_at": "2024-03-01T00:00:00.000Z",
    "namespace": {"id": 7, "name": "sub", "path": "sub", "kind": "group", "full_path": "group/sub"},
}

LABELS = [{"id": 501, "name": "bug", "color": "#d9534f", "text_color": "#FFFFFF",
           "description": "Something is broken", "open_issues_count": 1,
           "closed_issues_count": 0, "open_merge_requests_count": 0, "subscribed": False,
           "priority": None, "is_project_label": True}]

MILESTONE = {"id": 601, "iid": 1, "project_id": PID, "title": "v1", "description": "First",
             "state": "active", "created_at": "2024-01-02T00:00:00.000Z",
             "updated_at": "2024-01-02T00:00:00.000Z", "due_date": "2024-06-30",
             "start_date": None, "expired": False, "web_url": f"{WEB}/{PATH}/-/milestones/1"}


def issue(iid, title, author, state, created, *, closed=None, closed_by=None, labels=(),
          milestone=None, assignees=(), confidential=False, upvotes=0, locked=False):
    return {
        "id": 7000 + iid, "iid": iid, "project_id": PID, "title": title,
        "description": f"Body of {title}", "state": state, "created_at": created,
        "updated_at": closed or created, "closed_at": closed, "closed_by": closed_by,
        "labels": list(labels), "milestone": milestone, "assignees": list(assignees),
        "author": author, "type": "ISSUE", "assignee": assignees[0] if assignees else None,
        "user_notes_count": 1, "merge_requests_count": 0, "upvotes": upvotes, "downvotes": 0,
        "due_date": None, "confidential": confidential, "discussion_locked": locked,
        "issue_type": "issue", "web_url": f"{WEB}/{PATH}/-/issues/{iid}",
        "time_stats": {"time_estimate": 0, "total_time_spent": 0},
        "task_completion_status": {"count": 0, "completed_count": 0},
    }


ISSUES = [
    issue(1, "Crash on start", OCTO, "opened", "2024-01-05T10:00:00.000Z", labels=["bug"],
          milestone=MILESTONE, assignees=[HUBOT], upvotes=1),
    issue(2, "Docs typo", MONA, "closed", "2024-01-06T10:00:00.000Z",
          closed="2024-01-07T10:00:00.000Z", closed_by=OCTO),
    issue(3, "Security hole", HUBOT, "opened", "2024-01-08T10:00:00.000Z", confidential=True),
]


def mr(iid, title, author, state, source, head, *, merged_at=None, merge_user=None,
       merge_sha=None, closed_at=None, closed_by=None, draft=False, reviewers=(),
       created="2024-02-01T10:00:00.000Z", updated="2024-02-02T10:00:00.000Z", upvotes=0):
    return {
        "id": 8000 + iid, "iid": iid, "project_id": PID, "title": title,
        "description": f"MR {title}", "state": state, "created_at": created,
        "updated_at": updated, "merged_by": merge_user, "merge_user": merge_user,
        "merged_at": merged_at, "closed_by": closed_by, "closed_at": closed_at,
        "target_branch": "main", "source_branch": source, "user_notes_count": 1,
        "upvotes": upvotes, "downvotes": 0, "author": author, "assignees": [author],
        "assignee": author, "reviewers": list(reviewers), "source_project_id": PID,
        "target_project_id": PID, "labels": [], "draft": draft,
        "work_in_progress": draft, "milestone": None, "merge_when_pipeline_succeeds": False,
        "merge_status": "can_be_merged", "detailed_merge_status": "mergeable",
        "sha": head, "merge_commit_sha": merge_sha, "squash_commit_sha": None,
        "discussion_locked": None, "should_remove_source_branch": None,
        "force_remove_source_branch": False, "allow_collaboration": False,
        "reference": f"!{iid}", "references": {"short": f"!{iid}", "full": f"{PATH}!{iid}"},
        "web_url": f"{WEB}/{PATH}/-/merge_requests/{iid}",
        "time_stats": {"time_estimate": 0, "total_time_spent": 0}, "squash": False,
        "task_completion_status": {"count": 0, "completed_count": 0},
        "has_conflicts": False, "blocking_discussions_resolved": True,
    }


MRS = [
    mr(1, "Add merged.txt", OCTO, "merged", "feature-merged", "{{SHA_F1}}",
       merged_at="2024-02-02T12:00:00.000Z", merge_user=HUBOT, merge_sha="{{SHA_MERGE}}",
       updated="2024-02-02T12:00:00.000Z"),
    mr(2, "Shout TWO, add four", OCTO, "opened", "feature-open", "{{SHA_O1}}", draft=True,
       reviewers=[HUBOT], created="2024-02-10T10:00:00.000Z",
       updated="2024-02-12T10:00:00.000Z", upvotes=1),
    mr(3, "Abandoned idea", MONA, "closed", "experiment", "{{SHA_C1}}",
       closed_at="2024-02-04T09:00:00.000Z", closed_by=MONA,
       created="2024-02-03T10:00:00.000Z", updated="2024-02-04T09:00:00.000Z"),
]


def full(m, base, start):
    out = dict(m)
    out["diff_refs"] = {"base_sha": base, "head_sha": m["sha"], "start_sha": start}
    out["changes_count"] = "1"
    out["first_contribution"] = False
    return out


def note(nid, author, body, at, *, system=False, kind=None, position=None, resolvable=False,
         resolved=False, resolved_by=None, noteable="Issue", noteable_iid=1):
    out = {"id": nid, "type": kind, "body": body, "attachment": None, "author": author,
           "created_at": at, "updated_at": at, "system": system, "noteable_id": 1,
           "noteable_type": noteable, "project_id": PID, "noteable_iid": noteable_iid,
           "resolvable": resolvable, "confidential": False, "internal": False,
           "commands_changes": {}}
    if position:
        out["position"] = position
    if resolvable:
        out["resolved"] = resolved
        out["resolved_by"] = resolved_by
        out["resolved_at"] = "2024-02-12T09:30:00.000Z" if resolved else None
    return out


NOTES_1 = [
    note(9001, HUBOT, "Reproduced it", "2024-01-05T11:00:00.000Z"),
    note(9002, OCTO, "added ~bug label", "2024-01-05T11:01:00.000Z", system=True),
]
NOTES_2 = [note(9003, OCTO, "Fixed, thanks", "2024-01-07T09:00:00.000Z", noteable_iid=2)]

POSITION = {"base_sha": "{{SHA_M1}}", "start_sha": "{{SHA_M1}}", "head_sha": "{{SHA_O1}}",
            "old_path": "hello.txt", "new_path": "hello.txt", "position_type": "text",
            "old_line": None, "new_line": 2,
            "line_range": {"start": {"line_code": "x_1_2", "type": "new", "old_line": None,
                                     "new_line": 2},
                           "end": {"line_code": "x_1_2", "type": "new", "old_line": None,
                                   "new_line": 2}}}
# Line 2 of the first head ("2"), rewritten to "TWO" since: outdated.
OLD_POSITION = dict(POSITION, head_sha="{{SHA_O0}}", line_range=None)

DISCUSSIONS_2 = [
    {"id": "6a9c1750b37d513a43987b574953fceb50b03ce7", "individual_note": False, "notes": [
        note(9101, MONA, "Why uppercase?", "2024-02-11T10:00:00.000Z", kind="DiffNote",
             position=POSITION, resolvable=True, resolved=True, resolved_by=OCTO,
             noteable="MergeRequest", noteable_iid=2),
        note(9102, OCTO, "Emphasis!", "2024-02-11T12:00:00.000Z", kind="DiffNote",
             position=POSITION, resolvable=True, resolved=True, resolved_by=OCTO,
             noteable="MergeRequest", noteable_iid=2),
    ]},
    {"id": "87805b7c09016a7058e91bdbe7b29d1f284a39e6", "individual_note": True, "notes": [
        note(9103, HUBOT, "Looks good overall", "2024-02-11T13:00:00.000Z",
             noteable="MergeRequest", noteable_iid=2),
    ]},
    {"id": "1f2a3b", "individual_note": True, "notes": [
        note(9104, OCTO, "marked this merge request as **draft**", "2024-02-10T10:01:00.000Z",
             system=True, noteable="MergeRequest", noteable_iid=2),
    ]},
    # Outdated: on a line of an earlier head that is gone now.
    {"id": "2e3f4a", "individual_note": False, "notes": [
        note(9105, MONA, "Old remark on two", "2024-02-10T11:00:00.000Z", kind="DiffNote",
             position=OLD_POSITION, resolvable=True, noteable="MergeRequest", noteable_iid=2),
    ]},
]
DISCUSSIONS_1 = [{"id": "aa11", "individual_note": True, "notes": [
    note(9201, HUBOT, "Merging", "2024-02-02T11:00:00.000Z", noteable="MergeRequest",
         noteable_iid=1)]}]

APPROVALS_1 = {"id": 8001, "iid": 1, "project_id": PID, "title": "Add merged.txt",
               "state": "merged", "approved": True, "approvals_required": 0,
               "approvals_left": 0, "approved_by": [{"user": HUBOT}]}
APPROVALS_EMPTY = lambda iid: {"id": 8000 + iid, "iid": iid, "project_id": PID,
                               "approved": False, "approved_by": []}

AWARDS_ISSUE_1 = [{"id": 1, "name": "thumbsup", "user": HUBOT,
                   "created_at": "2024-01-05T12:00:00.000Z", "updated_at": "2024-01-05T12:00:00.000Z",
                   "awardable_id": 7001, "awardable_type": "Issue"}]
AWARDS_MR_2 = [{"id": 2, "name": "rocket", "user": MONA,
                "created_at": "2024-02-11T09:00:00.000Z", "updated_at": "2024-02-11T09:00:00.000Z",
                "awardable_id": 8002, "awardable_type": "MergeRequest"},
               {"id": 3, "name": "unicorn", "user": HUBOT,
                "created_at": "2024-02-11T09:00:00.000Z", "updated_at": "2024-02-11T09:00:00.000Z",
                "awardable_id": 8002, "awardable_type": "MergeRequest"}]

FILES = {
    "project.json": PROJECT,
    "labels.json": LABELS,
    "milestones.json": [MILESTONE],
    "issues.json": ISSUES,
    "issue_1_notes.json": NOTES_1,
    "issue_2_notes.json": NOTES_2,
    "issue_1_award_emoji.json": AWARDS_ISSUE_1,
    "merge_requests.json": MRS,
    "merge_request_1.json": full(MRS[0], "{{SHA_M1}}", "{{SHA_M1}}"),
    "merge_request_2.json": full(MRS[1], "{{SHA_M1}}", "{{SHA_M1}}"),
    "merge_request_3.json": full(MRS[2], "{{SHA_M1}}", "{{SHA_M1}}"),
    "merge_request_1_discussions.json": DISCUSSIONS_1,
    "merge_request_2_discussions.json": DISCUSSIONS_2,
    "merge_request_3_discussions.json": [],
    "merge_request_1_approvals.json": APPROVALS_1,
    "merge_request_2_approvals.json": APPROVALS_EMPTY(2),
    "merge_request_3_approvals.json": APPROVALS_EMPTY(3),
    "merge_request_2_award_emoji.json": AWARDS_MR_2,
    "users/11.json": dict(OCTO, public_email="octocat@github.com", bio=""),
    "users/12.json": dict(HUBOT, public_email="", bio=""),
    "users/13.json": dict(MONA, public_email=None, bio=""),
}

import os
os.makedirs("users", exist_ok=True)
for name, body in FILES.items():
    with open(name, "w") as f:
        json.dump(body, f, indent=2)
        f.write("\n")
