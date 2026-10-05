/* Mock repository content: file trees, a toy highlighter and diff generation. */
import { Rng, fakeSha } from './rng';

export interface MockFile {
  path: string;
  content: string;
}

const RUST: Record<string, string> = {
  'Cargo.toml': `[package]
name = "{name}"
version = "0.4.2"
edition = "2024"
license = "MIT OR Apache-2.0"

[dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
thiserror = "2"

[dev-dependencies]
proptest = "1"
`,
  'src/main.rs': `use std::net::SocketAddr;

use tracing::info;

mod config;
mod error;
mod router;

/// Entry point: parse config, bind, serve until ctrl-c.
#[tokio::main]
async fn main() -> Result<(), error::Error> {
    tracing_subscriber::fmt::init();
    let cfg = config::Config::from_env()?;
    let addr: SocketAddr = cfg.listen.parse()?;

    info!(%addr, "starting {name}");
    let app = router::build(cfg.clone());
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
`,
  'src/config.rs': `use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub listen: String,
    pub database_url: String,
    /// Maximum number of pooled connections.
    pub max_connections: u32,
}

impl Config {
    pub fn from_env() -> Result<Self, crate::error::Error> {
        Ok(Self {
            listen: std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:3000".into()),
            database_url: std::env::var("DATABASE_URL")?,
            max_connections: 16,
        })
    }
}
`,
  'src/error.rs': `use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("missing environment variable: {0}")]
    Env(#[from] std::env::VarError),
    #[error("invalid address: {0}")]
    Addr(#[from] std::net::AddrParseError),
}
`,
  'src/router.rs': `use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

use crate::config::Config;

pub fn build(_cfg: Config) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/items", get(list_items))
}

async fn health() -> &'static str {
    "ok"
}

async fn list_items() -> Json<Value> {
    // TODO: paginate (see #42)
    Json(json!({ "items": [], "next": null }))
}
`,
  'tests/health.rs': `#[tokio::test]
async fn health_returns_ok() {
    let body = reqwest::get("http://localhost:3000/health").await.unwrap().text().await.unwrap();
    assert_eq!(body, "ok");
}
`,
};

const TS: Record<string, string> = {
  'package.json': `{
  "name": "@{owner}/{name}",
  "version": "2.3.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc -b && vite build",
    "test": "vitest run"
  },
  "dependencies": {
    "react": "^19.0.0",
    "react-dom": "^19.0.0"
  }
}
`,
  'tsconfig.json': `{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true
  },
  "include": ["src"]
}
`,
  'src/index.ts': `export { Button } from './components/Button';
export { useDebounce } from './hooks/useDebounce';
export type { ButtonProps } from './components/Button';
`,
  'src/components/Button.tsx': `import type { ButtonHTMLAttributes } from 'react';
import styles from './Button.module.css';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: 'primary' | 'secondary' | 'danger';
  size?: 'sm' | 'md';
}

/** The one button. Keep variants small and boring. */
export function Button({ variant = 'secondary', size = 'md', className, ...rest }: ButtonProps) {
  const cls = [styles.button, styles[variant], styles[size], className].filter(Boolean).join(' ');
  return <button type="button" className={cls} {...rest} />;
}
`,
  'src/hooks/useDebounce.ts': `import { useEffect, useState } from 'react';

/** Debounce a value by \`delay\` milliseconds. */
export function useDebounce<T>(value: T, delay = 200): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const id = setTimeout(() => setDebounced(value), delay);
    return () => clearTimeout(id);
  }, [value, delay]);
  return debounced;
}
`,
  'src/lib/format.ts': `const rtf = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

export function relativeTime(date: Date, now = new Date()): string {
  const seconds = Math.round((date.getTime() - now.getTime()) / 1000);
  const abs = Math.abs(seconds);
  if (abs < 60) return rtf.format(seconds, 'second');
  if (abs < 3600) return rtf.format(Math.round(seconds / 60), 'minute');
  if (abs < 86400) return rtf.format(Math.round(seconds / 3600), 'hour');
  return rtf.format(Math.round(seconds / 86400), 'day');
}
`,
};

