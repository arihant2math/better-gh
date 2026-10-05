//! Parser, validation and trigger-matching tests.

use serde_json::json;

use super::*;

fn parse(yaml: &str) -> Workflow {
    match parse_workflow(yaml) {
        Ok(wf) => wf,
        Err(e) => panic!("parse failed: {e}\n---\n{yaml}"),
    }
}

fn parse_err(yaml: &str) -> String {
    match parse_workflow(yaml) {
        Ok(_) => panic!("expected an error for:\n{yaml}"),
        Err(e) => e.to_string(),
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Wraps a jobs section into a minimal valid workflow.
fn with_jobs(jobs: &str) -> String {
    format!("on: push\njobs:\n{jobs}")
}

/// Wraps an `on:` section into a minimal valid workflow.
fn with_on(on: &str) -> String {
    format!("{on}\njobs:\n  j:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo\n")
}

const RUST_CI: &str = r#"
name: CI
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1
  RUSTFLAGS: "-D warnings"

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

permissions:
  contents: read

jobs:
  fmt:
    name: Rustfmt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt
      - run: cargo fmt --all -- --check

  test:
    name: Test (${{ matrix.os }}, ${{ matrix.rust }})
    needs: fmt
    runs-on: ${{ matrix.os }}
    timeout-minutes: 30
    strategy:
      fail-fast: false
      max-parallel: 4
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        rust: [stable, beta]
        include:
          - os: ubuntu-latest
            rust: nightly
            experimental: true
        exclude:
          - os: windows-latest
            rust: beta
    continue-on-error: ${{ matrix.experimental == true }}
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_PASSWORD: postgres
        ports:
          - 5432:5432
          - 6000
        options: >-
          --health-cmd pg_isready
          --health-interval 10s
      redis: redis:7
    env:
      DATABASE_URL: postgres://postgres:postgres@localhost/test
    steps:
      - uses: actions/checkout@v4
      - name: Cache
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            target
          key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
      - name: Build
        id: build
        run: cargo build --workspace --all-targets
      - name: Test
        run: |
          cargo test --workspace
          echo "done"
        env:
          RUST_LOG: debug
          THREADS: 4
          VERBOSE: true

  clippy:
    needs: [fmt]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo clippy --workspace --all-targets -- -D warnings

  done:
    needs: [test, clippy]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
"#;

const RELEASE: &str = r#"
name: Release
run-name: Release ${{ github.ref_name }} by @${{ github.actor }}
on:
  push:
    tags:
      - "v[0-9]+.[0-9]+.[0-9]+"
      - "!v*-rc*"
permissions: write-all
jobs:
  build:
    runs-on: [self-hosted, linux, x64]
    container:
      image: ghcr.io/acme/builder:1.2
      credentials:
        username: ${{ github.actor }}
        password: ${{ secrets.GITHUB_TOKEN }}
      env:
        CI: true
      volumes:
        - /cache:/cache
      options: --cpus 2
    outputs:
      artifact: ${{ steps.pkg.outputs.path }}
    steps:
      - uses: actions/checkout@v4
      - id: pkg
        run: ./package.sh
        shell: bash
        working-directory: dist
  publish:
    needs: build
    runs-on:
      group: release-runners
      labels: [linux]
    environment:
      name: production
      url: https://example.com/releases/${{ github.ref_name }}
    steps:
      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*
          draft: false
          prerelease: ${{ contains(github.ref, '-') }}
"#;

const DISPATCH: &str = r#"
name: Manual deploy
on:
  workflow_dispatch:
    inputs:
      environment:
        description: Target environment
        type: choice
        required: true
        default: staging
        options:
          - staging
          - production
      dry_run:
        description: Dry run only
        type: boolean
        default: true
      replicas:
        type: number
        default: 3
      target:
        type: environment
      note:
        description: Free text
defaults:
  run:
    shell: bash
    working-directory: ./deploy
jobs:
  deploy:
    runs-on: ubuntu-latest
    environment: ${{ inputs.environment }}
    steps:
      - run: ./deploy.sh ${{ inputs.environment }} ${{ inputs.dry_run }}
"#;

// ---------------------------------------------------------------------------
// real-world workflows

#[test]
fn rust_ci_top_level() {
    let wf = parse(RUST_CI);
    assert_eq!(wf.name.as_deref(), Some("CI"));
    assert_eq!(
        wf.on.events.keys().collect::<Vec<_>>(),
        ["push", "pull_request"]
    );
    assert_eq!(wf.on.get("push").unwrap().branches, Some(s(&["main"])));
    assert_eq!(
        wf.env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        [
            ("CARGO_TERM_COLOR", "always"),
            ("RUST_BACKTRACE", "1"),
            ("RUSTFLAGS", "-D warnings")
        ]
    );
    let c = wf.concurrency.as_ref().unwrap();
    assert_eq!(c.group, "ci-${{ github.ref }}");
    assert_eq!(c.cancel_in_progress, Some(json!(true)));
    assert_eq!(
        wf.permissions,
        Some(Permissions::Map(
            [("contents".to_string(), "read".to_string())].into()
        ))
    );
    assert_eq!(
        wf.jobs.keys().collect::<Vec<_>>(),
        ["fmt", "test", "clippy", "done"]
    );
}

#[test]
fn rust_ci_matrix_job() {
    let wf = parse(RUST_CI);
    let test = &wf.jobs["test"];
    assert_eq!(test.needs, s(&["fmt"]));
    assert_eq!(test.runs_on, json!("${{ matrix.os }}"));
    assert_eq!(test.timeout_minutes, Some(json!(30)));
    assert_eq!(
        test.continue_on_error,
        Some(json!("${{ matrix.experimental == true }}"))
    );
    let strategy = test.strategy.as_ref().unwrap();
    assert_eq!(strategy.fail_fast, Some(json!(false)));
    assert_eq!(strategy.max_parallel, Some(json!(4)));
    let matrix = strategy.matrix.as_ref().unwrap();
    assert_eq!(
        matrix.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["os", "rust", "include", "exclude"]
    );
    let combos = expand_matrix(matrix).unwrap();
    // 3*2 - 1 excluded + 1 included nightly
    assert_eq!(combos.len(), 6);
    assert_eq!(
        serde_json::Value::Object(combos[5].clone()),
        json!({"os": "ubuntu-latest", "rust": "nightly", "experimental": true})
    );
}

#[test]
fn rust_ci_services_and_steps() {
    let wf = parse(RUST_CI);
    let test = &wf.jobs["test"];
    let pg = &test.services["postgres"];
    assert_eq!(pg.image, "postgres:16");
    assert_eq!(pg.env["POSTGRES_PASSWORD"], "postgres");
    assert_eq!(pg.ports, s(&["5432:5432", "6000"]));
    assert_eq!(
        pg.options.as_deref(),
        Some("--health-cmd pg_isready --health-interval 10s")
    );
    assert_eq!(test.services["redis"].image, "redis:7");
    assert_eq!(test.steps.len(), 4);
    assert_eq!(test.steps[1].with["path"], "~/.cargo/registry\ntarget\n");
    assert_eq!(test.steps[2].id.as_deref(), Some("build"));
    assert_eq!(
        test.steps[3].run.as_deref(),
        Some("cargo test --workspace\necho \"done\"\n")
    );
    assert_eq!(test.steps[3].env["THREADS"], "4");
    assert_eq!(test.steps[3].env["VERBOSE"], "true");
    assert_eq!(wf.jobs["done"].r#if.as_deref(), Some("always()"));
}

#[test]
fn rust_ci_job_order_and_triggers() {
    let wf = parse(RUST_CI);
    assert_eq!(wf.job_order(), s(&["fmt", "test", "clippy", "done"]));
    assert!(wf.on.matches_push("refs/heads/main", None));
    assert!(!wf.on.matches_push("refs/heads/dev", None));
    assert!(!wf.on.matches_push("refs/tags/v1.0.0", None));
    assert!(
        wf.on
            .matches_pull_request("pull_request", "opened", "main", None)
    );
    assert!(
        !wf.on
            .matches_pull_request("pull_request", "closed", "main", None)
    );
    assert!(
        !wf.on
            .matches_pull_request("pull_request", "opened", "dev", None)
    );
    assert!(
        !wf.on
            .matches_pull_request("pull_request_target", "opened", "main", None)
    );
}

#[test]
fn release_workflow() {
    let wf = parse(RELEASE);
    assert_eq!(
        wf.run_name.as_deref(),
        Some("Release ${{ github.ref_name }} by @${{ github.actor }}")
    );
    assert_eq!(wf.permissions, Some(Permissions::WriteAll));
    assert!(wf.on.matches_push("refs/tags/v1.2.3", None));
    assert!(wf.on.matches_push("refs/tags/v10.20.30", None));
    assert!(!wf.on.matches_push("refs/tags/v1.2", None));
    assert!(
        !wf.on.matches_push("refs/heads/main", None),
        "tags only: branches don't trigger"
    );

    let build = &wf.jobs["build"];
    assert_eq!(build.runs_on, json!(["self-hosted", "linux", "x64"]));
    let c = build.container.as_ref().unwrap();
    assert_eq!(c.image, "ghcr.io/acme/builder:1.2");
    assert_eq!(
        c.credentials.as_ref().unwrap()["username"],
        json!("${{ github.actor }}")
    );
    assert_eq!(c.env["CI"], "true");
    assert_eq!(c.volumes, s(&["/cache:/cache"]));
    assert_eq!(c.options.as_deref(), Some("--cpus 2"));
    assert_eq!(build.outputs["artifact"], "${{ steps.pkg.outputs.path }}");
    assert_eq!(build.steps[1].shell.as_deref(), Some("bash"));
    assert_eq!(build.steps[1].working_directory.as_deref(), Some("dist"));

    let publish = &wf.jobs["publish"];
    assert_eq!(
        publish.runs_on,
        json!({"group": "release-runners", "labels": ["linux"]})
    );
    assert_eq!(
        publish.environment.as_ref().unwrap()["name"],
        json!("production")
    );
    assert_eq!(publish.steps[0].with["draft"], "false");
    assert_eq!(
        publish.steps[0].with["prerelease"],
        "${{ contains(github.ref, '-') }}"
    );
}

#[test]
fn dispatch_inputs() {
    let wf = parse(DISPATCH);
    let d = wf.on.get("workflow_dispatch").unwrap();
    let names: Vec<_> = d.inputs.keys().collect();
    assert_eq!(
        names,
        ["environment", "dry_run", "replicas", "target", "note"]
    );
    let env = &d.inputs["environment"];
    assert_eq!(env.r#type, "choice");
    assert!(env.required);
    assert_eq!(env.default.as_deref(), Some("staging"));
    assert_eq!(env.options, s(&["staging", "production"]));
    assert_eq!(env.description.as_deref(), Some("Target environment"));
    let dry = &d.inputs["dry_run"];
    assert_eq!(dry.r#type, "boolean");
    assert!(!dry.required);
    assert_eq!(dry.default.as_deref(), Some("true"));
    assert_eq!(d.inputs["replicas"].default.as_deref(), Some("3"));
    assert_eq!(d.inputs["target"].r#type, "environment");
    assert_eq!(d.inputs["note"].r#type, "string");
    let run = wf.defaults.as_ref().unwrap().run.as_ref().unwrap();
    assert_eq!(run.shell.as_deref(), Some("bash"));
    assert_eq!(run.working_directory.as_deref(), Some("./deploy"));
    assert!(wf.on.has("workflow_dispatch"));
    assert!(!wf.on.matches_push("refs/heads/main", None));
}

#[test]
fn reusable_workflow_call_and_definition() {
    let wf = parse(
        r#"
on:
  workflow_call:
    inputs:
      config-path:
        required: true
        type: string
    outputs:
      result:
        description: The result
        value: ${{ jobs.call.outputs.r }}
    secrets:
      token:
        required: true
jobs:
  call:
    uses: octo-org/repo/.github/workflows/build.yml@v1
    with:
      config-path: .github/labeler.yml
      retries: 3
      verbose: true
    secrets: inherit
    strategy:
      matrix:
        target: [a, b]
  local:
    needs: call
    uses: ./.github/workflows/deploy.yml
    secrets:
      token: ${{ secrets.TOKEN }}
"#,
    );
    let wc = wf.on.get("workflow_call").unwrap();
    assert!(wc.inputs["config-path"].required);
    assert_eq!(
        wc.outputs["result"]["value"],
        json!("${{ jobs.call.outputs.r }}")
    );
    assert_eq!(wc.secrets["token"], json!({"required": true}));
    let call = &wf.jobs["call"];
    assert!(call.is_reusable_call());
    assert_eq!(call.runs_on, serde_json::Value::Null);
    assert!(call.steps.is_empty());
    assert_eq!(call.with["retries"], json!(3));
    assert_eq!(call.with["verbose"], json!(true));
    assert_eq!(call.secrets, Some(json!("inherit")));
    assert_eq!(
        wf.jobs["local"].secrets,
        Some(json!({"token": "${{ secrets.TOKEN }}"}))
    );
}

#[test]
fn json_roundtrip() {
    for yaml in [RUST_CI, RELEASE, DISPATCH] {
        let wf = parse(yaml);
        let text = serde_json::to_string(&wf).unwrap();
        let back: Workflow = serde_json::from_str(&text).unwrap();
        assert_eq!(wf, back);
    }
}

#[test]
fn permissions_serialize_like_github() {
    assert_eq!(
        serde_json::to_value(Permissions::ReadAll).unwrap(),
        json!("read-all")
    );
    assert_eq!(
        serde_json::to_value(Permissions::WriteAll).unwrap(),
        json!("write-all")
    );
    let map = Permissions::Map([("issues".to_string(), "write".to_string())].into());
    assert_eq!(
        serde_json::to_value(&map).unwrap(),
        json!({"issues": "write"})
    );
    let back: Permissions = serde_json::from_value(json!({"issues": "write"})).unwrap();
    assert_eq!(back, map);
    let back: Permissions = serde_json::from_value(json!("read-all")).unwrap();
    assert_eq!(back, Permissions::ReadAll);
}

// ---------------------------------------------------------------------------
// YAML edge cases

#[test]
fn on_as_string() {
    let wf = parse(&with_on("on: push"));
    assert_eq!(wf.on.events.keys().collect::<Vec<_>>(), ["push"]);
    assert_eq!(wf.on.events["push"], EventTrigger::default());
}

#[test]
fn on_as_list() {
    let wf = parse(&with_on("on: [push, pull_request, workflow_dispatch]"));
    assert_eq!(
        wf.on.events.keys().collect::<Vec<_>>(),
        ["push", "pull_request", "workflow_dispatch"]
    );
}

#[test]
fn on_map_with_null_values() {
    let wf = parse(&with_on(
        "on:\n  push:\n  workflow_dispatch:\n  issues:\n    types: opened",
    ));
    assert_eq!(wf.on.events.len(), 3);
    assert_eq!(wf.on.events["push"], EventTrigger::default());
    assert_eq!(wf.on.events["issues"].types, s(&["opened"]));
}

#[test]
fn on_event_names_lowercased() {
    let wf = parse(&with_on("on: [Push]"));
    assert!(wf.on.has("push"));
    assert!(wf.on.has("PUSH"));
}

#[test]
fn on_written_as_yaml_boolean_key() {
    // YAML 1.1 style `true:` / explicit boolean key must be read as `on`.
    let wf = parse(&with_on("true: push"));
    assert!(wf.on.has("push"));
    let wf = parse(&with_on("? !!bool true\n: [push]"));
    assert!(wf.on.has("push"));
    // quoted key
    let wf = parse(&with_on("\"on\": push"));
    assert!(wf.on.has("push"));
}

#[test]
fn on_defined_twice_is_error() {
    let e = parse_err(&with_on("on: push\ntrue: pull_request"));
    assert!(e.contains("more than once"), "{e}");
}

#[test]
fn scalars_stringified_in_env_and_with() {
    let wf = parse(
        r#"
on: push
env:
  BOOL: true
  INT: 3
  FLOAT: 1.5
  NULLV:
  NEG: -2
  STR: "007"
jobs:
  j:
    runs-on: ubuntu-latest
    steps:
      - uses: a/b@v1
        with:
          n: 10
          b: false
          e: ""
"#,
    );
    assert_eq!(wf.env["BOOL"], "true");
    assert_eq!(wf.env["INT"], "3");
    assert_eq!(wf.env["FLOAT"], "1.5");
    assert_eq!(wf.env["NULLV"], "");
    assert_eq!(wf.env["NEG"], "-2");
    assert_eq!(wf.env["STR"], "007");
    let with = &wf.jobs["j"].steps[0].with;
    assert_eq!(with["n"], "10");
    assert_eq!(with["b"], "false");
    assert_eq!(with["e"], "");
}

#[test]
fn env_rejects_nested_values() {
    let e = parse_err(&with_jobs(
        "  j:\n    runs-on: x\n    env:\n      A: [1, 2]\n    steps:\n      - run: echo",
    ));
    assert!(e.contains("env.A"), "{e}");
}

#[test]
fn env_order_preserved() {
    let wf = parse(
        "on: push\nenv:\n  Z: 1\n  A: 2\n  M: 3\njobs:\n  j:\n    runs-on: x\n    steps:\n      - run: echo\n",
    );
    assert_eq!(wf.env.keys().collect::<Vec<_>>(), ["Z", "A", "M"]);
}

#[test]
fn multiline_run_block_and_folded() {
    let wf = parse(&with_jobs(
        "  j:\n    runs-on: x\n    steps:\n      - run: |\n          echo one\n          echo two\n      - run: >\n          folded\n          line\n",
    ));
    assert_eq!(
        wf.jobs["j"].steps[0].run.as_deref(),
        Some("echo one\necho two\n")
    );
    assert_eq!(wf.jobs["j"].steps[1].run.as_deref(), Some("folded line\n"));
}

#[test]
fn yaml_anchors() {
    let wf = parse(
        r#"
on: push
jobs:
  a:
    runs-on: &os ubuntu-latest
    steps: &steps
      - run: echo shared
  b:
    runs-on: *os
    steps: *steps
"#,
    );
    assert_eq!(wf.jobs["b"].runs_on, json!("ubuntu-latest"));
    assert_eq!(wf.jobs["b"].steps[0].run.as_deref(), Some("echo shared"));
}

#[test]
fn merge_key_within_jobs() {
    let wf = parse(
        r#"
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    env: &envs
      A: "1"
    steps:
      - run: echo
  b:
    runs-on: ubuntu-latest
    env:
      <<: *envs
      B: "2"
    steps:
      - run: echo
"#,
    );
    assert_eq!(wf.jobs["b"].env["A"], "1");
    assert_eq!(wf.jobs["b"].env["B"], "2");
}

#[test]
fn step_if_and_job_if_bools_stringified() {
    let wf = parse(&with_jobs(
        "  j:\n    if: false\n    runs-on: x\n    steps:\n      - run: echo\n        if: ${{ success() }}\n",
    ));
    assert_eq!(wf.jobs["j"].r#if.as_deref(), Some("false"));
    assert_eq!(
        wf.jobs["j"].steps[0].r#if.as_deref(),
        Some("${{ success() }}")
    );
}

#[test]
fn concurrency_string_form() {
    let wf = parse(
        "on: push\nconcurrency: deploy\njobs:\n  j:\n    runs-on: x\n    concurrency:\n      group: g-${{ github.ref }}\n      cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}\n    steps:\n      - run: echo\n",
    );
    assert_eq!(
        wf.concurrency,
        Some(Concurrency {
            group: "deploy".into(),
            cancel_in_progress: None
        })
    );
    let jc = wf.jobs["j"].concurrency.as_ref().unwrap();
    assert_eq!(
        jc.cancel_in_progress,
        Some(json!("${{ github.ref != 'refs/heads/main' }}"))
    );
}

#[test]
fn permissions_forms() {
    let wf = parse(
        "on: push\npermissions: read-all\njobs:\n  j:\n    runs-on: x\n    permissions: {}\n    steps:\n      - run: echo\n",
    );
    assert_eq!(wf.permissions, Some(Permissions::ReadAll));
    assert_eq!(
        wf.jobs["j"].permissions,
        Some(Permissions::Map(Default::default()))
    );
    let e = parse_err(
        "on: push\npermissions: admin\njobs:\n  j:\n    runs-on: x\n    steps:\n      - run: echo\n",
    );
    assert!(e.contains("admin"), "{e}");
    let e = parse_err(
        "on: push\npermissions:\n  contents: admin\njobs:\n  j:\n    runs-on: x\n    steps:\n      - run: echo\n",
    );
    assert!(e.contains("contents"), "{e}");
}

#[test]
fn container_string_form_and_timeouts() {
    let wf = parse(&with_jobs(
        "  j:\n    runs-on: x\n    container: node:20\n    timeout-minutes: ${{ inputs.t }}\n    steps:\n      - run: echo\n        timeout-minutes: 5\n        continue-on-error: true\n",
    ));
    let j = &wf.jobs["j"];
    assert_eq!(j.container.as_ref().unwrap().image, "node:20");
    assert_eq!(j.timeout_minutes, Some(json!("${{ inputs.t }}")));
    assert_eq!(j.steps[0].timeout_minutes, Some(json!(5)));
    assert_eq!(j.steps[0].continue_on_error, Some(json!(true)));
}

#[test]
fn matrix_as_expression_string() {
    let wf = parse(&with_jobs(
        "  j:\n    runs-on: x\n    strategy:\n      matrix: ${{ fromJSON(needs.setup.outputs.matrix) }}\n    steps:\n      - run: echo\n",
    ));
    assert_eq!(
        wf.jobs["j"].strategy.as_ref().unwrap().matrix,
        Some(json!("${{ fromJSON(needs.setup.outputs.matrix) }}"))
    );
    let wf = parse(&with_jobs(
        "  j:\n    runs-on: x\n    strategy:\n      matrix:\n        os: ${{ fromJSON(inputs.oses) }}\n    steps:\n      - run: echo\n",
    ));
    assert_eq!(
        wf.jobs["j"].strategy.as_ref().unwrap().matrix,
        Some(json!({"os": "${{ fromJSON(inputs.oses) }}"}))
    );
}

#[test]
fn schedule_parsing() {
    let wf = parse(&with_on(
        "on:\n  schedule:\n    - cron: '*/15 * * * *'\n    - cron: \"0 0 * * MON\"",
    ));
    assert_eq!(wf.on.schedules(), s(&["*/15 * * * *", "0 0 * * MON"]));
    let no = parse(&with_on("on: push"));
    assert!(no.on.schedules().is_empty());
}

#[test]
fn workflow_run_trigger() {
    let wf = parse(&with_on(
        "on:\n  workflow_run:\n    workflows: [CI, \"Build docs\"]\n    types: [completed]\n    branches: [main]",
    ));
    let t = wf.on.get("workflow_run").unwrap();
    assert_eq!(t.workflows, s(&["CI", "Build docs"]));
    assert!(wf.on.matches_workflow_run("CI", "completed", "main"));
    assert!(!wf.on.matches_workflow_run("CI", "requested", "main"));
    assert!(!wf.on.matches_workflow_run("CI", "completed", "dev"));
    assert!(!wf.on.matches_workflow_run("Other", "completed", "main"));
}

// ---------------------------------------------------------------------------
// validation errors

#[test]
fn error_invalid_yaml() {
    let err = parse_workflow("on: [push\njobs:").unwrap_err();
    assert!(matches!(err, WorkflowError::Yaml(_)), "{err:?}");
}

#[test]
fn error_empty_and_non_mapping() {
    assert!(parse_err("").contains("empty"));
    assert!(parse_err("- a\n- b").contains("mapping"));
}

#[test]
fn error_missing_on() {
    let e = parse_err("jobs:\n  j:\n    runs-on: x\n    steps:\n      - run: echo\n");
    assert!(e.contains("'on'"), "{e}");
}

#[test]
fn error_missing_or_empty_jobs() {
    assert!(parse_err("on: push").contains("jobs"));
    assert!(parse_err("on: push\njobs: {}").contains("at least one job"));
    assert!(parse_err("on: push\njobs:").contains("at least one job"));
}

#[test]
fn error_unknown_top_level_key() {
    let e = parse_err(
        "on: push\nfoo: bar\njobs:\n  j:\n    runs-on: x\n    steps:\n      - run: echo\n",
    );
    assert!(e.contains("'foo'"), "{e}");
}

#[test]
fn error_unknown_job_key() {
    let e = parse_err(&with_jobs(
        "  j:\n    runs-on: x\n    step:\n      - run: echo\n",
    ));
    assert!(e.contains("job 'j'") && e.contains("'step'"), "{e}");
}

#[test]
fn error_unknown_step_key() {
    let e = parse_err(&with_jobs(
        "  j:\n    runs-on: x\n    steps:\n      - run: echo\n        args: x\n",
    ));
    assert!(e.contains("job 'j'") && e.contains("'args'"), "{e}");
}

#[test]
fn unknown_keys_inside_with_and_env_tolerated() {
    let wf = parse(&with_jobs(
        "  j:\n    runs-on: x\n    steps:\n      - uses: a/b@v1\n        with:\n          anything-goes: 1\n        env:\n          WHATEVER: x\n",
    ));
    assert_eq!(wf.jobs["j"].steps[0].with["anything-goes"], "1");
}

#[test]
fn error_invalid_job_id() {
    for bad in ["1job", "-job", "my job", "jöb"] {
        let e = parse_err(&with_jobs(&format!(
            "  \"{bad}\":\n    runs-on: x\n    steps:\n      - run: echo\n"
        )));
        assert!(e.contains("invalid job id"), "{bad}: {e}");
    }
    // valid ids
    parse(&with_jobs(
        "  _a-b_1:\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
}

#[test]
fn error_cycle() {
    let e = parse_err(&with_jobs(
        "  a:\n    needs: c\n    runs-on: x\n    steps:\n      - run: echo\n  b:\n    needs: a\n    runs-on: x\n    steps:\n      - run: echo\n  c:\n    needs: [b]\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("cycle"), "{e}");
    assert!(e.contains("a -> c -> b -> a"), "{e}");
}

#[test]
fn error_self_dependency() {
    let e = parse_err(&with_jobs(
        "  a:\n    needs: a\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("itself"), "{e}");
}

#[test]
fn error_unknown_needs() {
    let e = parse_err(&with_jobs(
        "  a:\n    needs: [build]\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("job 'a'") && e.contains("'build'"), "{e}");
}

#[test]
fn error_missing_runs_on() {
    let e = parse_err(&with_jobs("  build:\n    steps:\n      - run: echo\n"));
    assert!(e.contains("job 'build'") && e.contains("runs-on"), "{e}");
}

#[test]
fn error_no_steps_or_uses() {
    let e = parse_err(&with_jobs("  build:\n    runs-on: x\n"));
    assert!(e.contains("'steps' or 'uses'"), "{e}");
    let e = parse_err(&with_jobs("  build:\n    runs-on: x\n    steps: []\n"));
    assert!(e.contains("at least one step"), "{e}");
}

#[test]
fn error_step_with_both_run_and_uses() {
    let e = parse_err(&with_jobs(
        "  build:\n    runs-on: x\n    steps:\n      - name: Bad\n        run: echo\n        uses: a/b@v1\n",
    ));
    assert!(
        e.contains("job 'build'") && e.contains("step 1 ('Bad')") && e.contains("both"),
        "{e}"
    );
}

#[test]
fn error_step_with_neither() {
    let e = parse_err(&with_jobs(
        "  build:\n    runs-on: x\n    steps:\n      - name: Empty\n",
    ));
    assert!(e.contains("either 'run' or 'uses'"), "{e}");
}

#[test]
fn error_duplicate_step_id() {
    let e = parse_err(&with_jobs(
        "  build:\n    runs-on: x\n    steps:\n      - id: s\n        run: a\n      - id: s\n        run: b\n",
    ));
    assert!(
        e.contains("job 'build'") && e.contains("duplicate step id 's'"),
        "{e}"
    );
}

#[test]
fn same_step_id_in_different_jobs_ok() {
    parse(&with_jobs(
        "  a:\n    runs-on: x\n    steps:\n      - id: s\n        run: a\n  b:\n    runs-on: x\n    steps:\n      - id: s\n        run: b\n",
    ));
}

#[test]
fn error_invalid_step_id() {
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: x\n    steps:\n      - id: 1st\n        run: a\n",
    ));
    assert!(e.contains("invalid step id"), "{e}");
}

#[test]
fn error_bad_cron() {
    let e = parse_err(&with_on("on:\n  schedule:\n    - cron: '61 * * * *'"));
    assert!(e.contains("cron"), "{e}");
    let e = parse_err(&with_on("on:\n  schedule:\n    - foo: bar"));
    assert!(e.contains("cron"), "{e}");
}

#[test]
fn error_unknown_event() {
    let e = parse_err(&with_on("on: pushh"));
    assert!(e.contains("pushh"), "{e}");
}

#[test]
fn error_branches_and_branches_ignore() {
    let e = parse_err(&with_on(
        "on:\n  push:\n    branches: [main]\n    branches-ignore: [dev]",
    ));
    assert!(e.contains("branches-ignore"), "{e}");
}

#[test]
fn error_reusable_job_with_steps_or_runs_on() {
    let e = parse_err(&with_jobs(
        "  a:\n    uses: ./.github/workflows/x.yml\n    runs-on: x\n",
    ));
    assert!(e.contains("job 'a'") && e.contains("runs-on"), "{e}");
    let e = parse_err(&with_jobs(
        "  a:\n    uses: ./.github/workflows/x.yml\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("steps"), "{e}");
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: x\n    with:\n      a: 1\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("'with'"), "{e}");
}

#[test]
fn error_bad_input_definitions() {
    let e = parse_err(&with_on(
        "on:\n  workflow_dispatch:\n    inputs:\n      x:\n        type: list",
    ));
    assert!(e.contains("invalid input type 'list'"), "{e}");
    let e = parse_err(&with_on(
        "on:\n  workflow_dispatch:\n    inputs:\n      x:\n        type: choice",
    ));
    assert!(e.contains("options"), "{e}");
    let e = parse_err(&with_on(
        "on:\n  workflow_dispatch:\n    inputs:\n      x:\n        type: choice\n        options: [a, b]\n        default: c",
    ));
    assert!(e.contains("not one of the options"), "{e}");
    let e = parse_err(&with_on(
        "on:\n  workflow_call:\n    inputs:\n      x:\n        type: choice\n        options: [a]",
    ));
    assert!(e.contains("choice"), "{e}");
}

#[test]
fn error_container_without_image() {
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: x\n    services:\n      db:\n        ports: [5432]\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("services.db") && e.contains("image"), "{e}");
}

#[test]
fn error_bad_matrix_value() {
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: x\n    strategy:\n      matrix:\n        os: 3\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("matrix.os"), "{e}");
}

#[test]
fn error_uses_step_with_shell() {
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: x\n    steps:\n      - uses: a/b@v1\n        shell: bash\n",
    ));
    assert!(e.contains("shell"), "{e}");
}

#[test]
fn error_bad_runs_on_type() {
    let e = parse_err(&with_jobs(
        "  a:\n    runs-on: 3\n    steps:\n      - run: echo\n",
    ));
    assert!(e.contains("runs-on"), "{e}");
}

// ---------------------------------------------------------------------------
// job ordering

#[test]
fn job_order_stable_topological() {
    let wf = parse(&with_jobs(
        "  deploy:\n    needs: [test, build]\n    runs-on: x\n    steps:\n      - run: echo\n  test:\n    needs: build\n    runs-on: x\n    steps:\n      - run: echo\n  lint:\n    runs-on: x\n    steps:\n      - run: echo\n  build:\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
    assert_eq!(wf.job_order(), s(&["lint", "build", "test", "deploy"]));
}

#[test]
fn job_order_independent_jobs_keep_declaration_order() {
    let wf = parse(&with_jobs(
        "  c:\n    runs-on: x\n    steps:\n      - run: echo\n  a:\n    runs-on: x\n    steps:\n      - run: echo\n  b:\n    runs-on: x\n    steps:\n      - run: echo\n",
    ));
    assert_eq!(wf.job_order(), s(&["c", "a", "b"]));
}

#[test]
fn job_order_respects_every_dependency() {
    let wf = parse(RUST_CI);
    let order = wf.job_order();
    for (id, job) in &wf.jobs {
        let pos = order.iter().position(|x| x == id).unwrap();
        for need in &job.needs {
            assert!(order.iter().position(|x| x == need).unwrap() < pos);
        }
    }
}

// ---------------------------------------------------------------------------
// push filters

fn push_wf(on: &str) -> Workflow {
    parse(&with_on(on))
}

fn files(f: &[&str]) -> Vec<String> {
    s(f)
}

#[test]
fn push_without_filters_matches_all_refs() {
    let wf = push_wf("on: push");
    assert!(wf.on.matches_push("refs/heads/main", None));
    assert!(wf.on.matches_push("refs/heads/feature/x", None));
    assert!(wf.on.matches_push("refs/tags/v1", None));
}

#[test]
fn push_branches_only_excludes_tags() {
    let wf = push_wf("on:\n  push:\n    branches: ['**']");
    assert!(wf.on.matches_push("refs/heads/main", None));
    assert!(!wf.on.matches_push("refs/tags/v1", None));
    let wf = push_wf("on:\n  push:\n    branches-ignore: [dev]");
    assert!(wf.on.matches_push("refs/heads/main", None));
    assert!(!wf.on.matches_push("refs/heads/dev", None));
    assert!(!wf.on.matches_push("refs/tags/v1", None));
}

#[test]
fn push_tags_only_excludes_branches() {
    let wf = push_wf("on:\n  push:\n    tags: ['v*']");
    assert!(wf.on.matches_push("refs/tags/v1.0", None));
    assert!(!wf.on.matches_push("refs/tags/release", None));
    assert!(!wf.on.matches_push("refs/heads/main", None));
    let wf = push_wf("on:\n  push:\n    tags-ignore: ['*-rc*']");
    assert!(wf.on.matches_push("refs/tags/v1", None));
    assert!(!wf.on.matches_push("refs/tags/v1-rc1", None));
    assert!(!wf.on.matches_push("refs/heads/main", None));
}

#[test]
fn push_branches_and_tags_both() {
    let wf = push_wf("on:\n  push:\n    branches: [main]\n    tags: ['v*']");
    assert!(wf.on.matches_push("refs/heads/main", None));
    assert!(!wf.on.matches_push("refs/heads/dev", None));
    assert!(wf.on.matches_push("refs/tags/v2", None));
    assert!(!wf.on.matches_push("refs/tags/x2", None));
}

#[test]
fn push_branch_patterns_match_short_name() {
    let wf = push_wf(
        "on:\n  push:\n    branches:\n      - main\n      - 'releases/**'\n      - '!releases/**-alpha'\n      - 'feature/*'",
    );
    assert!(wf.on.matches_push("refs/heads/releases/10", None));
    assert!(wf.on.matches_push("refs/heads/releases/beta/mona", None));
    assert!(!wf.on.matches_push("refs/heads/releases/10-alpha", None));
    assert!(wf.on.matches_push("refs/heads/feature/x", None));
    assert!(!wf.on.matches_push("refs/heads/feature/x/y", None));
    assert!(!wf.on.matches_push("refs/heads/refs/heads/main", None));
}

#[test]
fn push_paths_filter() {
    let wf = push_wf(
        "on:\n  push:\n    paths:\n      - 'sub-project/**'\n      - '!sub-project/docs/**'",
    );
    let t = &wf.on;
    assert!(t.matches_push("refs/heads/main", Some(&files(&["sub-project/src/a.rs"]))));
    assert!(!t.matches_push("refs/heads/main", Some(&files(&["sub-project/docs/a.md"]))));
    assert!(t.matches_push(
        "refs/heads/main",
        Some(&files(&["sub-project/docs/a.md", "sub-project/lib.rs"]))
    ));
    assert!(!t.matches_push("refs/heads/main", Some(&files(&["README.md"]))));
    assert!(
        t.matches_push("refs/heads/main", None),
        "unknown changes pass"
    );
    assert!(!t.matches_push("refs/heads/main", Some(&[])));
}

#[test]
fn push_paths_ignore_all_vs_some() {
    let wf = push_wf("on:\n  push:\n    paths-ignore:\n      - 'docs/**'\n      - '**.md'");
    let t = &wf.on;
    assert!(!t.matches_push(
        "refs/heads/main",
        Some(&files(&["docs/a.txt", "README.md"]))
    ));
    assert!(t.matches_push(
        "refs/heads/main",
        Some(&files(&["docs/a.txt", "src/main.rs"]))
    ));
    assert!(t.matches_push("refs/heads/main", Some(&files(&["src/main.rs"]))));
}

#[test]
fn push_branches_and_paths_both_required() {
    let wf = push_wf("on:\n  push:\n    branches: [main]\n    paths: ['**.js']");
    let t = &wf.on;
    assert!(t.matches_push("refs/heads/main", Some(&files(&["app.js"]))));
    assert!(!t.matches_push("refs/heads/main", Some(&files(&["app.rs"]))));
    assert!(!t.matches_push("refs/heads/dev", Some(&files(&["app.js"]))));
}

#[test]
fn push_paths_not_evaluated_for_tags() {
    let wf = push_wf("on:\n  push:\n    tags: ['v*']\n    paths: ['src/**']");
    assert!(
        wf.on
            .matches_push("refs/tags/v1", Some(&files(&["README.md"])))
    );
}

#[test]
fn push_only_paths_filter_tag_push() {
    // Only a path filter: tag pushes still trigger (no ref filters).
    let wf = push_wf("on:\n  push:\n    paths: ['src/**']");
    assert!(
        wf.on
            .matches_push("refs/tags/v1", Some(&files(&["README.md"])))
    );
    assert!(
        !wf.on
            .matches_push("refs/heads/main", Some(&files(&["README.md"])))
    );
}

#[test]
fn push_not_listening() {
    let wf = push_wf("on: pull_request");
    assert!(!wf.on.matches_push("refs/heads/main", None));
}

// ---------------------------------------------------------------------------
// pull request filters

#[test]
fn pull_request_default_types() {
    let wf = push_wf("on: pull_request");
    for a in ["opened", "synchronize", "reopened"] {
        assert!(
            wf.on.matches_pull_request("pull_request", a, "main", None),
            "{a}"
        );
    }
    for a in ["closed", "labeled", "edited", "ready_for_review"] {
        assert!(
            !wf.on.matches_pull_request("pull_request", a, "main", None),
            "{a}"
        );
    }
}

#[test]
fn pull_request_explicit_types() {
    let wf = push_wf("on:\n  pull_request:\n    types: [closed, labeled]");
    assert!(
        wf.on
            .matches_pull_request("pull_request", "closed", "main", None)
    );
    assert!(
        wf.on
            .matches_pull_request("pull_request", "labeled", "main", None)
    );
    assert!(
        !wf.on
            .matches_pull_request("pull_request", "opened", "main", None)
    );
}

#[test]
fn pull_request_base_branch_filters() {
    let wf = push_wf(
        "on:\n  pull_request_target:\n    branches: ['releases/**']\n  pull_request:\n    branches-ignore: ['mona/octocat']",
    );
    assert!(
        wf.on
            .matches_pull_request("pull_request_target", "opened", "releases/1.0", None)
    );
    assert!(wf.on.matches_pull_request(
        "pull_request_target",
        "opened",
        "refs/heads/releases/1.0",
        None
    ));
    assert!(
        !wf.on
            .matches_pull_request("pull_request_target", "opened", "main", None)
    );
    assert!(
        wf.on
            .matches_pull_request("pull_request", "opened", "main", None)
    );
    assert!(
        !wf.on
            .matches_pull_request("pull_request", "opened", "mona/octocat", None)
    );
}

#[test]
fn pull_request_paths() {
    let wf = push_wf("on:\n  pull_request:\n    paths-ignore: ['docs/**']");
    assert!(!wf.on.matches_pull_request(
        "pull_request",
        "opened",
        "main",
        Some(&files(&["docs/x.md"]))
    ));
    assert!(wf.on.matches_pull_request(
        "pull_request",
        "opened",
        "main",
        Some(&files(&["docs/x.md", "a.rs"]))
    ));
}

// ---------------------------------------------------------------------------
// activity events

#[test]
fn activity_events() {
    let wf = push_wf(
        "on:\n  release:\n    types: [published]\n  issues:\n  issue_comment:\n    types: [created, edited]",
    );
    assert!(wf.on.matches_activity("release", "published"));
    assert!(!wf.on.matches_activity("release", "created"));
    assert!(wf.on.matches_activity("issues", "opened"));
    assert!(wf.on.matches_activity("issues", "anything"));
    assert!(wf.on.matches_activity("issue_comment", "edited"));
    assert!(!wf.on.matches_activity("issue_comment", "deleted"));
    assert!(!wf.on.matches_activity("label", "created"));
}

#[test]
fn types_accepts_single_string() {
    let wf = push_wf("on:\n  release:\n    types: published");
    assert_eq!(wf.on.get("release").unwrap().types, s(&["published"]));
}
