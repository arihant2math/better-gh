//! Reusable workflows (`jobs.<id>.uses` → a workflow with `on.workflow_call`).
//!
//! A calling job becomes one `call` row per matrix combination (see
//! migration 2800); the scheduler ([`crate::engine::advance_run`]) treats
//! each call as a nested scope whose jobs are ordinary job rows keyed
//! `<call key>/<job>` (`<call key>.<index>/<job>` for matrix callers) and
//! named `<caller> / <job>`. When every job of the scope completed, the call
//! row completes with the aggregated result and the evaluated
//! `on.workflow_call.outputs`, so `needs.<caller>.outputs` works downstream.
//!
//! This module holds the parts that don't touch the scheduler: parsing and
//! resolving `uses:`, the access rules for workflows of other repositories,
//! typing `with:` against `on.workflow_call.inputs`, `secrets:` (mapping or
//! `inherit`), evaluating outputs, and applying the secret layers when a
//! runner claims a called job.

use std::collections::HashMap;

use bgh_core::AppState;
use bgh_core::models::db;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgConnection;

use crate::expr::{self, MapContext};
use crate::trigger::{self, WORKFLOWS_DIR};
use crate::workflow::{self, InputDef, Permissions, Workflow};

/// Maximum nesting: a top-level workflow may call workflows that call
/// workflows ... up to this many levels.
pub const MAX_DEPTH: u32 = 4;
/// Maximum number of distinct reusable workflows one run may call.
pub const MAX_UNIQUE_WORKFLOWS: usize = 20;

/// A parsed `jobs.<id>.uses` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRef {
    /// `./.github/workflows/x.yml`: same repository, same commit as the caller.
    Local { path: String },
    /// `owner/repo/.github/workflows/x.yml@ref`
    Remote {
        owner: String,
        repo: String,
        path: String,
        git_ref: String,
    },
}

fn check_path(path: &str, uses: &str) -> Result<(), String> {
    let file = path.strip_prefix(&format!("{WORKFLOWS_DIR}/"));
    let ok = file.is_some_and(|f| {
        !f.is_empty() && !f.contains('/') && (f.ends_with(".yml") || f.ends_with(".yaml"))
    });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid value workflow reference: '{uses}': workflows must be defined at the top level of the .github/workflows/ directory"
        ))
    }
}

/// Parse `uses:` of a calling job (syntax only).
pub fn parse_ref(uses: &str) -> Result<CallRef, String> {
    let uses = uses.trim();
    if let Some(path) = uses.strip_prefix("./") {
        if path.contains('@') {
            return Err(format!(
                "invalid value workflow reference: '{uses}': local workflow references cannot have a version"
            ));
        }
        check_path(path, uses)?;
        return Ok(CallRef::Local { path: path.into() });
    }
    let Some((spec, git_ref)) = uses.rsplit_once('@') else {
        return Err(format!(
            "invalid value workflow reference: '{uses}': no version specified (expected owner/repo/.github/workflows/file.yml@ref or ./.github/workflows/file.yml)"
        ));
    };
    let mut parts = spec.splitn(3, '/');
    let (Some(owner), Some(repo), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!(
            "invalid value workflow reference: '{uses}': references to workflows must be prefixed with format 'owner/repository/' or './' for local workflows"
        ));
    };
    if owner.is_empty() || repo.is_empty() || git_ref.is_empty() {
        return Err(format!(
            "invalid value workflow reference: '{uses}': references to workflows must be prefixed with format 'owner/repository/' or './' for local workflows"
        ));
    }
    check_path(path, uses)?;
    Ok(CallRef::Remote {
        owner: owner.into(),
        repo: repo.into(),
        path: path.into(),
        git_ref: git_ref.into(),
    })
}

/// The repository and commit a workflow file was read from (`./` calls
/// resolve against it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub repo_id: i64,
    /// `owner/repo`
    pub full_name: String,
    /// Ref the file was referenced by (`refs/heads/main`, `v1`, a SHA).
    pub git_ref: String,
    pub sha: String,
}

/// One `secrets:` hop between a caller and a called workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SecretsLayer {
    /// `secrets: inherit`: the called workflow sees the caller's secrets.
    Inherit,
    /// An explicit mapping (no `secrets:` = empty mapping). Values are raw
    /// expressions, evaluated when the job is claimed against the previous
    /// layer's secrets and `ctx` (the caller's matrix, needs, inputs, ...).
    Map {
        secrets: IndexMap<String, String>,
        ctx: Map<String, Value>,
    },
}

