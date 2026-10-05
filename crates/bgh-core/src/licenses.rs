//! Vendored license templates (choosealicense.com, MIT; see
//! `data/licenses/LICENSE.md`, re-vendor with `scripts/vendor-templates.sh`).
//!
//! Backs `GET /licenses`, repository `license` objects, `license_template`
//! on repository creation and license detection. Nothing is fetched at
//! runtime.

use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::models::api::LicenseSimple;
use crate::urls::Urls;

/// SPDX id GitHub reports for a license file it can't identify
/// (`key: "other"`).
pub const NOASSERTION: &str = "NOASSERTION";

/// One choosealicense.com license.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    /// Lowercase key, e.g. `mit`, `apache-2.0`.
    pub key: String,
    pub name: String,
    pub spdx_id: String,
    pub nickname: Option<String>,
    pub description: String,
    /// The `how` text ("Create a text file …").
    pub implementation: String,
    pub permissions: Vec<String>,
    pub conditions: Vec<String>,
    pub limitations: Vec<String>,
    pub featured: bool,
    /// Not in GitHub's "commonly used" list (`GET /licenses`).
    pub hidden: bool,
    /// Template text with `[year]` / `[fullname]` placeholders.
    pub body: String,
}

macro_rules! vendored {
    ($($key:literal),* $(,)?) => {
        &[$(($key, include_str!(concat!("../data/licenses/", $key, ".txt")))),*]
    };
}

const FILES: &[(&str, &str)] = vendored![
    "0bsd",
    "afl-3.0",
    "agpl-3.0",
    "apache-2.0",
    "artistic-2.0",
    "blueoak-1.0.0",
    "bsd-2-clause",
    "bsd-2-clause-patent",
    "bsd-3-clause",
    "bsd-3-clause-clear",
    "bsd-4-clause",
    "bsl-1.0",
    "cc-by-4.0",
    "cc-by-sa-4.0",
    "cc0-1.0",
    "cecill-2.1",
    "cern-ohl-p-2.0",
    "cern-ohl-s-2.0",
    "cern-ohl-w-2.0",
    "ecl-2.0",
    "epl-1.0",
    "epl-2.0",
    "eupl-1.1",
    "eupl-1.2",
    "gfdl-1.3",
    "gpl-2.0",
    "gpl-3.0",
    "isc",
    "lgpl-2.1",
    "lgpl-3.0",
    "lppl-1.3c",
    "mit",
    "mit-0",
    "mpl-2.0",
    "ms-pl",
    "ms-rl",
    "mulanpsl-2.0",
    "ncsa",
    "odbl-1.0",
    "ofl-1.1",
    "osl-3.0",
    "postgresql",
    "unlicense",
    "upl-1.0",
    "vim",
    "wtfpl",
    "zlib",
];

static ALL: LazyLock<Vec<License>> =
    LazyLock::new(|| FILES.iter().map(|(k, t)| parse(k, t)).collect());

/// Every vendored license, sorted by key.
pub fn all() -> &'static [License] {
    &ALL
}

/// License by key (case-insensitive), e.g. `mit`.
pub fn find(key: &str) -> Option<&'static License> {
    ALL.iter().find(|l| l.key.eq_ignore_ascii_case(key))
}

/// License by SPDX id (case-insensitive), e.g. `MIT`.
pub fn by_spdx(spdx: &str) -> Option<&'static License> {
    ALL.iter().find(|l| l.spdx_id.eq_ignore_ascii_case(spdx))
}

/// GitHub-style legacy node id for a license (`License{key}`).
pub fn node_id(key: &str) -> String {
    STANDARD.encode(format!("07:License{key}"))
}

/// `license-simple` for a stored `license_spdx_id` (`NOASSERTION` renders
/// GitHub's `other` license; unknown ids render as themselves).
pub fn simple(urls: &Urls, spdx: &str) -> LicenseSimple {
    if spdx == NOASSERTION {
        return LicenseSimple {
            key: "other".into(),
            name: "Other".into(),
            spdx_id: NOASSERTION.into(),
            url: None,
            node_id: node_id("other"),
        };
    }
    match by_spdx(spdx) {
        Some(l) => LicenseSimple {
            key: l.key.clone(),
            name: l.name.clone(),
            spdx_id: l.spdx_id.clone(),
            url: Some(urls.api(&format!("/licenses/{}", l.key))),
            node_id: node_id(&l.key),
        },
        None => LicenseSimple {
            key: spdx.to_ascii_lowercase(),
            name: spdx.to_string(),
            spdx_id: spdx.to_string(),
            url: None,
            node_id: node_id(&spdx.to_ascii_lowercase()),
        },
    }
}

/// The template filled in like GitHub does for `license_template`.
pub fn render(license: &License, year: i32, fullname: &str) -> String {
    license
        .body
        .replace("[year]", &year.to_string())
        .replace("[fullname]", fullname)
}

/// Parse a choosealicense `_licenses/*.txt` file: YAML front matter
/// (flat `key: value` pairs and `- item` lists) followed by the text.
fn parse(key: &str, text: &str) -> License {
    let rest = text.strip_prefix("---\n").expect("front matter");
    let (front, body) = rest.split_once("\n---\n").expect("front matter end");
    let mut l = License {
        key: key.to_string(),
        name: String::new(),
        spdx_id: String::new(),
        nickname: None,
        description: String::new(),
        implementation: String::new(),
        permissions: vec![],
        conditions: vec![],
        limitations: vec![],
        featured: false,
        hidden: true,
        body: body.trim_start_matches('\n').to_string(),
    };
    let mut list: Option<&str> = None;
    for line in front.lines() {
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            let item = item.trim().to_string();
            match list {
                Some("permissions") => l.permissions.push(item),
                Some("conditions") => l.conditions.push(item),
                Some("limitations") => l.limitations.push(item),
                _ => {}
            }
            continue;
        }
        if line.starts_with(' ') || line.is_empty() {
            continue; // nested maps (`using:`) and blank lines
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        list = None;
        match k {
            "title" => l.name = v,
            "spdx-id" => l.spdx_id = v,
            "nickname" => l.nickname = Some(v),
            "description" => l.description = v,
            "how" => l.implementation = v,
            "featured" => l.featured = v == "true",
            "hidden" => l.hidden = v != "false",
            "permissions" | "conditions" | "limitations" => list = Some(k),
            _ => {}
        }
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vendored_licenses() {
        assert_eq!(all().len(), FILES.len());
        let mit = find("MIT").unwrap();
        assert_eq!(mit.name, "MIT License");
        assert_eq!(mit.spdx_id, "MIT");
        assert!(mit.featured && !mit.hidden);
        assert_eq!(mit.permissions[0], "commercial-use");
        assert_eq!(mit.conditions, vec!["include-copyright"]);
        assert!(mit.body.starts_with("MIT License"));
        assert_eq!(by_spdx("gpl-3.0").unwrap().key, "gpl-3.0");
        assert_eq!(
            by_spdx("GPL-3.0").unwrap().nickname.as_deref(),
            Some("GNU GPLv3")
        );
        assert_eq!(all().iter().filter(|l| !l.hidden).count(), 13);
        for l in all() {
            assert!(!l.name.is_empty() && !l.spdx_id.is_empty(), "{}", l.key);
            assert!(!l.body.is_empty(), "{}", l.key);
        }
        assert_eq!(node_id("mit"), STANDARD.encode("07:Licensemit"));
        let text = render(mit, 2026, "Alice");
        assert!(text.contains("Copyright (c) 2026 Alice"));
    }
}
