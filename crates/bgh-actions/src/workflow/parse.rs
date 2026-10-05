//! YAML -> [`Workflow`] conversion with validation.

use indexmap::IndexMap;
use serde_json::Value as Json;
use serde_yaml::{Mapping, Value as Yaml};

use super::WorkflowError;
use super::cron::CronSchedule;
use super::model::{
    Concurrency, Container, Defaults, EventTrigger, InputDef, Job, Permissions, RunDefaults, Step,
    Strategy, Triggers, Workflow,
};
use super::on::KNOWN_EVENTS;

type Result<T> = std::result::Result<T, WorkflowError>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(WorkflowError::invalid(msg))
}

const TOP_LEVEL_KEYS: &[&str] = &[
    "name",
    "run-name",
    "on",
    "env",
    "defaults",
    "concurrency",
    "permissions",
    "jobs",
];

const JOB_KEYS: &[&str] = &[
    "name",
    "needs",
    "if",
    "runs-on",
    "environment",
    "permissions",
    "concurrency",
    "outputs",
    "env",
    "defaults",
    "timeout-minutes",
    "continue-on-error",
    "strategy",
    "container",
    "services",
    "steps",
    "uses",
    "with",
    "secrets",
];

/// Keys allowed on a job that calls a reusable workflow.
const REUSABLE_JOB_KEYS: &[&str] = &[
    "name",
    "uses",
    "with",
    "secrets",
    "strategy",
    "needs",
    "if",
    "permissions",
    "concurrency",
];

const STEP_KEYS: &[&str] = &[
    "id",
    "name",
    "if",
    "uses",
    "run",
    "shell",
    "working-directory",
    "with",
    "env",
    "continue-on-error",
    "timeout-minutes",
];

/// Parses and validates a workflow file.
pub fn parse_workflow(yaml: &str) -> Result<Workflow> {
    let mut doc: Yaml =
        serde_yaml::from_str(yaml).map_err(|e| WorkflowError::Yaml(e.to_string()))?;
    doc.apply_merge()
        .map_err(|e| WorkflowError::Yaml(e.to_string()))?;
    let doc = strip_tags(doc);
    let map = match doc {
        Yaml::Mapping(m) => m,
        Yaml::Null => return invalid("workflow file is empty"),
        other => {
            return invalid(format!(
                "workflow must be a mapping, found {}",
                kind(&other)
            ));
        }
    };

    let mut wf = Workflow::default();
    let mut seen_on = false;
    let mut jobs: Option<&Yaml> = None;
    for (k, v) in &map {
        let key = match k {
            Yaml::String(s) => s.as_str(),
            // YAML 1.1 parsers read a bare `on` key as boolean true.
            Yaml::Bool(true) => "on",
            other => return invalid(format!("unexpected top-level key {}", describe_key(other))),
        };
        match key {
            "name" => wf.name = opt_string(v, "name")?,
            "run-name" => wf.run_name = opt_string(v, "run-name")?,
            "on" => {
                if seen_on {
                    return invalid("'on' is defined more than once");
                }
                seen_on = true;
                wf.on = parse_on(v)?;
            }
            "env" => wf.env = string_map(v, "env")?,
            "defaults" => wf.defaults = opt(v, |v| parse_defaults(v, "defaults"))?,
            "concurrency" => wf.concurrency = opt(v, |v| parse_concurrency(v, "concurrency"))?,
            "permissions" => wf.permissions = Some(parse_permissions(v, "permissions")?),
            "jobs" => jobs = Some(v),
            other => {
                return invalid(format!(
                    "unexpected top-level key '{other}' (allowed: {})",
                    TOP_LEVEL_KEYS.join(", ")
                ));
            }
        }
    }
    if !seen_on {
        return invalid("missing required key 'on'");
    }
    let Some(jobs) = jobs else {
        return invalid("missing required key 'jobs'");
    };
    wf.jobs = parse_jobs(jobs)?;
    validate_needs(&wf)?;
    Ok(wf)
}

// ---------------------------------------------------------------------------
// generic helpers

fn strip_tags(v: Yaml) -> Yaml {
    match v {
        Yaml::Tagged(t) => strip_tags(t.value),
        Yaml::Sequence(s) => Yaml::Sequence(s.into_iter().map(strip_tags).collect()),
        Yaml::Mapping(m) => Yaml::Mapping(
            m.into_iter()
                .map(|(k, v)| (strip_tags(k), strip_tags(v)))
                .collect(),
        ),
        other => other,
    }
}

