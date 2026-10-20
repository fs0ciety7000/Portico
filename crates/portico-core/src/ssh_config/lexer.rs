//! Line tokenizer mirroring OpenSSH `argv_split(…, terminate_on_comment)`.

/// A tokenized, non-empty config line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    /// Lowercased keyword.
    pub keyword: String,
    pub args: Vec<String>,
}

/// Tokenizes one line. Returns `Ok(None)` for blank lines and comments.
///
/// Supports `Keyword value`, `Keyword=value`, `Keyword = value`, double quotes
/// (no escapes inside except `\"`), and `#` starting a token as end-of-line
/// comment.
pub(crate) fn tokenize(raw: &str) -> Result<Option<Line>, String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut chars = raw.chars().peekable();

    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        if first == '#' {
            break;
        }

        let mut token = String::new();
        let mut in_quotes = false;
        let mut quoted = false;
        while let Some(&c) = chars.peek() {
            if in_quotes {
                chars.next();
                match c {
                    '"' => in_quotes = false,
                    '\\' if chars.peek() == Some(&'"') => {
                        token.push('"');
                        chars.next();
                    }
                    _ => token.push(c),
                }
            } else if c.is_whitespace() {
                break;
            } else if c == '"' {
                chars.next();
                in_quotes = true;
                quoted = true;
            } else if c == '=' && tokens.is_empty() {
                // `Keyword=value`: '=' terminates the keyword.
                break;
            } else {
                chars.next();
                token.push(c);
            }
        }
        if in_quotes {
            return Err("unterminated quoted string".to_owned());
        }
        if !token.is_empty() || quoted {
            tokens.push(token);
        }
        // Swallow a single '=' separator after the keyword.
        if tokens.len() == 1 {
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if chars.peek() == Some(&'=') {
                chars.next();
            }
        }
    }

    let mut iter = tokens.into_iter();
    match iter.next() {
        None => Ok(None),
        Some(keyword) => Ok(Some(Line {
            keyword: keyword.to_ascii_lowercase(),
            args: iter.collect(),
        })),
    }
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
}