/// `actions_jobs.spec` of a `call` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCall {
    /// The caller's `uses:` as written.
    pub uses: String,
    /// `owner/repo/.github/workflows/x.yml@ref`
    pub workflow_ref: String,
    /// The called workflow's file path (`.github/workflows/x.yml`).
    pub path: String,
    /// Where the called workflow was read from.
    pub source: Source,
    /// Job key prefix of the called jobs (`build/`, `build.1/`).
    pub prefix: String,
    /// Name prefix of the called jobs (`Build (linux) / `).
    pub name_prefix: String,
    /// 1 for a call made by the top-level workflow.
    pub depth: u32,
    pub def: Workflow,
    /// Typed `inputs` context of the called workflow.
    pub inputs: Value,
    /// Secret layers from the top-level workflow down to this call.
    pub secrets: Vec<SecretsLayer>,
    /// `permissions:` of every caller on the way down (`None`: the caller
    /// had none, i.e. the default); the called jobs' token gets at most
    /// their intersection, and the innermost one when the called workflow
    /// sets no `permissions:` itself.
    #[serde(default)]
    pub permission_caps: Vec<Option<Permissions>>,
}

/// Job key of the top-level job a (possibly nested) job key belongs to:
/// `build.1/test/x` → `build`.
pub fn root_key(key: &str) -> &str {
    let first = key.split('/').next().unwrap_or(key);
    first.split('.').next().unwrap_or(first)
}

// ---------------------------------------------------------------------------
// Access to other repositories' workflows
// ---------------------------------------------------------------------------

