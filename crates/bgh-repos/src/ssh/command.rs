//! Parsing of SSH exec commands sent by git / git-lfs.

/// A supported exec request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    UploadPack {
        owner: String,
        repo: String,
    },
    ReceivePack {
        owner: String,
        repo: String,
    },
    /// `git-lfs-authenticate <repo> upload|download`
    LfsAuthenticate {
        owner: String,
        repo: String,
        upload: bool,
    },
}

impl Command {
    pub fn repo(&self) -> (&str, &str) {
        match self {
            Self::UploadPack { owner, repo }
            | Self::ReceivePack { owner, repo }
            | Self::LfsAuthenticate { owner, repo, .. } => (owner, repo),
        }
    }

    /// Whether the command needs write access.
    pub fn is_write(&self) -> bool {
        match self {
            Self::UploadPack { .. } => false,
            Self::ReceivePack { .. } => true,
            Self::LfsAuthenticate { upload, .. } => *upload,
        }
    }
}

/// Split like a POSIX shell for the subset git uses (`sq_quote`): single
/// quotes, double quotes, and backslash escapes outside single quotes.
pub fn split_words(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => cur.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => cur.push(chars.next()?),
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// `owner/repo[.git]` with optional leading `/` or `~/`.
pub fn parse_repo_path(path: &str) -> Option<(String, String)> {
    let p = path.trim_start_matches("~/").trim_matches('/');
    let (owner, repo) = p.split_once('/')?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    (ok(owner) && ok(repo)).then(|| (owner.to_string(), repo.to_string()))
}

/// Parse an exec command line. `Err` carries the message for the client.
pub fn parse(line: &str) -> Result<Command, String> {
    let words = split_words(line).ok_or("Invalid command quoting.")?;
    let mut it = words.iter().map(String::as_str);
    let mut verb = it.next().ok_or("No command given.")?.to_string();
    if verb == "git" {
        verb = format!("git-{}", it.next().ok_or("No git command given.")?);
    }
    let unknown = || format!("Invalid command: {verb}");
    match verb.as_str() {
        "git-upload-pack" | "git-receive-pack" | "git-lfs-authenticate" => {}
        "git-lfs-transfer" => {
            return Err("git-lfs-transfer is not supported; use the HTTP transfer.".into());
        }
        "git-upload-archive" => return Err("git-upload-archive is not supported.".into()),
        _ => return Err(unknown()),
    }
    let path = it.next().ok_or("No repository given.")?;
    let (owner, repo) = parse_repo_path(path).ok_or("Invalid repository path.")?;
    Ok(match verb.as_str() {
        "git-upload-pack" => Command::UploadPack { owner, repo },
        "git-receive-pack" => Command::ReceivePack { owner, repo },
        _ => {
            let upload = match it.next() {
                Some("upload") => true,
                Some("download") => false,
                _ => return Err("Usage: git-lfs-authenticate <repo> upload|download".into()),
            };
            Command::LfsAuthenticate {
                owner,
                repo,
                upload,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(o: &str, r: &str) -> Command {
        Command::UploadPack {
            owner: o.into(),
            repo: r.into(),
        }
    }

    #[test]
    fn parses_git_commands() {
        assert_eq!(
            parse("git-upload-pack 'alice/demo.git'"),
            Ok(up("alice", "demo"))
        );
        assert_eq!(
            parse("git upload-pack '/alice/demo'"),
            Ok(up("alice", "demo"))
        );
        assert_eq!(
            parse("git-receive-pack 'org/my.repo.git'"),
            Ok(Command::ReceivePack {
                owner: "org".into(),
                repo: "my.repo".into()
            })
        );
        assert_eq!(
            parse("git-lfs-authenticate alice/demo.git upload"),
            Ok(Command::LfsAuthenticate {
                owner: "alice".into(),
                repo: "demo".into(),
                upload: true
            })
        );
        assert!(parse("git-lfs-authenticate alice/demo.git delete").is_err());
        assert!(parse("ls -la").is_err());
        assert!(parse("git-upload-pack '../etc/passwd'").is_err());
        assert!(parse("git-upload-pack 'a/b/c'").is_err());
        assert!(parse("git-upload-pack 'unterminated").is_err());
        assert!(parse("git-lfs-transfer alice/demo.git download").is_err());
    }

    #[test]
    fn splits_quoted_words() {
        assert_eq!(
            split_words(r"a 'b c' d\ e 'it'\''s'").unwrap(),
            vec!["a", "b c", "d e", "it's"]
        );
    }
}
