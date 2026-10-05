//! Language detection by file name / extension (a small Linguist subset).

/// Lowercased extension without the dot (`"rs"`), if any.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Linguist-style language name for a file.
pub fn detect(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "dockerfile" | "containerfile" => return Some("Dockerfile"),
        "makefile" | "gnumakefile" => return Some("Makefile"),
        "cmakelists.txt" => return Some("CMake"),
        "rakefile" | "gemfile" => return Some("Ruby"),
        "justfile" => return Some("Just"),
        _ => {}
    }
    let ext = extension(&lower)?;
    Some(match ext.as_str() {
        "rs" => "Rust",
        "go" => "Go",
        "py" | "pyi" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JavaScript",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TSX",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" => "Scala",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
        "cs" => "C#",
        "fs" | "fsx" => "F#",
        "m" => "Objective-C",
        "mm" => "Objective-C++",
        "rb" => "Ruby",
        "php" => "PHP",
        "pl" | "pm" => "Perl",
        "lua" => "Lua",
        "r" => "R",
        "jl" => "Julia",
        "dart" => "Dart",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "hs" => "Haskell",
        "ml" | "mli" => "OCaml",
        "clj" | "cljs" | "cljc" => "Clojure",
        "zig" => "Zig",
        "nim" => "Nim",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" => "PowerShell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" => "SCSS",
        "sass" => "Sass",
        "less" => "Less",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "json" => "JSON",
        "yml" | "yaml" => "YAML",
        "toml" => "TOML",
        "xml" => "XML",
        "md" | "markdown" => "Markdown",
        "rst" => "reStructuredText",
        "tex" => "TeX",
        "proto" => "Protocol Buffer",
        "graphql" | "gql" => "GraphQL",
        "tf" => "HCL",
        "nix" => "Nix",
        "vim" => "Vim Script",
        "txt" => "Text",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects() {
        assert_eq!(detect("main.rs"), Some("Rust"));
        assert_eq!(detect("App.TSX"), Some("TSX"));
        assert_eq!(detect("Dockerfile"), Some("Dockerfile"));
        assert_eq!(detect("unknown.zzz"), None);
        assert_eq!(extension(".gitignore"), None);
        assert_eq!(extension("a.tar.GZ").as_deref(), Some("gz"));
    }
}