/// `none` | `user` | `organization` | `enterprise` (GitHub's
/// `/actions/permissions/access`).
pub async fn access_level(conn: &mut PgConnection, repo_id: i64) -> Result<String, sqlx::Error> {
    let level: Option<String> =
        sqlx::query_scalar("SELECT access_level FROM actions_repo_access WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_optional(conn)
            .await?;
    Ok(level.unwrap_or_else(|| "none".into()))
}

/// May workflows of `caller` use workflows of `target`?
pub fn may_call(caller: &db::Repository, target: &db::Repository, target_access: &str) -> bool {
    if caller.id == target.id || target.visibility == "public" {
        return true;
    }
    let same_owner = caller.owner_id == target.owner_id;
    match target_access {
        "user" | "organization" => same_owner,
        // The whole server is the enterprise: any repository may call an
        // internal repository's workflows; private ones stay owner-only.
        "enterprise" => same_owner || target.visibility == "internal",
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// A resolved and parsed called workflow.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub workflow_ref: String,
    pub path: String,
    pub source: Source,
    pub def: Workflow,
}

/// Parsed workflow files by `(repo_id, sha, path)`, shared by the calls
/// resolved in one scheduler pass.
pub type FileCache = HashMap<(i64, String, String), Result<Workflow, String>>;

async fn read_workflow(
    state: &AppState,
    cache: &mut FileCache,
    repo_id: i64,
    sha: &str,
    path: &str,
) -> anyhow::Result<Result<Workflow, String>> {
    let key = (repo_id, sha.to_string(), path.to_string());
    if let Some(hit) = cache.get(&key) {
        return Ok(hit.clone());
    }
    let (rev, p) = (sha.to_string(), path.to_string());
    let data: Option<Vec<u8>> = trigger::store(state)
        .read(repo_id, move |r| {
            let entry = match r.lookup_path(&rev, &p) {
                Ok(bgh_git::PathLookup::Entry(e)) if e.kind == bgh_git::TreeEntryKind::Blob => e,
                Ok(_) | Err(bgh_git::GitError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(Some(
                r.blob_with_limit(&entry.sha, trigger::MAX_WORKFLOW_SIZE)?
                    .data,
            ))
        })
        .await?;
    let parsed = match data {
        None => Err(format!("workflow was not found: {path}")),
        Some(bytes) => workflow::parse_workflow(&String::from_utf8_lossy(&bytes))
            .map_err(|e| format!("error parsing called workflow \"{path}\": {e}")),
    };
    cache.insert(key, parsed.clone());
    Ok(parsed)
}

/// Resolve `uses:` of a job in a workflow read from `source`. `Err(msg)` is
/// a user-facing error that fails the calling job.
pub async fn resolve(
    state: &AppState,
    conn: &mut PgConnection,
    cache: &mut FileCache,
    caller: &db::Repository,
    source: &Source,
    uses: &str,
) -> anyhow::Result<Result<Resolved, String>> {
    let call = match parse_ref(uses) {
        Ok(c) => c,
        Err(e) => return Ok(Err(e)),
    };
    let (source, path) = match call {
        CallRef::Local { path } => (source.clone(), path),
        CallRef::Remote {
            owner,
            repo,
            path,
            git_ref,
        } => {
            let not_found = || {
                Err(format!(
                    "error parsing called workflow \"{uses}\": failed to fetch workflow: repository {owner}/{repo} was not found or is not accessible"
                ))
            };
            let Some(owner_row) = db::User::find_by_login(&mut *conn, &owner).await? else {
                return Ok(not_found());
            };
            let Some(target) =
                db::Repository::find_by_name(&mut *conn, owner_row.id, &repo).await?
            else {
                return Ok(not_found());
            };
            let level = access_level(conn, target.id).await?;
            if !may_call(caller, &target, &level) {
                return Ok(not_found());
            }
            let candidates =
                if git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit()) {
                    vec![git_ref.clone()]
                } else {
                    vec![
                        format!("refs/heads/{git_ref}"),
                        format!("refs/tags/{git_ref}"),
                        git_ref.clone(),
                    ]
                };
            let sha = trigger::store(state)
                .read(target.id, move |g| {
                    Ok(candidates.iter().find_map(|c| g.resolve_commit(c).ok()))
                })
                .await
                .ok()
                .flatten();
            let Some(sha) = sha else {
                return Ok(Err(format!(
                    "error parsing called workflow \"{uses}\": failed to fetch workflow: reference to workflow should be either a valid branch, tag, or commit (\"{git_ref}\" not found in {}/{})",
                    owner_row.login, target.name
                )));
            };
            (
                Source {
                    repo_id: target.id,
                    full_name: format!("{}/{}", owner_row.login, target.name),
                    git_ref,
                    sha,
                },
                path,
            )
        }
    };
    let def = match read_workflow(state, cache, source.repo_id, &source.sha, &path).await? {
        Ok(d) => d,
        Err(e) => {
            return Ok(Err(if e.starts_with("workflow was not found") {
                format!(
                    "error parsing called workflow \"{uses}\": workflow was not found in {}@{}",
                    source.full_name, source.git_ref
                )
            } else {
                e
            }));
        }
    };
    if !def.on.has("workflow_call") {
        return Ok(Err(format!(
            "error parsing called workflow \"{uses}\": workflow is not reusable as it is missing a `on.workflow_call` trigger"
        )));
    }
    Ok(Ok(Resolved {
        workflow_ref: format!("{}/{path}@{}", source.full_name, source.git_ref),
        path,
        source,
        def,
    }))
}

// ---------------------------------------------------------------------------
// with: / secrets:
// ---------------------------------------------------------------------------

fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn typed(name: &str, def: &InputDef, v: Value) -> Result<Value, String> {
    let bad = |v: &Value| {
        Err(format!(
            "Invalid input, {name} is expected to be a {}, but got {} '{}'",
            def.r#type,
            json_kind(v),
            expr::to_display_string(v)
        ))
    };
    match def.r#type.as_str() {
        "boolean" => match &v {
            Value::Bool(_) => Ok(v),
            Value::String(s) if s == "true" || s == "false" => Ok(Value::Bool(s == "true")),
            _ => bad(&v),
        },
        "number" => match &v {
            Value::Number(_) => Ok(v),
            Value::String(s) => match s.trim().parse::<f64>() {
                Ok(f) if f.fract() == 0.0 && f.abs() < 9e15 => Ok(json!(f as i64)),
                Ok(f) => Ok(json!(f)),
                Err(_) => bad(&v),
            },
            _ => bad(&v),
        },
        _ => match &v {
            Value::String(_) => Ok(v),
            Value::Bool(_) | Value::Number(_) => Ok(Value::String(expr::to_display_string(&v))),
            _ => bad(&v),
        },
    }
}

/// The typed `inputs` context of a called workflow from the caller's
/// evaluated `with:` (defaults applied, required inputs enforced).
pub fn typed_inputs(def: &Workflow, with: &IndexMap<String, Value>) -> Result<Value, String> {
    let empty = IndexMap::new();
    let decl = def
        .on
        .get("workflow_call")
        .map(|t| &t.inputs)
        .unwrap_or(&empty);
    for name in with.keys() {
        if !decl.contains_key(name) {
            return Err(format!(
                "Invalid input, {name} is not defined in the referenced workflow."
            ));
        }
    }
    let mut out = Map::new();
    for (name, d) in decl {
        let value = match with.get(name) {
            Some(v) => typed(name, d, v.clone())?,
            None => match &d.default {
                Some(dv) => typed(name, d, Value::String(dv.clone()))?,
                None if d.required => {
                    return Err(format!(
                        "Input {name} is required, but not provided while calling."
                    ));
                }
                None => match d.r#type.as_str() {
                    "boolean" => Value::Bool(false),
                    "number" => json!(0),
                    _ => Value::String(String::new()),
                },
            },
        };
        out.insert(name.clone(), value);
    }
    Ok(Value::Object(out))
}

/// The caller's `secrets:` as a layer, validated against the called
/// workflow's `on.workflow_call.secrets`.
pub fn secrets_layer(
    def: &Workflow,
    secrets: Option<&Value>,
    ctx: Map<String, Value>,
) -> Result<SecretsLayer, String> {
    let empty = IndexMap::new();
    let decl = def
        .on
        .get("workflow_call")
        .map(|t| &t.secrets)
        .unwrap_or(&empty);
    let mapping: IndexMap<String, String> = match secrets {
        Some(Value::String(s)) if s.trim() == "inherit" => return Ok(SecretsLayer::Inherit),
        None | Some(Value::Null) => IndexMap::new(),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    Value::String(s) => s.clone(),
                    other => expr::to_display_string(other),
                };
                (k.clone(), s)
            })
            .collect(),
        Some(other) => {
            return Err(format!(
                "Invalid secrets: expected a mapping or 'inherit', found {}",
                json_kind(other)
            ));
        }
    };
    for name in mapping.keys() {
        if !decl.keys().any(|d| d.eq_ignore_ascii_case(name)) {
            return Err(format!(
                "Invalid secret, {name} is not defined in the referenced workflow."
            ));
        }
    }
    for (name, d) in decl {
        let required = d
            .get("required")
            .is_some_and(|r| r == &Value::Bool(true) || r == "true");
        if required && !mapping.keys().any(|m| m.eq_ignore_ascii_case(name)) {
            return Err(format!(
                "Secret {name} is required, but not provided while calling."
            ));
        }
    }
    Ok(SecretsLayer::Map {
        secrets: mapping,
        ctx,
    })
}

