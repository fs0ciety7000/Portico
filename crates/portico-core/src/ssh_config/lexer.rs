//! Line tokenizer mirroring OpenSSH: `strdelim` for the keyword (an `=`
//! may separate it from the value) then `argv_split(…, terminate_on_comment
//! = 1)` (`misc.c`) for the arguments, rule for rule:
//!
//! - arguments are separated by spaces and tabs;
//! - `"` or `'` opens a quote that only the same character closes;
//! - `\` followed by `'`, `"` or `\` (or by a space outside quotes) yields
//!   that character; **any other `\` is kept as is** (`C:\Users\me` is
//!   unchanged, `\\.\pipe\x` becomes `\.\pipe\x`: write the pipe
//!   `//./pipe/x`);
//! - `#` at the start of an argument ends the line;
//! - an unterminated quote is an error.

/// A tokenized, non-empty config line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    /// Lowercased keyword.
    pub keyword: String,
    pub args: Vec<String>,
}

/// What separates arguments (`argv_split`).
const ARG_SPACE: [char; 2] = [' ', '\t'];
/// What `strdelim` and the trailing trim treat as whitespace (`readconf.c`:
/// `WHITESPACE` and the end-of-line `\f`).
const LINE_SPACE: [char; 5] = [' ', '\t', '\r', '\n', '\x0c'];