fn kind(v: &Yaml) -> &'static str {
    match v {
        Yaml::Null => "null",
        Yaml::Bool(_) => "a boolean",
        Yaml::Number(_) => "a number",
        Yaml::String(_) => "a string",
        Yaml::Sequence(_) => "a sequence",
        Yaml::Mapping(_) => "a mapping",
        Yaml::Tagged(_) => "a tagged value",
    }
}

fn describe_key(v: &Yaml) -> String {
    match scalar(v) {
        Some(s) => format!("'{s}'"),
        None => kind(v).to_string(),
    }
}

/// Stringifies a scalar like GitHub: `true`, `3`, `1.5`; null -> "".
fn scalar(v: &Yaml) -> Option<String> {
    match v {
        Yaml::Null => Some(String::new()),
        Yaml::Bool(b) => Some(b.to_string()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn string(v: &Yaml, ctx: &str) -> Result<String> {
    match scalar(v) {
        Some(s) => Ok(s),
        None => invalid(format!("{ctx}: expected a string, found {}", kind(v))),
    }
}

/// Optional scalar: null -> None.
fn opt_string(v: &Yaml, ctx: &str) -> Result<Option<String>> {
    if v.is_null() {
        Ok(None)
    } else {
        string(v, ctx).map(Some)
    }
}

fn opt<T>(v: &Yaml, f: impl FnOnce(&Yaml) -> Result<T>) -> Result<Option<T>> {
    if v.is_null() {
        Ok(None)
    } else {
        f(v).map(Some)
    }
}

fn map_key(k: &Yaml, ctx: &str) -> Result<String> {
    match scalar(k) {
        Some(s) => Ok(s),
        None => invalid(format!("{ctx}: keys must be strings, found {}", kind(k))),
    }
}

fn mapping<'a>(v: &'a Yaml, ctx: &str) -> Result<Option<&'a Mapping>> {
    match v {
        Yaml::Null => Ok(None),
        Yaml::Mapping(m) => Ok(Some(m)),
        other => invalid(format!("{ctx}: expected a mapping, found {}", kind(other))),
    }
}

/// Mapping of scalars (env, with, outputs). Null -> empty.
fn string_map(v: &Yaml, ctx: &str) -> Result<IndexMap<String, String>> {
    let mut out = IndexMap::new();
    if let Some(m) = mapping(v, ctx)? {
        for (k, val) in m {
            let key = map_key(k, ctx)?;
            let s = string(val, &format!("{ctx}.{key}"))?;
            out.insert(key, s);
        }
    }
    Ok(out)
}

/// Mapping of arbitrary values kept raw as JSON. Null -> empty.
fn json_map(v: &Yaml, ctx: &str) -> Result<IndexMap<String, Json>> {
    let mut out = IndexMap::new();
    if let Some(m) = mapping(v, ctx)? {
        for (k, val) in m {
            out.insert(map_key(k, ctx)?, to_json(val));
        }
    }
    Ok(out)
}

/// A string or a sequence of scalars. Null -> empty.
fn string_list(v: &Yaml, ctx: &str) -> Result<Vec<String>> {
    match v {
        Yaml::Null => Ok(Vec::new()),
        Yaml::Sequence(seq) => seq
            .iter()
            .enumerate()
            .map(|(i, item)| string(item, &format!("{ctx}[{i}]")))
            .collect(),
        other => match scalar(other) {
            Some(s) => Ok(vec![s]),
            None => invalid(format!(
                "{ctx}: expected a string or a list, found {}",
                kind(other)
            )),
        },
    }
}

/// Converts YAML to raw JSON (keys stringified, tags dropped).
pub(crate) fn to_json(v: &Yaml) -> Json {
    match v {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(*b),
        Yaml::Number(n) => {
            if let Some(i) = n.as_i64() {
                Json::from(i)
            } else if let Some(u) = n.as_u64() {
                Json::from(u)
            } else {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map_or_else(|| Json::String(n.to_string()), Json::Number)
            }
        }
        Yaml::String(s) => Json::String(s.clone()),
        Yaml::Sequence(seq) => Json::Array(seq.iter().map(to_json).collect()),
        Yaml::Mapping(m) => Json::Object(
            m.iter()
                .map(|(k, v)| (scalar(k).unwrap_or_else(|| kind(k).to_string()), to_json(v)))
                .collect(),
        ),
        Yaml::Tagged(t) => to_json(&t.value),
    }
}

/// A bool, or an expression string (`continue-on-error`, `cancel-in-progress`, `fail-fast`).
fn bool_or_expr(v: &Yaml, ctx: &str) -> Result<Option<Json>> {
    match v {
        Yaml::Null => Ok(None),
        Yaml::Bool(_) | Yaml::String(_) => Ok(Some(to_json(v))),
        other => invalid(format!(
            "{ctx}: expected a boolean or expression, found {}",
            kind(other)
        )),
    }
}

/// A number, or an expression string (`timeout-minutes`, `max-parallel`).
fn number_or_expr(v: &Yaml, ctx: &str) -> Result<Option<Json>> {
    match v {
        Yaml::Null => Ok(None),
        Yaml::Number(_) | Yaml::String(_) => Ok(Some(to_json(v))),
        other => invalid(format!(
            "{ctx}: expected a number or expression, found {}",
            kind(other)
        )),
    }
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ---------------------------------------------------------------------------
// on:

fn parse_on(v: &Yaml) -> Result<Triggers> {
    let mut events: IndexMap<String, EventTrigger> = IndexMap::new();
    let mut add = |name: String, trigger: EventTrigger| -> Result<()> {
        let lower = name.to_ascii_lowercase();
        if !KNOWN_EVENTS.contains(&lower.as_str()) {
            return invalid(format!("on: unknown event '{name}'"));
        }
        if events.insert(lower, trigger).is_some() {
            return invalid(format!("on: event '{name}' is listed more than once"));
        }
        Ok(())
    };
    match v {
        Yaml::String(s) => add(s.clone(), EventTrigger::default())?,
        Yaml::Sequence(seq) => {
            for (i, item) in seq.iter().enumerate() {
                match item {
                    Yaml::String(s) => add(s.clone(), EventTrigger::default())?,
                    other => {
                        return invalid(format!(
                            "on[{i}]: expected an event name, found {}",
                            kind(other)
                        ));
                    }
                }
            }
        }
        Yaml::Mapping(m) => {
            for (k, cfg) in m {
                let name = map_key(k, "on")?;
                let trigger = parse_event(&name.to_ascii_lowercase(), cfg)?;
                add(name, trigger)?;
            }
        }
        other => {
            return invalid(format!(
                "on: expected an event name, list or mapping, found {}",
                kind(other)
            ));
        }
    }
    if events.is_empty() {
        return invalid("on: at least one event is required");
    }
    Ok(Triggers { events })
}

fn parse_event(event: &str, cfg: &Yaml) -> Result<EventTrigger> {
    let ctx = format!("on.{event}");
    let mut t = EventTrigger::default();
    if event == "schedule" {
        let Yaml::Sequence(seq) = cfg else {
            return invalid(format!("{ctx}: expected a list of {{cron: ...}} entries"));
        };
        for (i, item) in seq.iter().enumerate() {
            let cron = mapping(item, &ctx)?
                .and_then(|m| m.get("cron"))
                .ok_or_else(|| WorkflowError::invalid(format!("{ctx}[{i}]: missing 'cron'")))?;
            let cron = string(cron, &format!("{ctx}[{i}].cron"))?;
            CronSchedule::parse(&cron)?;
            t.crons.push(cron);
        }
        if t.crons.is_empty() {
            return invalid(format!("{ctx}: at least one cron entry is required"));
        }
        return Ok(t);
    }
    let Some(m) = mapping(cfg, &ctx)? else {
        return Ok(t);
    };
    for (k, v) in m {
        let key = map_key(k, &ctx)?;
        let kctx = format!("{ctx}.{key}");
        match key.as_str() {
            "types" => t.types = string_list(v, &kctx)?,
            "branches" => t.branches = Some(string_list(v, &kctx)?),
            "branches-ignore" => t.branches_ignore = Some(string_list(v, &kctx)?),
            "tags" => t.tags = Some(string_list(v, &kctx)?),
            "tags-ignore" => t.tags_ignore = Some(string_list(v, &kctx)?),
            "paths" => t.paths = Some(string_list(v, &kctx)?),
            "paths-ignore" => t.paths_ignore = Some(string_list(v, &kctx)?),
            "inputs" => t.inputs = parse_inputs(event, v, &kctx)?,
            "outputs" => t.outputs = json_map(v, &kctx)?,
            "secrets" => t.secrets = json_map(v, &kctx)?,
            "workflows" => t.workflows = string_list(v, &kctx)?,
            // Unknown event options are tolerated.
            _ => {}
        }
    }
    for (a, b, name) in [
        (&t.branches, &t.branches_ignore, "branches"),
        (&t.tags, &t.tags_ignore, "tags"),
        (&t.paths, &t.paths_ignore, "paths"),
    ] {
        if a.is_some() && b.is_some() {
            return invalid(format!(
                "{ctx}: '{name}' and '{name}-ignore' cannot be used together"
            ));
        }
    }
    Ok(t)
}

fn parse_inputs(event: &str, v: &Yaml, ctx: &str) -> Result<IndexMap<String, InputDef>> {
    let allowed: &[&str] = if event == "workflow_dispatch" {
        &["string", "boolean", "choice", "number", "environment"]
    } else {
        &["string", "boolean", "number"]
    };
    let mut out = IndexMap::new();
    let Some(m) = mapping(v, ctx)? else {
        return Ok(out);
    };
    for (k, def) in m {
        let name = map_key(k, ctx)?;
        let ictx = format!("{ctx}.{name}");
        let mut input = InputDef::default();
        if let Some(dm) = mapping(def, &ictx)? {
            for (dk, dv) in dm {
                let dkey = map_key(dk, &ictx)?;
                let dctx = format!("{ictx}.{dkey}");
                match dkey.as_str() {
                    "description" => input.description = opt_string(dv, &dctx)?,
                    "required" => {
                        input.required = match dv {
                            Yaml::Bool(b) => *b,
                            Yaml::Null => false,
                            Yaml::String(s) if s == "true" || s == "false" => s == "true",
                            other => {
                                return invalid(format!(
                                    "{dctx}: expected a boolean, found {}",
                                    kind(other)
                                ));
                            }
                        }
                    }
                    "default" => input.default = opt_string(dv, &dctx)?,
                    "type" => input.r#type = string(dv, &dctx)?,
                    "options" => input.options = string_list(dv, &dctx)?,
                    _ => {}
                }
            }
        }
        if !allowed.contains(&input.r#type.as_str()) {
            return invalid(format!(
                "{ictx}: invalid input type '{}' (allowed: {})",
                input.r#type,
                allowed.join(", ")
            ));
        }
        match input.r#type.as_str() {
            "choice" => {
                if input.options.is_empty() {
                    return invalid(format!("{ictx}: a choice input requires 'options'"));
                }
                if let Some(d) = &input.default
                    && !input.options.contains(d)
                {
                    return invalid(format!("{ictx}: default '{d}' is not one of the options"));
                }
            }
            "boolean" => {
                if let Some(d) = &input.default
                    && d != "true"
                    && d != "false"
                    && !d.contains("${{")
                {
                    return invalid(format!(
                        "{ictx}: default '{d}' of a boolean input must be true or false"
                    ));
                }
            }
            _ => {}
        }
        out.insert(name, input);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// shared sections

fn parse_defaults(v: &Yaml, ctx: &str) -> Result<Defaults> {
    let mut d = Defaults::default();
    let Some(m) = mapping(v, ctx)? else {
        return Ok(d);
    };
    for (k, val) in m {
        let key = map_key(k, ctx)?;
        if key != "run" {
            return invalid(format!("{ctx}: unexpected key '{key}'"));
        }
        let rctx = format!("{ctx}.run");
        let mut run = RunDefaults::default();
        if let Some(rm) = mapping(val, &rctx)? {
            for (rk, rv) in rm {
                let rkey = map_key(rk, &rctx)?;
                match rkey.as_str() {
                    "shell" => run.shell = opt_string(rv, &format!("{rctx}.shell"))?,
                    "working-directory" => {
                        run.working_directory =
                            opt_string(rv, &format!("{rctx}.working-directory"))?
                    }
                    other => return invalid(format!("{rctx}: unexpected key '{other}'")),
                }
            }
        }
        d.run = Some(run);
    }
    Ok(d)
}

fn parse_concurrency(v: &Yaml, ctx: &str) -> Result<Concurrency> {
    if let Yaml::Mapping(m) = v {
        let mut c = Concurrency::default();
        let mut has_group = false;
        for (k, val) in m {
            let key = map_key(k, ctx)?;
            match key.as_str() {
                "group" => {
                    c.group = string(val, &format!("{ctx}.group"))?;
                    has_group = true;
                }
                "cancel-in-progress" => {
                    c.cancel_in_progress = bool_or_expr(val, &format!("{ctx}.cancel-in-progress"))?
                }
                other => return invalid(format!("{ctx}: unexpected key '{other}'")),
            }
        }
        if !has_group || c.group.is_empty() {
            return invalid(format!("{ctx}: 'group' is required"));
        }
        Ok(c)
    } else {
        Ok(Concurrency {
            group: string(v, ctx)?,
            cancel_in_progress: None,
        })
    }
}

fn parse_permissions(v: &Yaml, ctx: &str) -> Result<Permissions> {
    match v {
        Yaml::String(s) if s == "read-all" => Ok(Permissions::ReadAll),
        Yaml::String(s) if s == "write-all" => Ok(Permissions::WriteAll),
        Yaml::Null => Ok(Permissions::Map(IndexMap::new())),
        Yaml::Mapping(m) => {
            let mut out = IndexMap::new();
            for (k, val) in m {
                let scope = map_key(k, ctx)?;
                let level = string(val, &format!("{ctx}.{scope}"))?;
                if !matches!(level.as_str(), "read" | "write" | "none") {
                    return invalid(format!(
                        "{ctx}.{scope}: invalid permission '{level}' (expected read, write or none)"
                    ));
                }
                out.insert(scope, level);
            }
            Ok(Permissions::Map(out))
        }
        other => invalid(format!(
            "{ctx}: expected 'read-all', 'write-all' or a mapping, found {}",
            match scalar(other) {
                Some(s) => format!("'{s}'"),
                None => kind(other).to_string(),
            }
        )),
    }
}

fn parse_container(v: &Yaml, ctx: &str) -> Result<Container> {
    let m = match v {
        Yaml::String(s) => {
            return Ok(Container {
                image: s.clone(),
                ..Container::default()
            });
        }
        Yaml::Mapping(m) => m,
        other => {
            return invalid(format!(
                "{ctx}: expected an image name or a mapping, found {}",
                kind(other)
            ));
        }
    };
    let mut c = Container::default();
    let mut has_image = false;
    for (k, val) in m {
        let key = map_key(k, ctx)?;
        let kctx = format!("{ctx}.{key}");
        match key.as_str() {
            "image" => {
                c.image = string(val, &kctx)?;
                has_image = true;
            }
            "credentials" => c.credentials = opt(val, |v| Ok(to_json(v)))?,
            "env" => c.env = string_map(val, &kctx)?,
            "ports" => c.ports = string_list(val, &kctx)?,
            "volumes" => c.volumes = string_list(val, &kctx)?,
            "options" => c.options = opt_string(val, &kctx)?,
            other => return invalid(format!("{ctx}: unexpected key '{other}'")),
        }
    }
    if !has_image {
        return invalid(format!("{ctx}: 'image' is required"));
    }
    Ok(c)
}

fn parse_strategy(v: &Yaml, ctx: &str) -> Result<Strategy> {
    let mut s = Strategy::default();
    let Some(m) = mapping(v, ctx)? else {
        return Ok(s);
    };
    for (k, val) in m {
        let key = map_key(k, ctx)?;
        let kctx = format!("{ctx}.{key}");
        match key.as_str() {
            "matrix" => s.matrix = opt(val, |v| parse_matrix(v, &kctx))?,
            "fail-fast" => s.fail_fast = bool_or_expr(val, &kctx)?,
            "max-parallel" => s.max_parallel = number_or_expr(val, &kctx)?,
            other => return invalid(format!("{ctx}: unexpected key '{other}'")),
        }
    }
    Ok(s)
}

fn parse_matrix(v: &Yaml, ctx: &str) -> Result<Json> {
    match v {
        Yaml::String(_) => Ok(to_json(v)),
        Yaml::Mapping(m) => {
            for (k, val) in m {
                let key = map_key(k, ctx)?;
                let ok = matches!(val, Yaml::Sequence(_) | Yaml::String(_));
                if !ok {
                    return invalid(format!(
                        "{ctx}.{key}: expected an array or expression, found {}",
                        kind(val)
                    ));
                }
                if let Yaml::Sequence(seq) = val
                    && (key == "include" || key == "exclude")
                    && let Some(i) = seq.iter().position(|e| !e.is_mapping())
                {
                    return invalid(format!("{ctx}.{key}[{i}]: expected a mapping"));
                }
            }
            Ok(to_json(v))
        }
        other => invalid(format!(
            "{ctx}: expected a mapping or expression, found {}",
            kind(other)
        )),
    }
}

// ---------------------------------------------------------------------------
// jobs

fn parse_jobs(v: &Yaml) -> Result<IndexMap<String, Job>> {
    let m = match v {
        Yaml::Mapping(m) => m,
        Yaml::Null => return invalid("'jobs' must contain at least one job"),
        other => return invalid(format!("jobs: expected a mapping, found {}", kind(other))),
    };
    if m.is_empty() {
        return invalid("'jobs' must contain at least one job");
    }
    let mut jobs = IndexMap::new();
    for (k, val) in m {
        let id = map_key(k, "jobs")?;
        if !is_valid_id(&id) {
            return invalid(format!(
                "invalid job id '{id}': must start with a letter or '_' and contain only alphanumeric characters, '-' or '_'"
            ));
        }
        let job = parse_job(&id, val)?;
        jobs.insert(id, job);
    }
    Ok(jobs)
}

fn parse_job(id: &str, v: &Yaml) -> Result<Job> {
    let ctx = format!("job '{id}'");
    let m = match v {
        Yaml::Mapping(m) => m,
        other => return invalid(format!("{ctx}: expected a mapping, found {}", kind(other))),
    };
    let mut job = Job::default();
    let mut keys: Vec<String> = Vec::new();
    let mut steps: Option<&Yaml> = None;
    for (k, val) in m {
        let key = map_key(k, &ctx)?;
        let kctx = format!("{ctx}: {key}");
        match key.as_str() {
            "name" => job.name = opt_string(val, &kctx)?,
            "needs" => job.needs = string_list(val, &kctx)?,
            "if" => job.r#if = opt_string(val, &kctx)?,
            "runs-on" => {
                if !matches!(val, Yaml::String(_) | Yaml::Sequence(_) | Yaml::Mapping(_)) {
                    return invalid(format!(
                        "{kctx}: expected a label, list of labels or {{group, labels}} mapping, found {}",
                        kind(val)
                    ));
                }
                job.runs_on = to_json(val);
            }
            "environment" => job.environment = opt(val, |v| Ok(to_json(v)))?,
            "permissions" => job.permissions = Some(parse_permissions(val, &kctx)?),
            "concurrency" => job.concurrency = opt(val, |v| parse_concurrency(v, &kctx))?,
            "outputs" => job.outputs = string_map(val, &kctx)?,
            "env" => job.env = string_map(val, &kctx)?,
            "defaults" => job.defaults = opt(val, |v| parse_defaults(v, &kctx))?,
            "timeout-minutes" => job.timeout_minutes = number_or_expr(val, &kctx)?,
            "continue-on-error" => job.continue_on_error = bool_or_expr(val, &kctx)?,
            "strategy" => job.strategy = opt(val, |v| parse_strategy(v, &kctx))?,
            "container" => job.container = opt(val, |v| parse_container(v, &kctx))?,
            "services" => {
                if let Some(sm) = mapping(val, &kctx)? {
                    for (sk, sv) in sm {
                        let name = map_key(sk, &kctx)?;
                        let svc = parse_container(sv, &format!("{ctx}: services.{name}"))?;
                        job.services.insert(name, svc);
                    }
                }
            }
            "steps" => steps = Some(val),
            "uses" => job.uses = Some(string(val, &kctx)?),
            "with" => job.with = string_map(val, &kctx)?,
            "secrets" => job.secrets = opt(val, |v| Ok(to_json(v)))?,
            other => {
                return invalid(format!(
                    "{ctx}: unexpected key '{other}' (allowed: {})",
                    JOB_KEYS.join(", ")
                ));
            }
        }
        keys.push(key);
    }

    if job.uses.is_some() {
        if let Some(bad) = keys
            .iter()
            .find(|k| !REUSABLE_JOB_KEYS.contains(&k.as_str()))
        {
            return invalid(format!(
                "{ctx}: '{bad}' cannot be used in a job that calls a reusable workflow with 'uses'"
            ));
        }
        return Ok(job);
    }
    for key in ["with", "secrets"] {
        if keys.iter().any(|k| k == key) {
            return invalid(format!(
                "{ctx}: '{key}' is only allowed in a job that calls a reusable workflow with 'uses'"
            ));
        }
    }
    let Some(steps) = steps else {
        return invalid(format!("{ctx}: a job must define either 'steps' or 'uses'"));
    };
    if !keys.iter().any(|k| k == "runs-on") {
        return invalid(format!("{ctx}: 'runs-on' is required"));
    }
    let seq = match steps {
        Yaml::Sequence(seq) if !seq.is_empty() => seq,
        Yaml::Sequence(_) | Yaml::Null => {
            return invalid(format!("{ctx}: 'steps' must contain at least one step"));
        }
        other => {
            return invalid(format!(
                "{ctx}: steps: expected a list, found {}",
                kind(other)
            ));
        }
    };
    for (i, sv) in seq.iter().enumerate() {
        let step = parse_step(&ctx, i, sv)?;
        if let Some(sid) = &step.id
            && job.steps.iter().any(|s| s.id.as_deref() == Some(sid))
        {
            return invalid(format!(
                "{ctx}: step {} uses duplicate step id '{sid}'",
                i + 1
            ));
        }
        job.steps.push(step);
    }
    Ok(job)
}

fn parse_step(job_ctx: &str, index: usize, v: &Yaml) -> Result<Step> {
    let m = match v {
        Yaml::Mapping(m) => m,
        other => {
            return invalid(format!(
                "{job_ctx}: step {}: expected a mapping, found {}",
                index + 1,
                kind(other)
            ));
        }
    };
    // Name the step by id or name in messages when available.
    let label = ["id", "name"]
        .iter()
        .find_map(|k| m.get(*k).and_then(scalar))
        .map_or_else(
            || format!("step {}", index + 1),
            |l| format!("step {} ('{l}')", index + 1),
        );
    let ctx = format!("{job_ctx}: {label}");
    let mut s = Step::default();
    for (k, val) in m {
        let key = map_key(k, &ctx)?;
        let kctx = format!("{ctx}: {key}");
        match key.as_str() {
            "id" => s.id = opt_string(val, &kctx)?,
            "name" => s.name = opt_string(val, &kctx)?,
            "if" => s.r#if = opt_string(val, &kctx)?,
            "uses" => s.uses = Some(string(val, &kctx)?),
            "run" => s.run = Some(string(val, &kctx)?),
            "shell" => s.shell = opt_string(val, &kctx)?,
            "working-directory" => s.working_directory = opt_string(val, &kctx)?,
            "with" => s.with = string_map(val, &kctx)?,
            "env" => s.env = string_map(val, &kctx)?,
            "continue-on-error" => s.continue_on_error = bool_or_expr(val, &kctx)?,
            "timeout-minutes" => s.timeout_minutes = number_or_expr(val, &kctx)?,
            other => {
                return invalid(format!(
                    "{ctx}: unexpected key '{other}' (allowed: {})",
                    STEP_KEYS.join(", ")
                ));
            }
        }
    }
    match (&s.run, &s.uses) {
        (Some(_), Some(_)) => return invalid(format!("{ctx}: cannot have both 'run' and 'uses'")),
        (None, None) => return invalid(format!("{ctx}: must have either 'run' or 'uses'")),
        _ => {}
    }
    if let Some(id) = &s.id
        && !is_valid_id(id)
    {
        return invalid(format!(
            "{ctx}: invalid step id '{id}': must start with a letter or '_' and contain only alphanumeric characters, '-' or '_'"
        ));
    }
    if s.uses.is_some() && (s.shell.is_some() || s.working_directory.is_some()) {
        return invalid(format!(
            "{ctx}: 'shell' and 'working-directory' are only allowed with 'run'"
        ));
    }
    Ok(s)
}

fn validate_needs(wf: &Workflow) -> Result<()> {
    for (id, job) in &wf.jobs {
        for need in &job.needs {
            if !wf.jobs.contains_key(need) {
                return invalid(format!("job '{id}' depends on unknown job '{need}'"));
            }
            if need == id {
                return invalid(format!("job '{id}' depends on itself"));
            }
        }
    }
    if let Some(cycle) = wf.find_needs_cycle() {
        return invalid(format!(
            "jobs form a dependency cycle: {}",
            cycle.join(" -> ")
        ));
    }
    Ok(())
}
