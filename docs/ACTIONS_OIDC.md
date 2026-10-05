# Actions OIDC: keyless cloud authentication

Workflows can exchange a short-lived OpenID Connect token for cloud
credentials instead of storing long-lived secrets, exactly like on
GitHub. Code: `crates/bgh-actions/src/oidc.rs`.

## How it works

* **Issuer:** `https://<your-host>/_services/token` (the GHES convention).
  * Discovery: `https://<your-host>/_services/token/.well-known/openid-configuration`
  * JWKS: `https://<your-host>/_services/token/.well-known/jwks`

  Cloud providers fetch these over the internet, so the host needs a
  publicly trusted TLS certificate and must be reachable from the
  provider (at least these two paths).
* **Signing keys:** RS256, stored in `{BGH_DATA_DIR}/actions/oidc/`
  (`{created}-{kid}.pem`, mode 0600). The first key is created on first
  use. Keys rotate every 90 days (maintenance loop) or on demand with
  `POST /_bgh/admin/actions/oidc/rotate-key` (site admin). The previous
  key stays in the JWKS, so tokens issued just before a rotation keep
  validating. Back up this directory with the rest of the data dir; when
  several app nodes run, they must share it.
* **Requesting a token:** a job with

  ```yaml
  permissions:
    id-token: write
    contents: read
  ```

  gets `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
  (no variables without `id-token: write`; pull requests from forks never
  get it). `core.getIDToken(audience)` from `@actions/core`, and every
  action built on it, works unchanged. By hand:

  ```sh
  curl -sH "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
    "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=sts.amazonaws.com" | jq -r .value
  ```

  Tokens are valid for 5 minutes and only while the job runs.

## Claims

Same names and string formats as GitHub:

| Claim | Example |
|---|---|
| `iss` | `https://bgh.example.com/_services/token` |
| `aud` | the requested audience, else `https://bgh.example.com/<owner>` |
| `sub` | `repo:acme/api:ref:refs/heads/main`, `repo:acme/api:environment:prod`, `repo:acme/api:pull_request` |
| `repository`, `repository_id`, `repository_owner`, `repository_owner_id`, `repository_visibility` | `acme/api`, `42`, `acme`, `7`, `private` |
| `ref`, `ref_type`, `ref_protected`, `sha` | `refs/heads/main`, `branch`, `false`, `ffac537e…` |
| `workflow`, `workflow_ref`, `workflow_sha` | `Deploy`, `acme/api/.github/workflows/deploy.yml@refs/heads/main` |
| `job_workflow_ref`, `job_workflow_sha` | the called workflow for jobs of reusable workflows, else the same as `workflow_ref` |
| `run_id`, `run_number`, `run_attempt` | `1234`, `17`, `1` |
| `actor`, `actor_id`, `event_name`, `head_ref`, `base_ref` | `octocat`, `3`, `push`, ``, `` |
| `environment`, `environment_node_id` | only for jobs with `environment:` |
| `runner_environment` | always `self-hosted` |
| `check_run_id`, `jti`, `iat`, `nbf`, `exp` | |

### Customizing `sub`

`GET`/`PUT /repos/{owner}/{repo}/actions/oidc/customization/sub` and
`GET`/`PUT /orgs/{org}/actions/oidc/customization/sub` take GitHub's
bodies:

```sh
# Organization template (org owners)
gh api -X PUT orgs/acme/actions/oidc/customization/sub \
  --input - <<<'{"include_claim_keys": ["repository_owner_id", "context"]}'
# A repository opts into the organization template (repo admins)...
gh api -X PUT repos/acme/api/actions/oidc/customization/sub \
  --input - <<<'{"use_default": false}'
# ...or uses its own, or goes back to the default (`repo`, `context`).
gh api -X PUT repos/acme/api/actions/oidc/customization/sub \
  --input - <<<'{"use_default": false, "include_claim_keys": ["repo", "context", "job_workflow_ref"]}'
```

Keys are joined with `:` as `key:value`; `repo` renders `repo:owner/name`
and `context` renders `environment:<name>`, `pull_request` or
`ref:<ref>`. The example above produces
`repo:acme/api:ref:refs/heads/main:job_workflow_ref:acme/api/.github/workflows/deploy.yml@refs/heads/main`.

## Cloud trust setup

Below, `bgh.example.com` is your host and `acme/api` the repository.

### AWS

1. IAM → Identity providers → Add provider → OpenID Connect:
   provider URL `https://bgh.example.com/_services/token`, audience
   `sts.amazonaws.com`.
2. Create a role with a web identity trust policy:

   ```json
   {
     "Version": "2012-10-17",
     "Statement": [{
       "Effect": "Allow",
       "Principal": {"Federated": "arn:aws:iam::123456789012:oidc-provider/bgh.example.com/_services/token"},
       "Action": "sts:AssumeRoleWithWebIdentity",
       "Condition": {
         "StringEquals": {"bgh.example.com/_services/token:aud": "sts.amazonaws.com"},
         "StringLike": {"bgh.example.com/_services/token:sub": "repo:acme/api:ref:refs/heads/main"}
       }
     }]
   }
   ```
3. Workflow:

   ```yaml
   permissions: {id-token: write, contents: read}
   steps:
     - uses: aws-actions/configure-aws-credentials@v4
       with:
         role-to-assume: arn:aws:iam::123456789012:role/bgh-deploy
         aws-region: eu-west-1
   ```

### Google Cloud

```sh
gcloud iam workload-identity-pools create bgh --location=global
gcloud iam workload-identity-pools providers create-oidc bgh-actions \
  --location=global --workload-identity-pool=bgh \
  --issuer-uri=https://bgh.example.com/_services/token \
  --attribute-mapping=google.subject=assertion.sub,attribute.repository=assertion.repository \
  --attribute-condition="assertion.repository_owner == 'acme'"
gcloud iam service-accounts add-iam-policy-binding deploy@my-project.iam.gserviceaccount.com \
  --role=roles/iam.workloadIdentityUser \
  --member="principalSet://iam.googleapis.com/projects/123456/locations/global/workloadIdentityPools/bgh/attribute.repository/acme/api"
```

```yaml
permissions: {id-token: write, contents: read}
steps:
  - uses: google-github-actions/auth@v2
    with:
      workload_identity_provider: projects/123456/locations/global/workloadIdentityPools/bgh/providers/bgh-actions
      service_account: deploy@my-project.iam.gserviceaccount.com
```

### Azure

Add a federated credential to an app registration (or user-assigned
managed identity):

```sh
az ad app federated-credential create --id <app-object-id> --parameters '{
  "name": "bgh-acme-api-prod",
  "issuer": "https://bgh.example.com/_services/token",
  "subject": "repo:acme/api:environment:prod",
  "audiences": ["api://AzureADTokenExchange"]
}'
```

```yaml
permissions: {id-token: write, contents: read}
jobs:
  deploy:
    environment: prod
    steps:
      - uses: azure/login@v2
        with:
          client-id: ${{ vars.AZURE_CLIENT_ID }}
          tenant-id: ${{ vars.AZURE_TENANT_ID }}
          subscription-id: ${{ vars.AZURE_SUBSCRIPTION_ID }}
```

Third-party actions such as these are fetched from github.com, so the
runner needs `BGH_ACTIONS_REMOTE_ACTIONS` (or a mirrored copy of the
actions on your instance).