const GO: Record<string, string> = {
  'go.mod': `module github.com/{owner}/{name}

go 1.23
`,
  'main.go': `package main

import (
	"context"
	"log"
	"os/signal"
	"syscall"

	"github.com/{owner}/{name}/scheduler"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	s := scheduler.New(scheduler.Options{Workers: 8})
	if err := s.Run(ctx); err != nil {
		log.Fatalf("scheduler: %v", err)
	}
}
`,
  'scheduler/scheduler.go': `package scheduler

import (
	"context"
	"sync"
	"time"
)

// Options configures a Scheduler.
type Options struct {
	Workers int
}

// Scheduler runs jobs on a fixed pool of workers.
type Scheduler struct {
	opts Options
	jobs chan Job
	wg   sync.WaitGroup
}

// Job is a unit of work.
type Job func(ctx context.Context) error

func New(opts Options) *Scheduler {
	return &Scheduler{opts: opts, jobs: make(chan Job, 1024)}
}

func (s *Scheduler) Run(ctx context.Context) error {
	for i := 0; i < s.opts.Workers; i++ {
		s.wg.Add(1)
		go s.worker(ctx)
	}
	<-ctx.Done()
	s.wg.Wait()
	return nil
}

func (s *Scheduler) worker(ctx context.Context) {
	defer s.wg.Done()
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case job := <-s.jobs:
			_ = job(ctx)
		case <-ticker.C:
		}
	}
}
`,
};

const PY: Record<string, string> = {
  'pyproject.toml': `[project]
name = "{name}"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["numpy>=2", "polars>=1"]
`,
  'src/analysis.py': `"""Helpers for the weekly metrics notebook."""
from dataclasses import dataclass

import polars as pl


@dataclass
class Window:
    start: str
    end: str


def load(path: str, window: Window) -> pl.DataFrame:
    # Filter early: the raw export is ~2 GB.
    df = pl.scan_parquet(path)
    return df.filter(pl.col("ts").is_between(window.start, window.end)).collect()
`,
};

const SHELL: Record<string, string> = {
  'install.sh': `#!/usr/bin/env bash
set -euo pipefail

DOTFILES="$(cd "$(dirname "$0")" && pwd)"
for f in zshrc gitconfig tmux.conf; do
  ln -sf "$DOTFILES/$f" "$HOME/.$f"
  echo "linked $f"
done
`,
  zshrc: `export EDITOR=nvim
alias g=git
alias gs="git status -sb"
bindkey -e
`,
};

export function repoFiles(owner: string, name: string, language: string | null, description: string | null): MockFile[] {
  const base: Record<string, string> =
    language === 'Rust' ? RUST : language === 'Go' ? GO : language === 'Python' ? PY : language === 'Shell' ? SHELL : TS;
  const files: MockFile[] = Object.entries(base).map(([path, content]) => ({
    path,
    content: content.replaceAll('{name}', name).replaceAll('{owner}', owner),
  }));
  files.push({ path: 'README.md', content: readme(owner, name, language, description) });
  files.push({ path: 'LICENSE', content: `MIT License\n\nCopyright (c) 2026 ${owner}\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software and associated documentation files (the "Software"), to deal\nin the Software without restriction.\n` });
  files.push({ path: '.gitignore', content: 'target/\nnode_modules/\ndist/\n.env\n' });
  files.push({ path: 'docs/CONTRIBUTING.md', content: `# Contributing to ${name}\n\n1. Fork and clone.\n2. Create a branch: \`git switch -c my-change\`.\n3. Run the tests.\n4. Open a pull request.\n` });
  return files.sort((a, b) => a.path.localeCompare(b.path));
}

