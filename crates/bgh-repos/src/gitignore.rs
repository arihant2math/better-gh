//! `.gitignore` templates (github/gitignore, CC0-1.0; vendored under
//! `data/gitignore`, list in `data/gitignore-names.txt`, re-vendor with
//! `scripts/vendor-templates.sh`): `GET /gitignore/templates[/{name}]` and
//! `gitignore_template` on repository creation.

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/gitignore/templates", get(list))
        .route("/gitignore/templates/{name}", get(show))
}

macro_rules! vendored {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../data/gitignore/", $name, ".gitignore")))),*]
    };
}

/// `(name, source)`, sorted by name (byte order, like GitHub).
pub const TEMPLATES: &[(&str, &str)] = vendored![
    "Actionscript",
    "Ada",
    "Agda",
    "Android",
    "Angular",
    "AppEngine",
    "AppceleratorTitanium",
    "ArchLinuxPackages",
    "Autotools",
    "Ballerina",
    "C",
    "C++",
    "CFWheels",
    "CMake",
    "CUDA",
    "CakePHP",
    "ChefCookbook",
    "Clojure",
    "CodeIgniter",
    "CommonLisp",
    "Composer",
    "Concrete5",
    "Coq",
    "CraftCMS",
    "D",
    "DM",
    "Dart",
    "Delphi",
    "Dotnet",
    "Drupal",
    "EPiServer",
    "Eagle",
    "Elisp",
    "Elixir",
    "Elm",
    "Erlang",
    "ExpressionEngine",
    "ExtJs",
    "Fancy",
    "Finale",
    "Firebase",
    "FlaxEngine",
    "Flutter",
    "ForceDotCom",
    "Fortran",
    "FuelPHP",
    "GWT",
    "Gcov",
    "GitBook",
    "GitHubPages",
    "Gleam",
    "Go",
    "Godot",
    "Gradle",
    "Grails",
    "HIP",
    "Haskell",
    "Haxe",
    "IAR",
    "IGORPro",
    "Idris",
    "JBoss",
    "JENKINS_HOME",
    "Java",
    "Jekyll",
    "Joomla",
    "Julia",
    "Katalon",
    "KiCad",
    "Kohana",
    "Kotlin",
    "LabVIEW",
    "LangChain",
    "Laravel",
    "Leiningen",
    "LemonStand",
    "Lilypond",
    "Lithium",
    "Lua",
    "Luau",
    "Magento",
    "Maven",
    "Mercury",
    "MetaProgrammingSystem",
    "Modelica",
    "Nanoc",
    "Nestjs",
    "Nextjs",
    "Nim",
    "Nix",
    "Node",
    "OCaml",
    "Objective-C",
    "Opa",
    "OpenCart",
    "OracleForms",
    "Packer",
    "Perl",
    "Phalcon",
    "PlayFramework",
    "Plone",
    "Prestashop",
    "Processing",
    "PureScript",
    "Python",
    "Qooxdoo",
    "Qt",
    "R",
    "ROS",
    "Racket",
    "Rails",
    "Raku",
    "ReScript",
    "RhodesRhomobile",
    "Ruby",
    "Rust",
    "SCons",
    "Sass",
    "Scala",
    "Scheme",
    "Scrivener",
    "Sdcc",
    "SeamGen",
    "SketchUp",
    "Smalltalk",
    "Solidity-Remix",
    "Stella",
    "SugarCRM",
    "Swift",
    "Symfony",
    "SymphonyCMS",
    "TeX",
    "Terraform",
    "Textpattern",
    "TurboGears2",
    "TwinCAT3",
    "Typo3",
    "Unity",
    "UnrealEngine",
    "VBA",
    "VVVV",
    "VisualStudio",
    "Waf",
    "WordPress",
    "Xojo",
    "Yeoman",
    "Yii",
    "ZendFramework",
    "Zephir",
    "Zig",
];

/// Template by name: exact match first, then case-insensitive.
pub fn find(name: &str) -> Option<(&'static str, &'static str)> {
    TEMPLATES
        .iter()
        .find(|(n, _)| *n == name)
        .or_else(|| TEMPLATES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)))
        .copied()
}

/// `gitignore-template`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitignoreTemplate {
    pub name: String,
    pub source: String,
}

/// `GET /gitignore/templates`: every template name (not paginated).
async fn list(State(_state): State<AppState>) -> Json<Vec<&'static str>> {
    Json(TEMPLATES.iter().map(|(n, _)| *n).collect())
}

/// `GET /gitignore/templates/{name}` (`application/vnd.github.raw` returns
/// the source as text).
async fn show(
    State(_state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let (name, source) = find(&name).ok_or(ApiError::NotFound)?;
    Ok(crate::licenses::raw_or_json(
        &headers,
        source,
        GitignoreTemplate {
            name: name.to_string(),
            source: source.to_string(),
        },
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn templates_sorted_and_found() {
        let names: Vec<&str> = super::TEMPLATES.iter().map(|t| t.0).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(super::find("Go").unwrap().1.contains("*.exe"));
        assert_eq!(super::find("rust").unwrap().0, "Rust");
        assert!(super::find("Nope").is_none());
    }
}