/// Tokenizes one line. Returns `Ok(None)` for blank lines and comments.
///
/// Supports `Keyword value`, `Keyword=value`, `Keyword = value`, quotes and
/// escapes as described in the module doc.
pub(crate) fn tokenize(raw: &str) -> Result<Option<Line>, String> {
    let line = raw
        .trim_end_matches(LINE_SPACE)
        .trim_start_matches(LINE_SPACE);
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    // `strdelim`: the keyword ends at whitespace or `=`; one `=` (with
    // whitespace around it) may separate it from the arguments.
    let end = line
        .find(|c: char| LINE_SPACE.contains(&c) || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start_matches(LINE_SPACE);
    let rest = rest
        .strip_prefix('=')
        .map_or(rest, |r| r.trim_start_matches(LINE_SPACE));
    Ok(Some(Line {
        keyword: keyword.to_ascii_lowercase(),
        args: argv_split(rest)?,
    }))
}

/// OpenSSH `argv_split(s, …, terminate_on_comment = 1)`.
fn argv_split(s: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| ARG_SPACE.contains(c)).is_some() {}
        match chars.peek() {
            None | Some('#') => break,
            Some(_) => {}
        }
        let mut arg = String::new();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek().copied() {
                    Some(next @ ('\'' | '"' | '\\')) => {
                        arg.push(next);
                        chars.next();
                    }
                    Some(' ') if quote.is_none() => {
                        arg.push(' ');
                        chars.next();
                    }
                    // Unrecognised escape: the backslash stays.
                    _ => arg.push('\\'),
                }
            } else if quote.is_none() && ARG_SPACE.contains(&c) {
                break;
            } else if quote.is_none() && (c == '"' || c == '\'') {
                quote = Some(c);
            } else if quote == Some(c) {
                quote = None;
            } else {
                arg.push(c);
            }
        }
        if quote.is_some() {
            return Err("unterminated quoted string".to_owned());
        }
        args.push(arg);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(s: &str) -> Option<(String, Vec<String>)> {
        tokenize(s).unwrap().map(|l| (l.keyword, l.args))
    }

    #[test]
    fn forms() {
        let expected = Some(("hostname".to_owned(), vec!["example.com".to_owned()]));
        assert_eq!(tok("HostName example.com"), expected);
        assert_eq!(tok("HostName=example.com"), expected);
        assert_eq!(tok("  HostName = example.com  "), expected);
        assert_eq!(tok("\tHostName\texample.com # trailing"), expected);
    }

    #[test]
    fn comments_and_blank() {
        assert_eq!(tok(""), None);
        assert_eq!(tok("   "), None);
        assert_eq!(tok("# Host foo"), None);
    }

    #[test]
    fn quotes() {
        assert_eq!(
            tok(r#"IdentityFile "~/.ssh/my key""#),
            Some(("identityfile".into(), vec!["~/.ssh/my key".into()]))
        );
        assert!(tokenize(r#"IdentityFile "oops"#).is_err());
        assert!(tokenize("IdentityFile 'oops").is_err());
    }

    /// `argv_split` of OpenSSH, case by case.
    #[test]
    fn argv_split_parity() {
        let cases: &[(&str, &[&str])] = &[
            // Windows paths: unrecognised escapes keep their backslash.
            (
                r"IdentityFile C:\Users\alice\.ssh\id_work",
                &[r"C:\Users\alice\.ssh\id_work"],
            ),
            (r"IdentityFile C:\\Users\\bob\\id", &[r"C:\Users\bob\id"]),
            (
                r"IdentityAgent \\.\pipe\openssh-ssh-agent",
                &[r"\.\pipe\openssh-ssh-agent"],
            ),
            (
                "IdentityAgent //./pipe/openssh-ssh-agent",
                &["//./pipe/openssh-ssh-agent"],
            ),
            (
                r#"UserKnownHostsFile "C:\Users\alice\My Files\kh" '/x y/z'"#,
                &[r"C:\Users\alice\My Files\kh", "/x y/z"],
            ),
            // Unix: an escaped space, single quotes, escaped quotes.
            (r"IdentityFile ~/my\ key", &["~/my key"]),
            ("IdentityFile 'a b'", &["a b"]),
            (r#"SetEnv A="x y""#, &["A=x y"]),
            (r#"SetEnv A=\"x"#, &[r#"A="x"#]),
            (r"SetEnv A=it\'s", &["A=it's"]),
            (r#"RemoteCommand "it's \"quoted\"""#, &[r#"it's "quoted""#]),
            (r#"RemoteCommand 'say "hi"'"#, &[r#"say "hi""#]),
            // Inside quotes, `\ ` is not an escape.
            (r#"IdentityFile "a\ b""#, &[r"a\ b"]),
            (r"IdentityFile 'a\\b'", &[r"a\b"]),
            // A trailing backslash stays.
            (r"IdentityFile a\", &[r"a\"]),
            // Empty quoted arguments are arguments.
            (r#"SendEnv "" x"#, &["", "x"]),
            // `#` only at the start of an argument.
            ("Host a#b # comment", &["a#b"]),
            ("Host a \t b", &["a", "b"]),
            // CRLF line ending.
            ("HostName example.com\r", &["example.com"]),
        ];
        for (line, expected) in cases {
            let (_, args) = tok(line).unwrap_or_default();
            assert_eq!(args, *expected, "{line}");
        }
    }

    #[test]
    fn multiple_args() {
        assert_eq!(
            tok("Host web-* !web-legacy db"),
            Some((
                "host".into(),
                vec!["web-*".into(), "!web-legacy".into(), "db".into()]
            ))
        );
    }

    /// The same lines through the real `ssh -G` (OpenSSH, or OpenSSH for
    /// Windows on CI `windows-latest`), when `ssh` is installed.
    #[test]
    fn same_arguments_as_ssh_dash_g() {
        let lines = [
            r"IdentityFile /a\ b",
            r"IdentityFile /g\\h",
            r"IdentityFile /i\j",
            r#"IdentityFile "/k\"l""#,
            r"IdentityFile C:\Users\alice\id",
            r"IdentityFile C:\\Users\\bob\\id",
            r#"UserKnownHostsFile "/c d" '/e f'"#,
        ];
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let mut text = String::from("Host parity\n");
        for line in lines {
            text.push_str("  ");
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(&config, &text).unwrap();
        let Ok(output) = std::process::Command::new("ssh")
            .arg("-G")
            .arg("-F")
            .arg(&config)
            .arg("parity")
            .stdin(std::process::Stdio::null())
            .output()
        else {
            return; // no ssh client here
        };
        if !output.status.success() {
            // An ssh too old for -G, or one refusing the config file.
            return;
        }
        let shown = String::from_utf8_lossy(&output.stdout);
        let values = |key: &str| -> Vec<String> {
            shown
                .lines()
                .filter_map(|l| l.strip_prefix(key))
                .map(str::to_owned)
                .collect()
        };
        let mut identity_files = Vec::new();
        let mut known_hosts = Vec::new();
        for line in lines {
            let parsed = tokenize(line).unwrap().unwrap();
            match parsed.keyword.as_str() {
                "identityfile" => identity_files.extend(parsed.args),
                _ => known_hosts.push(parsed.args.join(" ")),
            }
        }
        assert_eq!(values("identityfile "), identity_files, "{shown}");
        assert_eq!(values("userknownhostsfile "), known_hosts, "{shown}");
    }
}
