//! Issue templates and issue forms from `.github/ISSUE_TEMPLATE`, exposed
//! to the web client as `GET /_bgh/repos/{owner}/{repo}/issue-templates`.
//!
//! Parsed results are cached in Redis keyed by commit SHA (immutable).

use std::collections::HashSet;

use axum::extract::State;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use bgh_git::TreeEntryKind;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TEMPLATE_DIR: &str = ".github/ISSUE_TEMPLATE";
const LEGACY_PATHS: [&str; 6] = [
    ".github/ISSUE_TEMPLATE.md",
    ".github/issue_template.md",
    "ISSUE_TEMPLATE.md",
    "issue_template.md",
    "docs/ISSUE_TEMPLATE.md",
    "docs/issue_template.md",
];
const MAX_FILES: usize = 50;
const MAX_FILE_SIZE: u64 = 256 * 1024;
const CACHE_TTL_SECS: u64 = 24 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Template {
    pub filename: String,
    /// `markdown` | `form`
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// `about` for Markdown templates, `description` for forms.
    pub about: String,
    pub title: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub projects: Vec<String>,
    pub issue_type: Option<String>,
    /// Markdown template body.
    pub body: Option<String>,
    /// Issue form elements (validated `body` array).
    pub form: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactLink {
    pub name: String,
    pub url: String,
    pub about: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemplateConfig {
    pub blank_issues_enabled: bool,
    pub contact_links: Vec<ContactLink>,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            blank_issues_enabled: true,
            contact_links: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemplateError {
    pub filename: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Templates {
    /// Commit the templates were read from (`null` for empty repositories).
    pub commit_sha: Option<String>,
    pub templates: Vec<Template>,
    pub config: TemplateConfig,
    pub errors: Vec<TemplateError>,
}

/// Comma-separated string or list of strings.
fn string_list(v: Option<&serde_yaml::Value>) -> Vec<String> {
    match v {
        Some(serde_yaml::Value::String(s)) => s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.trim().to_string()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}

fn yaml_str(v: &serde_yaml::Value, key: &str) -> Option<String> {
    match v.get(key)? {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Parse a Markdown template with YAML front matter.
pub fn parse_markdown(filename: &str, text: &str) -> Result<Template, String> {
    let text = text.trim_start_matches('\u{feff}');
    let (front, body) = match text.strip_prefix("---") {
        Some(rest) => {
            let end = rest
                .find("\n---")
                .ok_or_else(|| "front matter is not terminated".to_string())?;
            let body = &rest[end + 4..];
            let body = body.split_once('\n').map(|(_, b)| b).unwrap_or("");
            (Some(&rest[..end]), body)
        }
        None => (None, text),
    };
    let meta: serde_yaml::Value = match front {
        Some(f) => serde_yaml::from_str(f).map_err(|e| format!("invalid front matter: {e}"))?,
        None => serde_yaml::Value::Null,
    };
    let legacy = !filename.starts_with(TEMPLATE_DIR);
    let name = yaml_str(&meta, "name");
    let about = yaml_str(&meta, "about");
    let (name, about) = match (name, about) {
        (Some(n), Some(a)) => (n, a),
        (n, a) if legacy => (
            n.unwrap_or_else(|| "Issue".to_string()),
            a.unwrap_or_default(),
        ),
        _ => return Err("template must define `name` and `about`".into()),
    };
    Ok(Template {
        filename: filename.to_string(),
        kind: "markdown".into(),
        name,
        about,
        title: yaml_str(&meta, "title").filter(|t| !t.is_empty()),
        labels: string_list(meta.get("labels")),
        assignees: string_list(meta.get("assignees")),
        projects: string_list(meta.get("projects")),
        issue_type: yaml_str(&meta, "type"),
        body: Some(body.to_string()),
        form: None,
    })
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Parse and validate an issue form (`.yml` / `.yaml`).
pub fn parse_form(filename: &str, text: &str) -> Result<Template, String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("invalid YAML: {e}"))?;
    let name = yaml_str(&doc, "name").ok_or("form must define `name`")?;
    let description = yaml_str(&doc, "description").ok_or("form must define `description`")?;
    let body = doc
        .get("body")
        .and_then(|b| b.as_sequence())
        .ok_or("form must define a `body` array")?;
    if body.is_empty() {
        return Err("`body` must contain at least one element".into());
    }
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();
    let mut elements = Vec::with_capacity(body.len());
    let mut has_input = false;
    for (i, el) in body.iter().enumerate() {
        let n = i + 1;
        let kind = yaml_str(el, "type").ok_or(format!("body[{n}]: missing `type`"))?;
        let attrs = el
            .get("attributes")
            .ok_or(format!("body[{n}]: missing `attributes`"))?;
        if let Some(id) = yaml_str(el, "id") {
            if !is_valid_id(&id) {
                return Err(format!("body[{n}]: invalid id {id:?}"));
            }
            if !ids.insert(id.clone()) {
                return Err(format!("body[{n}]: duplicate id {id:?}"));
            }
        }
        match kind.as_str() {
            "markdown" => {
                if yaml_str(attrs, "value").is_none() {
                    return Err(format!("body[{n}]: markdown requires `attributes.value`"));
                }
            }
            "textarea" | "input" | "dropdown" | "checkboxes" => {
                has_input = true;
                let label = yaml_str(attrs, "label")
                    .ok_or(format!("body[{n}]: missing `attributes.label`"))?;
                if !labels.insert(label.to_lowercase()) {
                    return Err(format!("body[{n}]: duplicate label {label:?}"));
                }
                if kind == "dropdown" || kind == "checkboxes" {
                    let opts = attrs
                        .get("options")
                        .and_then(|o| o.as_sequence())
                        .filter(|o| !o.is_empty())
                        .ok_or(format!("body[{n}]: {kind} requires `attributes.options`"))?;
                    if kind == "checkboxes" && opts.iter().any(|o| yaml_str(o, "label").is_none()) {
                        return Err(format!("body[{n}]: checkbox options require `label`"));
                    }
                    if kind == "dropdown" {
                        let mut seen = HashSet::new();
                        for o in opts {
                            let s = match o {
                                serde_yaml::Value::String(s) => s.clone(),
                                serde_yaml::Value::Number(n) => n.to_string(),
                                _ => {
                                    return Err(format!(
                                        "body[{n}]: dropdown options must be strings"
                                    ));
                                }
                            };
                            if !seen.insert(s.to_lowercase()) {
                                return Err(format!("body[{n}]: duplicate dropdown option {s:?}"));
                            }
                        }
                    }
                }
            }
            other => return Err(format!("body[{n}]: unknown type {other:?}")),
        }
        elements.push(serde_json::to_value(el).map_err(|e| e.to_string())?);
    }
    if !has_input {
        return Err("form must contain at least one non-markdown field".into());
    }
    Ok(Template {
        filename: filename.to_string(),
        kind: "form".into(),
        name,
        about: description,
        title: yaml_str(&doc, "title").filter(|t| !t.is_empty()),
        labels: string_list(doc.get("labels")),
        assignees: string_list(doc.get("assignees")),
        projects: string_list(doc.get("projects")),
        issue_type: yaml_str(&doc, "type"),
        body: None,
        form: Some(elements),
    })
}

/// Parse `config.yml`.
pub fn parse_config(text: &str) -> Result<TemplateConfig, String> {
    let doc: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| format!("invalid YAML: {e}"))?;
    let mut cfg = TemplateConfig::default();
    if let Some(b) = doc.get("blank_issues_enabled").and_then(|b| b.as_bool()) {
        cfg.blank_issues_enabled = b;
    }
    if let Some(links) = doc.get("contact_links").and_then(|l| l.as_sequence()) {
        for (i, l) in links.iter().enumerate() {
            match (
                yaml_str(l, "name"),
                yaml_str(l, "url"),
                yaml_str(l, "about"),
            ) {
                (Some(name), Some(url), about) if url.starts_with("http") => {
                    cfg.contact_links.push(ContactLink {
                        name,
                        url,
                        about: about.unwrap_or_default(),
                    })
                }
                _ => {
                    return Err(format!(
                        "contact_links[{}]: requires name, url and about",
                        i + 1
                    ));
                }
            }
        }
    }
    Ok(cfg)
}

/// Build [`Templates`] from `(path, contents)` files.
pub fn parse_all(commit_sha: Option<String>, files: Vec<(String, Vec<u8>)>) -> Templates {
    let mut out = Templates {
        commit_sha,
        ..Default::default()
    };
    for (path, data) in files {
        let text = String::from_utf8_lossy(&data);
        let lower = path.to_lowercase();
        let file = lower.rsplit('/').next().unwrap_or("");
        let result = if file == "config.yml" || file == "config.yaml" {
            match parse_config(&text) {
                Ok(c) => {
                    out.config = c;
                    continue;
                }
                Err(e) => Err(e),
            }
        } else if lower.ends_with(".md") {
            parse_markdown(&path, &text)
        } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
            parse_form(&path, &text)
        } else {
            continue;
        };
        match result {
            Ok(t) => out.templates.push(t),
            Err(message) => out.errors.push(TemplateError {
                filename: path,
                message,
            }),
        }
    }
    out.templates.sort_by(|a, b| a.filename.cmp(&b.filename));
    out
}

/// Read template files at `rev` (default branch when `None`).
pub async fn load(
    state: &AppState,
    repo: &db::Repository,
    rev: Option<String>,
) -> ApiResult<Templates> {
    let store = bgh_git::RepoStore::from_config(&state.config);
    let rev = rev.unwrap_or_else(|| repo.default_branch.clone());
    let sha = match store
        .read(repo.id, {
            let rev = rev.clone();
            move |r| r.resolve(&rev)
        })
        .await
    {
        Ok(Some(sha)) => sha,
        Ok(None) | Err(bgh_git::GitError::NotFound(_)) => return Ok(Templates::default()),
        Err(e) => return Err(e.into()),
    };
    let key = state.redis_key(&format!("issues:templates:{}:{sha}", repo.id));
    let mut redis = state.redis.clone();
    if let Ok(Some(cached)) = redis.get::<_, Option<String>>(&key).await
        && let Ok(t) = serde_json::from_str::<Templates>(&cached)
    {
        return Ok(t);
    }
    let commit = sha.clone();
    let files = store
        .read(repo.id, move |r| {
            let mut files = Vec::new();
            let read = |path: String, sha: &str, files: &mut Vec<(String, Vec<u8>)>| {
                if let Ok(b) = r.blob_with_limit(sha, MAX_FILE_SIZE) {
                    files.push((path, b.data));
                }
            };
            if let Ok(bgh_git::PathLookup::Tree { entries, .. }) =
                r.lookup_path(&commit, TEMPLATE_DIR)
            {
                for e in entries
                    .iter()
                    .filter(|e| e.kind == TreeEntryKind::Blob)
                    .take(MAX_FILES)
                {
                    read(format!("{TEMPLATE_DIR}/{}", e.name), &e.sha, &mut files);
                }
            }
            if files
                .iter()
                .all(|(p, _)| p.to_lowercase().ends_with("config.yml"))
            {
                for path in LEGACY_PATHS {
                    if let Ok(bgh_git::PathLookup::Entry(e)) = r.lookup_path(&commit, path) {
                        read(path.to_string(), &e.sha, &mut files);
                        break;
                    }
                }
            }
            Ok(files)
        })
        .await?;
    let parsed = parse_all(Some(sha), files);
    if let Ok(s) = serde_json::to_string(&parsed) {
        let _: Result<(), _> = redis.set_ex(&key, s, CACHE_TTL_SECS).await;
    }
    Ok(parsed)
}

#[derive(Debug, Default, Deserialize)]
pub struct TemplatesQuery {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
}

/// `GET /_bgh/repos/{owner}/{repo}/issue-templates[?ref=]`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<TemplatesQuery>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let t = load(&state, &access.repo, q.git_ref).await?;
    Ok(Json(json!(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_template() {
        let t = parse_markdown(
            ".github/ISSUE_TEMPLATE/bug.md",
            "---\nname: Bug report\nabout: Report a bug\ntitle: '[BUG] '\nlabels: bug, triage\nassignees:\n  - alice\n---\n\n**Describe**\n",
        )
        .unwrap();
        assert_eq!(t.name, "Bug report");
        assert_eq!(t.labels, vec!["bug", "triage"]);
        assert_eq!(t.assignees, vec!["alice"]);
        assert_eq!(t.title.as_deref(), Some("[BUG] "));
        assert_eq!(t.body.as_deref(), Some("\n**Describe**\n"));
        assert!(parse_markdown(".github/ISSUE_TEMPLATE/x.md", "no front matter").is_err());
    }

    #[test]
    fn form_validation() {
        let ok = "name: Bug\ndescription: File a bug\nbody:\n  - type: markdown\n    attributes:\n      value: Thanks!\n  - type: input\n    id: version\n    attributes:\n      label: Version\n    validations:\n      required: true\n  - type: dropdown\n    attributes:\n      label: OS\n      options: [Linux, macOS]\n";
        let t = parse_form(".github/ISSUE_TEMPLATE/bug.yml", ok).unwrap();
        assert_eq!(t.kind, "form");
        assert_eq!(t.form.as_ref().unwrap().len(), 3);
        assert_eq!(t.form.as_ref().unwrap()[1]["validations"]["required"], true);
        let dup = "name: Bug\ndescription: d\nbody:\n  - type: input\n    id: a\n    attributes: {label: A}\n  - type: input\n    id: a\n    attributes: {label: B}\n";
        assert!(
            parse_form("x.yml", dup)
                .unwrap_err()
                .contains("duplicate id")
        );
        assert!(parse_form("x.yml", "name: x\nbody: []").is_err());
    }

    #[test]
    fn config() {
        let c = parse_config("blank_issues_enabled: false\ncontact_links:\n  - name: Forum\n    url: https://example.com\n    about: Ask here\n").unwrap();
        assert!(!c.blank_issues_enabled);
        assert_eq!(c.contact_links[0].name, "Forum");
    }
}