function readme(owner: string, name: string, language: string | null, description: string | null): string {
  const install =
    language === 'Rust'
      ? `cargo add ${name}`
      : language === 'Go'
        ? `go get github.com/${owner}/${name}`
        : language === 'Python'
          ? `pip install ${name}`
          : `npm install @${owner}/${name}`;
  return `# ${name}

${description ?? ''}

## Install

\`\`\`sh
${install}
\`\`\`

## Features

- **Fast** — designed for low latency from day one
- **Small** — no heavy dependencies
- **Typed** — strict types end to end
- [x] Stable API
- [ ] Plugin system (see the roadmap)

## Usage

See [\`docs/CONTRIBUTING.md\`](docs/CONTRIBUTING.md) for the development workflow.

| Command | Description |
|---------|-------------|
| \`build\` | Compile everything |
| \`test\`  | Run the test suite |

> **Note**
> This project is maintained by @${owner}. Issues and PRs welcome.
`;
}

export function blobSha(content: string): string {
  return fakeSha(`blob ${content.length}\0${content}`);
}

// ---------------------------------------------------------------- highlighter

const KEYWORDS: Record<string, string[]> = {
  rust: ['fn', 'let', 'mut', 'pub', 'use', 'mod', 'struct', 'enum', 'impl', 'async', 'await', 'match', 'if', 'else', 'return', 'for', 'in', 'while', 'loop', 'crate', 'self', 'Self', 'where', 'trait', 'const', 'static', 'move', 'ref', 'type', 'as', 'dyn'],
  typescript: ['import', 'export', 'from', 'const', 'let', 'function', 'return', 'if', 'else', 'interface', 'type', 'extends', 'new', 'class', 'default', 'async', 'await', 'for', 'of', 'in', 'typeof', 'as'],
  go: ['package', 'import', 'func', 'return', 'type', 'struct', 'for', 'range', 'if', 'else', 'go', 'defer', 'select', 'case', 'chan', 'var', 'nil', 'map', 'interface'],
  python: ['def', 'class', 'import', 'from', 'return', 'if', 'else', 'elif', 'for', 'in', 'with', 'as', 'None', 'True', 'False', 'async', 'await'],
  shell: ['for', 'do', 'done', 'if', 'then', 'fi', 'in', 'set', 'echo', 'export', 'alias'],
};

export function languageOf(path: string): string | null {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  return (
    { rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'typescript', go: 'go', py: 'python', sh: 'shell', toml: 'toml', json: 'json', md: 'markdown', mod: 'go' } as Record<string, string>
  )[ext] ?? (path.endsWith('zshrc') ? 'shell' : null);
}

const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

