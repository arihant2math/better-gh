"""Real-client check of P46 (GitHub Apps part 2) with PyGithub.

Same server setup as p46-octokit.mjs; `pip install PyGithub`, then
`python3 scripts/real-clients/p46-pygithub.py http://127.0.0.1:3000`.
"""
import hashlib
import json
import sys
import time
from datetime import datetime, timedelta, timezone
import urllib.parse

import requests
from github import Auth, Github, GithubIntegration

base = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:3046"
api = f"{base}/api/v3"
failures = 0


def check(cond, msg):
    global failures
    print(("✓ " if cond else "✗ ") + msg)
    if not cond:
        failures += 1


s = requests.Session()
r = s.post(f"{base}/_bgh/session", json={"login": "octo", "password": "Passw0rd!x"})
session = s.cookies.get("bgh_session")
csrf = hashlib.sha256(f"bgh-csrf:{session}".encode()).hexdigest()[:40]
s.headers["x-csrf-token"] = csrf

manifest = {
    "name": f"PyGithub Probe {int(time.time()) % 100000}",
    "url": "https://example.com/py",
    "redirect_url": "https://example.com/redirect",
    "callback_urls": ["http://127.0.0.1:9/cb"],
    "default_permissions": {"checks": "write", "contents": "read", "issues": "write"},
    "default_events": [],
}
r = requests.post(
    f"{base}/organizations/acme/settings/apps/new",
    data={"manifest": json.dumps(manifest)},
    allow_redirects=False,
)
token = r.headers["location"].split("manifest=")[1]
confirm = s.post(f"{base}/_bgh/app-manifests/{token}", json={}).json()
code = urllib.parse.parse_qs(urllib.parse.urlparse(confirm["redirect_url"]).query)["code"][0]
conv = requests.post(f"{api}/app-manifests/{code}/conversions").json()
check(conv["slug"] == confirm["app_slug"], "manifest conversion")
inst = s.post(
    f"{base}/_bgh/apps/{conv['slug']}/installations",
    json={"account": "acme", "repository_selection": "all"},
).json()["installation"]

gi = GithubIntegration(auth=Auth.AppAuth(conv["id"], conv["pem"]), base_url=api)
app = gi.get_app()
check(app.id == conv["id"] and app.slug == conv["slug"], "GithubIntegration.get_app")
check(any(i.id == inst["id"] for i in gi.get_installations()), "GithubIntegration.get_installations")

g = gi.get_github_for_installation(inst["id"])
repo = g.get_repo("acme/widgets")
sha = repo.get_branch("main").commit.sha
run = repo.create_check_run(name="py-probe", head_sha=sha, status="completed", conclusion="success")
check(run.app.id == conv["id"] and run.app.slug == conv["slug"], "create_check_run attributes the run to the app")
suites = repo.get_commit(sha).get_check_suites(app_id=conv["id"])
check(suites.totalCount == 1 and suites[0].app.id == conv["id"], "get_check_suites(app_id=...) finds the app's suite")

# User-to-server token (code from the web flow), then refresh.
info = s.get(
    f"{base}/_bgh/oauth/authorize",
    params={"client_id": conv["client_id"], "redirect_uri": "http://127.0.0.1:9/cb"},
).json()
authz = s.post(f"{base}/_bgh/oauth/authorize", json={"consent": info["consent"], "authorize": True}).json()
ucode = urllib.parse.parse_qs(urllib.parse.urlparse(authz["redirect_url"]).query)["code"][0]
tok = requests.post(
    f"{base}/login/oauth/access_token",
    headers={"accept": "application/json"},
    data={"client_id": conv["client_id"], "client_secret": conv["client_secret"], "code": ucode},
).json()
user_auth = Auth.AppUserAuth(
    client_id=conv["client_id"],
    client_secret=conv["client_secret"],
    token=tok["access_token"],
    # Already expired: PyGithub refreshes it with the refresh token first.
    expires_at=datetime.now(timezone.utc) - timedelta(minutes=1),
    refresh_token=tok["refresh_token"],
    refresh_expires_at=datetime.now(timezone.utc) + timedelta(days=30),
)
gu = Github(auth=user_auth, base_url=api)
check(gu.get_user().login == "octo", "Auth.AppUserAuth acts as the user")
inst_ids = [i.id for i in gu.get_user().get_installations()]
check(inst_ids == [inst["id"]], "AuthenticatedUser.get_installations (this app only)")
check(user_auth.token != tok["access_token"] and user_auth.token.startswith("bghu_"), "AppUserAuth refreshed the expired token")

sys.exit(1 if failures else 0)
