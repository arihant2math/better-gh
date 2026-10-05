//! Linguist-like language statistics: file extension / name → language,
//! counting bytes of programming and markup files only (prose and data
//! formats such as Markdown, JSON or YAML are ignored, as are vendored,
//! generated and documentation paths), like GitHub's language bar.

use std::collections::HashMap;

use crate::objects::TreeEntryKind;
use crate::ops::LsTreeEntry;

/// Language for a path, if it is a counted (programming/markup) language.
pub fn detect(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "dockerfile" | "containerfile" => Some("Dockerfile"),
        "makefile" | "gnumakefile" => Some("Makefile"),
        "cmakelists.txt" => Some("CMake"),
        "rakefile" | "gemfile" => Some("Ruby"),
        "justfile" => Some("Just"),
        "build" | "build.bazel" | "workspace" => Some("Starlark"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    if lower.starts_with("dockerfile.") {
        return Some("Dockerfile");
    }
    let ext = lower.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => "Rust",
        "go" => "Go",
        "py" | "pyw" | "pyi" => "Python",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "mts" | "cts" | "tsx" => "TypeScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" | "sc" => "Scala",
        "groovy" | "gradle" => "Groovy",
        "c" => "C",
        "h" => "C",
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" => "C++",
        "m" => "Objective-C",
        "mm" => "Objective-C++",
        "cs" => "C#",
        "fs" | "fsi" | "fsx" => "F#",
        "vb" => "Visual Basic .NET",
        "swift" => "Swift",
        "rb" | "gemspec" | "rake" => "Ruby",
        "php" | "phtml" => "PHP",
        "pl" | "pm" => "Perl",
        "lua" => "Lua",
        "r" => "R",
        "jl" => "Julia",
        "hs" | "lhs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "clj" | "cljs" | "cljc" | "edn" => "Clojure",
        "lisp" | "lsp" | "cl" => "Common Lisp",
        "el" => "Emacs Lisp",
        "scm" | "ss" => "Scheme",
        "rkt" => "Racket",
        "dart" => "Dart",
        "zig" => "Zig",
        "nim" => "Nim",
        "v" | "sv" | "svh" => "Verilog",
        "vhd" | "vhdl" => "VHDL",
        "sh" | "bash" | "zsh" | "ksh" => "Shell",
        "fish" => "fish",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "bat" | "cmd" => "Batchfile",
        "sql" => "SQL",
        "pls" | "plsql" => "PLSQL",
        "html" | "htm" | "xhtml" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "sass" => "Sass",
        "less" => "Less",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "astro" => "Astro",
        "elm" => "Elm",
        "purs" => "PureScript",
        "coffee" => "CoffeeScript",
        "tex" | "sty" | "cls" => "TeX",
        "asm" | "s" => "Assembly",
        "nix" => "Nix",
        "tf" | "hcl" => "HCL",
        "proto" => "Protocol Buffer",
        "graphql" | "gql" => "GraphQL",
        "sol" => "Solidity",
        "cr" => "Crystal",
        "d" => "D",
        "f" | "f90" | "f95" | "f03" | "for" => "Fortran",
        "pas" | "pp" => "Pascal",
        "ada" | "adb" | "ads" => "Ada",
        "cob" | "cbl" => "COBOL",
        "vim" => "Vim Script",
        "cmake" => "CMake",
        "mk" => "Makefile",
        "ipynb" => "Jupyter Notebook",
        "wat" | "wast" => "WebAssembly",
        "glsl" | "vert" | "frag" => "GLSL",
        "hlsl" => "HLSL",
        "cu" | "cuh" => "Cuda",
        "gd" => "GDScript",
        "hx" => "Haxe",
        "rst" | "md" | "markdown" | "txt" | "json" | "yaml" | "yml" | "toml" | "xml" | "csv"
        | "lock" | "ini" | "cfg" | "conf" | "svg" => return None,
        _ => return None,
    })
}

/// Paths excluded from statistics: vendored dependencies, generated or
/// minified files and documentation.
pub fn is_excluded(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    const DIRS: &[&str] = &[
        "node_modules/",
        "vendor/",
        "vendors/",
        "third_party/",
        "third-party/",
        "thirdparty/",
        "external/",
        "bower_components/",
        "dist/",
        ".git/",
        "docs/",
        "doc/",
        "documentation/",
        "target/",
        "build/",
        "__pycache__/",
        ".yarn/",
        "pods/",
        "carthage/",
    ];
    let segments_start = |dir: &str| lower.starts_with(dir) || lower.contains(&format!("/{dir}"));
    DIRS.iter().any(|d| segments_start(d))
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".pb.go")
        || lower.ends_with("_pb2.py")
        || lower.ends_with(".generated.ts")
        || lower.ends_with(".d.ts")
}

/// Bytes per language from a recursive tree listing, largest first (ties
/// by name).
pub fn tally(entries: &[LsTreeEntry]) -> Vec<(String, u64)> {
    let mut counts: HashMap<&'static str, u64> = HashMap::new();
    for e in entries {
        if !matches!(e.kind, TreeEntryKind::Blob | TreeEntryKind::Executable) {
            continue;
        }
        if is_excluded(&e.path) {
            continue;
        }
        if let (Some(lang), Some(size)) = (detect(&e.path), e.size) {
            *counts.entry(lang).or_default() += size;
        }
    }
    let mut out: Vec<(String, u64)> = counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(l, n)| (l.to_string(), n))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> LsTreeEntry {
        LsTreeEntry {
            path: path.into(),
            mode: "100644".into(),
            kind: TreeEntryKind::Blob,
            sha: "a".repeat(40),
            size: Some(size),
        }
    }

    #[test]
    fn detects_languages() {
        assert_eq!(detect("src/main.rs"), Some("Rust"));
        assert_eq!(detect("web/App.tsx"), Some("TypeScript"));
        assert_eq!(detect("Dockerfile"), Some("Dockerfile"));
        assert_eq!(detect("README.md"), None);
        assert_eq!(detect("noext"), None);
        assert!(is_excluded("web/node_modules/x/index.js"));
        assert!(is_excluded("vendor/lib.go"));
        assert!(!is_excluded("src/vendored.rs"));
    }

    #[test]
    fn tallies() {
        let t = tally(&[
            entry("a.rs", 100),
            entry("b.rs", 50),
            entry("c.py", 120),
            entry("README.md", 1000),
            entry("node_modules/x.js", 9999),
        ]);
        assert_eq!(t, vec![("Rust".into(), 150), ("Python".into(), 120)]);
    }
}
