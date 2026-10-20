//! OpenSSH-style wildcard patterns (`*`, `?`, `!negation`, comma lists).

/// Matches `text` against a glob containing `*` (any run) and `?` (any char).
///
/// ASCII case-insensitive when `ignore_case` is set, as OpenSSH does for host
/// names. Linear-time greedy algorithm with single-star backtracking.
pub(crate) fn wildcard_match(pattern: &str, text: &str, ignore_case: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let eq = |a: char, b: char| {
        if ignore_case {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    };

    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match (p.get(pi), t.get(ti)) {
            (Some('*'), _) => {
                star = Some((pi, ti));
                pi += 1;
            }
            (Some(&pc), Some(&tc)) if pc == '?' || eq(pc, tc) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p.get(pi..)
        .is_some_and(|rest| rest.iter().all(|&c| c == '*'))
}

/// Returns `true` if the string contains glob metacharacters.
pub(crate) fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// One entry of a `Host` line or a `Match host` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    pub negated: bool,
    pub glob: String,
}

impl HostPattern {
    pub fn parse(raw: &str) -> Self {
        match raw.strip_prefix('!') {
            Some(rest) => Self {
                negated: true,
                glob: rest.to_owned(),
            },
            None => Self {
                negated: false,
                glob: raw.to_owned(),
            },
        }
    }

    /// A pattern that names exactly one host (shown in the host list).
    pub fn is_concrete(&self) -> bool {
        !self.negated && !has_wildcard(&self.glob)
    }
}

/// OpenSSH semantics: matches if at least one positive pattern matches and no
/// negated pattern matches.
pub(crate) fn match_pattern_list(patterns: &[HostPattern], text: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        if wildcard_match(&p.glob, text, true) {
            if p.negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Splits a comma-separated pattern list (`Match host a,b,!c`).
pub(crate) fn parse_comma_list(raw: &str) -> Vec<HostPattern> {
    raw.split(',')
        .filter(|s| !s.is_empty())
        .map(HostPattern::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", "", false));
        assert!(wildcard_match("*.example.com", "db.example.com", false));
        assert!(!wildcard_match("*.example.com", "example.com", false));
        assert!(wildcard_match("web-??", "web-01", false));
        assert!(!wildcard_match("web-??", "web-1", false));
        assert!(wildcard_match("a*b*c", "axxbyyc", false));
        assert!(!wildcard_match("a*b*c", "axxbyy", false));
        assert!(wildcard_match("PROD*", "prod-1", true));
        assert!(!wildcard_match("PROD*", "prod-1", false));
    }

    #[test]
    fn negation_wins() {
        let pats: Vec<_> = ["*.corp", "!bastion.corp"]
            .iter()
            .map(|s| HostPattern::parse(s))
            .collect();
        assert!(match_pattern_list(&pats, "db.corp"));
        assert!(!match_pattern_list(&pats, "bastion.corp"));
        assert!(!match_pattern_list(&pats, "db.home"));
    }

    #[test]
    fn only_negations_never_match() {
        let pats = vec![HostPattern::parse("!foo")];
        assert!(!match_pattern_list(&pats, "bar"));
    }
}