/** Toy line highlighter producing the `hl-*` classes the real server emits. */
export function highlight(content: string, language: string | null): string[] {
  const kw = new Set(KEYWORDS[language ?? ''] ?? []);
  const comment = language === 'python' || language === 'shell' || language === 'toml' ? '#' : '//';
  return content.replace(/\n$/, '').split('\n').map((line) => {
    let out = '';
    const re = /("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`[^`]*`)|(\b\d+(?:\.\d+)?\b)|([A-Za-z_][\w]*)|(\s+)|(.)/g;
    const ci = line.indexOf(comment);
    const code = ci >= 0 && !/["'`]/.test(line.slice(0, ci)) ? line.slice(0, ci) : line;
    const rest = code.length < line.length ? line.slice(code.length) : '';
    let m: RegExpExecArray | null;
    while ((m = re.exec(code))) {
      if (m[1]) out += `<span class="hl-s">${esc(m[1])}</span>`;
      else if (m[2]) out += `<span class="hl-n">${m[2]}</span>`;
      else if (m[3]) {
        const w = m[3];
        const next = code[re.lastIndex];
        if (kw.has(w)) out += `<span class="hl-k">${w}</span>`;
        else if (/^[A-Z]/.test(w)) out += `<span class="hl-t">${w}</span>`;
        else if (next === '(' || next === '!') out += `<span class="hl-f">${w}</span>`;
        else out += w;
      } else out += esc(m[0]);
    }
    if (rest) out += `<span class="hl-c">${esc(rest)}</span>`;
    return out;
  });
}

// ---------------------------------------------------------------- diffs

const NEW_LINES = [
  '    // Retry transient failures with jittered backoff.',
  '    let attempts = 0;',
  '    if (attempts > MAX_RETRIES) {',
  '        return Err(Error::Timeout);',
  '    }',
  '  const controller = new AbortController();',
  '  if (!response.ok) throw new ApiError(response.status);',
  '  // Cache by ETag so repeat visits are free.',
  '  return cache.get(key) ?? load(key);',
  '\tif err != nil {',
  '\t\treturn fmt.Errorf("dispatch: %w", err)',
  '\t}',
  '    log.debug("flushing %d items", len(batch))',
];

/** Deterministic unified diff for a PR over the repo's files. */
export function pullDiff(files: MockFile[], seed: number, changedFiles: number): string {
  const rng = new Rng(seed);
  const code = files.filter((f) => !/LICENSE|\.gitignore/.test(f.path));
  const chosen = rng.sample(code, Math.min(changedFiles, code.length));
  const extra = changedFiles - chosen.length;
  let out = '';
  for (const f of chosen) out += fileDiff(f, rng);
  for (let i = 0; i < extra; i++) {
    const path = `src/feature_${seed % 97}_${i}.${rng.pick(['rs', 'ts', 'go'])}`;
    const lines = rng.sample(NEW_LINES, rng.int(4, 9));
    out += `diff --git a/${path} b/${path}\nnew file mode 100644\nindex 0000000..${fakeSha(path).slice(0, 7)}\n--- /dev/null\n+++ b/${path}\n@@ -0,0 +1,${lines.length} @@\n${lines.map((l) => `+${l}`).join('\n')}\n`;
  }
  return out;
}

function fileDiff(f: MockFile, rng: Rng): string {
  const lines = f.content.replace(/\n$/, '').split('\n');
  let body = '';
  const hunks = Math.max(1, Math.min(3, Math.floor(lines.length / 12)));
  let cursor = 0;
  let offset = 0;
  for (let h = 0; h < hunks; h++) {
    const start = Math.min(lines.length - 1, cursor + rng.int(0, Math.max(1, Math.floor(lines.length / hunks) - 6)));
    const ctxBefore = lines.slice(Math.max(cursor, start - 3), start);
    const removed = lines.slice(start, start + rng.int(0, 2));
    const added = rng.sample(NEW_LINES, rng.int(1, 4));
    const afterStart = start + removed.length;
    const ctxAfter = lines.slice(afterStart, afterStart + 3);
    const oldStart = start - ctxBefore.length + 1;
    const oldLen = ctxBefore.length + removed.length + ctxAfter.length;
    const newLen = ctxBefore.length + added.length + ctxAfter.length;
    body += `@@ -${oldStart},${oldLen} +${oldStart + offset},${newLen} @@\n`;
    body += ctxBefore.map((l) => ` ${l}`).join('\n') + (ctxBefore.length ? '\n' : '');
    body += removed.map((l) => `-${l}`).join('\n') + (removed.length ? '\n' : '');
    body += added.map((l) => `+${l}`).join('\n') + '\n';
    body += ctxAfter.map((l) => ` ${l}`).join('\n') + (ctxAfter.length ? '\n' : '');
    offset += added.length - removed.length;
    cursor = afterStart + ctxAfter.length;
    if (cursor >= lines.length - 2) break;
  }
  const sha = fakeSha(f.path);
  return `diff --git a/${f.path} b/${f.path}\nindex ${sha.slice(0, 7)}..${sha.slice(7, 14)} 100644\n--- a/${f.path}\n+++ b/${f.path}\n${body}`;
}