/// Apply the secret layers of a called job to the caller repository's
/// secrets (`GITHUB_TOKEN` is added by the caller afterwards).
pub fn apply_secret_layers(
    base: IndexMap<String, String>,
    layers: &[SecretsLayer],
    github: &Value,
    vars: &Value,
) -> IndexMap<String, String> {
    let mut current = base;
    for layer in layers {
        let SecretsLayer::Map { secrets, ctx } = layer else {
            continue;
        };
        let secrets_json: Map<String, Value> = current
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let mut c = MapContext::new()
            .with("github", github.clone())
            .with("vars", vars.clone())
            .with("secrets", Value::Object(secrets_json));
        for (k, v) in ctx {
            c = c.with(k, v.clone());
        }
        current = secrets
            .iter()
            .map(|(k, v)| {
                (
                    k.to_ascii_uppercase(),
                    expr::interpolate(v, &c).unwrap_or_default(),
                )
            })
            .collect();
    }
    current
}

/// Evaluate `on.workflow_call.outputs` with the called workflow's `jobs`
/// context.
pub fn outputs(def: &Workflow, ctx: &MapContext) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(trig) = def.on.get("workflow_call") else {
        return out;
    };
    for (name, o) in &trig.outputs {
        let raw = match o {
            Value::Object(m) => m.get("value").cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        let v = match &raw {
            Value::String(s) => expr::interpolate(s, ctx).unwrap_or_default(),
            Value::Null => String::new(),
            other => expr::to_display_string(other),
        };
        out.insert(name.clone(), Value::String(v));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_remote_refs() {
        assert_eq!(
            parse_ref("./.github/workflows/build.yml"),
            Ok(CallRef::Local {
                path: ".github/workflows/build.yml".into()
            })
        );
        assert_eq!(
            parse_ref("octo/ci/.github/workflows/x.yaml@v1"),
            Ok(CallRef::Remote {
                owner: "octo".into(),
                repo: "ci".into(),
                path: ".github/workflows/x.yaml".into(),
                git_ref: "v1".into(),
            })
        );
        for bad in [
            "./.github/workflows/x.yml@v1",
            "octo/ci/.github/workflows/x.yml",
            "octo/.github/workflows/x.yml@v1",
            "octo/ci/workflows/x.yml@v1",
            "octo/ci/.github/workflows/sub/x.yml@v1",
            "./.github/workflows/x.txt",
        ] {
            assert!(parse_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn root_keys() {
        assert_eq!(root_key("build"), "build");
        assert_eq!(root_key("build/test"), "build");
        assert_eq!(root_key("build.2/test/x"), "build");
    }

    fn called(yaml: &str) -> Workflow {
        workflow::parse_workflow(yaml).unwrap()
    }

    #[test]
    fn inputs_are_typed_defaulted_and_checked() {
        let def = called(
            "on:\n  workflow_call:\n    inputs:\n      name:\n        type: string\n        required: true\n      n:\n        type: number\n        default: 2\n      flag:\n        type: boolean\njobs:\n  a:\n    runs-on: x\n    steps:\n      - run: 'true'\n",
        );
        let with: IndexMap<String, Value> = [("name".to_string(), json!(7))].into();
        assert_eq!(
            typed_inputs(&def, &with).unwrap(),
            json!({"name": "7", "n": 2, "flag": false})
        );
        let with: IndexMap<String, Value> = [
            ("name".to_string(), json!("x")),
            ("n".to_string(), json!("4.5")),
            ("flag".to_string(), json!("true")),
        ]
        .into();
        assert_eq!(
            typed_inputs(&def, &with).unwrap(),
            json!({"name": "x", "n": 4.5, "flag": true})
        );
        let err = typed_inputs(&def, &IndexMap::new()).unwrap_err();
        assert!(err.contains("Input name is required"), "{err}");
        let with: IndexMap<String, Value> = [
            ("name".to_string(), json!("x")),
            ("n".to_string(), json!("lots")),
        ]
        .into();
        let err = typed_inputs(&def, &with).unwrap_err();
        assert!(err.contains("n is expected to be a number"), "{err}");
        let with: IndexMap<String, Value> = [
            ("name".to_string(), json!("x")),
            ("flag".to_string(), json!(1)),
        ]
        .into();
        assert!(typed_inputs(&def, &with).is_err());
        let with: IndexMap<String, Value> = [
            ("name".to_string(), json!("x")),
            ("nope".to_string(), json!(1)),
        ]
        .into();
        let err = typed_inputs(&def, &with).unwrap_err();
        assert!(err.contains("nope is not defined"), "{err}");
    }

    #[test]
    fn secrets_mapping_inherit_and_layers() {
        let def = called(
            "on:\n  workflow_call:\n    secrets:\n      token:\n        required: true\n      extra: {}\njobs:\n  a:\n    runs-on: x\n    steps:\n      - run: 'true'\n",
        );
        assert_eq!(
            secrets_layer(&def, Some(&json!("inherit")), Map::new()),
            Ok(SecretsLayer::Inherit)
        );
        assert!(
            secrets_layer(&def, None, Map::new())
                .unwrap_err()
                .contains("token is required")
        );
        assert!(
            secrets_layer(&def, Some(&json!({"token": "a", "bogus": "b"})), Map::new())
                .unwrap_err()
                .contains("bogus is not defined")
        );
        let layer = secrets_layer(
            &def,
            Some(&json!({"token": "${{ secrets.DEPLOY }}-${{ matrix.os }}"})),
            [("matrix".to_string(), json!({"os": "linux"}))]
                .into_iter()
                .collect(),
        )
        .unwrap();
        let base: IndexMap<String, String> = [
            ("DEPLOY".to_string(), "d".to_string()),
            ("OTHER".to_string(), "o".to_string()),
        ]
        .into();
        let got = apply_secret_layers(
            base.clone(),
            &[SecretsLayer::Inherit, layer],
            &json!({}),
            &json!({}),
        );
        assert_eq!(got, [("TOKEN".to_string(), "d-linux".to_string())].into());
        assert_eq!(
            apply_secret_layers(
                base.clone(),
                &[SecretsLayer::Inherit],
                &json!({}),
                &json!({})
            ),
            base
        );
    }

    #[test]
    fn outputs_use_the_jobs_context() {
        let def = called(
            "on:\n  workflow_call:\n    outputs:\n      v:\n        value: ${{ jobs.a.outputs.x }}-${{ inputs.s }}\njobs:\n  a:\n    runs-on: x\n    steps:\n      - run: 'true'\n",
        );
        let ctx = MapContext::new()
            .with(
                "jobs",
                json!({"a": {"result": "success", "outputs": {"x": "1"}}}),
            )
            .with("inputs", json!({"s": "y"}));
        assert_eq!(Value::Object(outputs(&def, &ctx)), json!({"v": "1-y"}));
    }
}
