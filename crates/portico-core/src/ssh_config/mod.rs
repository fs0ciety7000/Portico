//! Native `~/.ssh/config` support.
//!
//! Portico **reads** the user's OpenSSH configuration and never rewrites it.
//! Portico-specific metadata (tags, favorites, groups) lives in its own store
//! (see [`crate::hosts`]), keyed by host alias.
//!
//! Supported: `Host` (wildcards, negation), `Match` (`all`, `host`,
//! `originalhost`, `user`, `localuser`), `Include` (globs in the file name,
//! recursion up to [`MAX_INCLUDE_DEPTH`]), first-obtained-value-wins
//! resolution, multi-valued keywords, `%`-token and `~` expansion, `ProxyJump`.
//!
//! Not evaluated (recorded as [`ConfigWarning`]s, treated as non-matching):
//! `Match exec`, `canonical`, `final`, `tagged`, `localnetwork`.

mod lexer;
pub(crate) mod pattern;
mod resolve;

use std::path::{Path, PathBuf};

pub use pattern::HostPattern;
pub use resolve::{JumpHost, ResolveContext, ResolvedHost, StrictHostKeyChecking, expand_home};

use lexer::tokenize;
use pattern::{has_wildcard, parse_comma_list, wildcard_match};

/// Maximum nesting of `Include` directives (OpenSSH uses 16).
pub const MAX_INCLUDE_DEPTH: usize = 16;

/// Keywords whose values accumulate instead of first-wins.
const MULTI_VALUED: &[&str] = &[
    "identityfile",
    "certificatefile",
    "localforward",
    "remoteforward",
    "dynamicforward",
    "sendenv",
    "setenv",
];

/// Errors that make a config file unusable.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}:{line}: {message}", file.display())]
    Syntax {
        file: PathBuf,
        line: usize,
        message: String,
    },
    #[error("{}:{line}: Include nested deeper than {MAX_INCLUDE_DEPTH} levels", file.display())]
    IncludeDepth { file: PathBuf, line: usize },
}

/// Where a directive came from (for "go to definition" and the debug panel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file: PathBuf,
    pub line: usize,
}

/// Non-fatal oddity found while parsing or resolving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub source: Source,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Directive {
    pub key: String,
    pub args: Vec<String>,
    pub source: Source,
}

/// A single `Match` criterion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MatchCriterion {
    All,
    Host(Vec<HostPattern>, bool),
    OriginalHost(Vec<HostPattern>, bool),
    User(Vec<HostPattern>, bool),
    LocalUser(Vec<HostPattern>, bool),
    /// Criterion we deliberately do not evaluate (e.g. `exec`).
    Unsupported(String),
}

/// Condition guarding a block. Blocks produced by an `Include` placed inside a
/// `Host`/`Match` carry their parent's condition too (all must hold).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Condition {
    Host(Vec<HostPattern>),
    Match(Vec<MatchCriterion>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    pub conditions: Vec<Condition>,
    pub directives: Vec<Directive>,
}

/// A host alias declared on a `Host` line (no wildcard, not negated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAlias {
    pub name: String,
    pub source: Source,
}

/// A parsed OpenSSH client configuration.
#[derive(Debug, Clone, Default)]
pub struct SshConfig {
    pub(crate) blocks: Vec<Block>,
    warnings: Vec<ConfigWarning>,
    /// Every file an `Include` resolved to (existing or not), in order.
    included: Vec<PathBuf>,
    /// Built on first use (blocks are complete by then).
    index: std::sync::OnceLock<BlockIndex>,
}

/// Which blocks can apply to an alias, so resolving every alias of a large
/// config does not test every block against every alias (quadratic).
/// A block whose only condition is `Host` with literal names (no wildcard,
/// no negation) can only apply to those names: it is indexed by them
/// (lowercased, matching is case-insensitive). Every other block is generic
/// and is always tested. Order is kept: resolving walks the merged block
/// numbers in file order, so OpenSSH's first-value-wins is unchanged.
#[derive(Debug, Clone, Default)]
pub(crate) struct BlockIndex {
    literal: std::collections::HashMap<String, Vec<usize>>,
    generic: Vec<usize>,
}

impl BlockIndex {
    fn build(blocks: &[Block]) -> Self {
        let mut index = Self::default();
        for (i, block) in blocks.iter().enumerate() {
            match block.conditions.as_slice() {
                [Condition::Host(patterns)]
                    if !patterns.is_empty() && patterns.iter().all(HostPattern::is_concrete) =>
                {
                    for p in patterns {
                        let list = index.literal.entry(p.glob.to_lowercase()).or_default();
                        if list.last() != Some(&i) {
                            list.push(i);
                        }
                    }
                }
                _ => index.generic.push(i),
            }
        }
        index
    }

    /// Block numbers that may apply to `alias`, in file order.
    pub(crate) fn candidates(&self, alias: &str) -> Vec<usize> {
        let literal = self
            .literal
            .get(&alias.to_lowercase())
            .map_or(&[][..], Vec::as_slice);
        let mut out = Vec::with_capacity(self.generic.len() + literal.len());
        let (mut g, mut l) = (self.generic.iter().peekable(), literal.iter().peekable());
        loop {
            let next = match (g.peek(), l.peek()) {
                (Some(&&a), Some(&&b)) if a <= b => g.next(),
                (Some(_) | None, Some(_)) => l.next(),
                (Some(_), None) => g.next(),
                (None, None) => break,
            };
            if let Some(&n) = next {
                out.push(n);
            }
        }
        out
    }
}

impl SshConfig {
    pub(crate) fn block_index(&self) -> &BlockIndex {
        self.index.get_or_init(|| BlockIndex::build(&self.blocks))
    }
}

impl SshConfig {
    /// Loads `path`. A missing file yields an empty configuration (like `ssh`).
    pub fn load(path: &Path, ssh_dir: &Path) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(cfg),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        let mut parser = Parser {
            ssh_dir,
            cfg: &mut cfg,
        };
        parser.parse(&text, path, &[], 0)?;
        Ok(cfg)
    }

    /// Parses config text. `origin` is used for error locations and `Include`
    /// resolution happens relative to `ssh_dir`.
    pub fn parse_str(text: &str, origin: &Path, ssh_dir: &Path) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        let mut parser = Parser {
            ssh_dir,
            cfg: &mut cfg,
        };
        parser.parse(text, origin, &[], 0)?;
        Ok(cfg)
    }

    /// Non-fatal issues found while parsing.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// Every file an `Include` directive resolved to, nested ones
    /// included, in reading order (a missing file is listed too: OpenSSH
    /// ignores it silently, but the directive is there).
    pub fn included_files(&self) -> &[PathBuf] {
        &self.included
    }

    /// Concrete host aliases in declaration order, de-duplicated.
    pub fn host_aliases(&self) -> Vec<HostAlias> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.all_concrete_aliases()
            .into_iter()
            .filter(|a| seen.insert(a.name.clone()))
            .collect()
    }

    /// Every occurrence of a concrete host alias, in declaration order, with
    /// the file and line of its block: an alias declared twice (in the
    /// import file and again in the user's own config) appears twice,
    /// unlike [`Self::host_aliases`] (first wins).
    pub fn all_concrete_aliases(&self) -> Vec<HostAlias> {
        let mut out: Vec<HostAlias> = Vec::new();
        for block in &self.blocks {
            // Only the block's own (last) condition declares names.
            let Some(Condition::Host(patterns)) = block.conditions.last() else {
                continue;
            };
            let source = block.directives.first().map_or_else(
                || Source {
                    file: PathBuf::new(),
                    line: 0,
                },
                |d| d.source.clone(),
            );
            for p in patterns.iter().filter(|p| p.is_concrete()) {
                out.push(HostAlias {
                    name: p.glob.clone(),
                    source: source.clone(),
                });
            }
        }
        out
    }
}

struct Parser<'a> {
    ssh_dir: &'a Path,
    cfg: &'a mut SshConfig,
}

impl Parser<'_> {
    fn parse(
        &mut self,
        text: &str,
        file: &Path,
        parent: &[Condition],
        depth: usize,
    ) -> Result<(), ConfigError> {
        // Directives before the first Host/Match in this file apply under the
        // parent condition only.
        let mut current = Block {
            conditions: parent.to_vec(),
            directives: Vec::new(),
        };

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let source = Source {
                file: file.to_owned(),
                line: line_no,
            };
            let line = tokenize(raw).map_err(|message| ConfigError::Syntax {
                file: file.to_owned(),
                line: line_no,
                message,
            })?;
            let Some(line) = line else { continue };

            match line.keyword.as_str() {
                "host" => {
                    if line.args.is_empty() {
                        return Err(syntax(file, line_no, "Host requires at least one pattern"));
                    }
                    self.flush(&mut current, parent);
                    let patterns = line.args.iter().map(|a| HostPattern::parse(a)).collect();
                    current.conditions.push(Condition::Host(patterns));
                    // Anchor the block's location even if it has no directives.
                    current.directives.push(Directive {
                        key: "#host".to_owned(),
                        args: line.args,
                        source,
                    });
                }
                "match" => {
                    self.flush(&mut current, parent);
                    let criteria = self.parse_match(&line.args, &source)?;
                    current.conditions.push(Condition::Match(criteria));
                }
                "include" => {
                    if depth + 1 > MAX_INCLUDE_DEPTH {
                        return Err(ConfigError::IncludeDepth {
                            file: file.to_owned(),
                            line: line_no,
                        });
                    }
                    // Flush so ordering between included and local directives is kept.
                    let conditions = current.conditions.clone();
                    self.flush(&mut current, parent);
                    current.conditions.clone_from(&conditions);
                    for arg in &line.args {
                        for path in self.expand_include(arg, &source) {
                            self.cfg.included.push(path.clone());
                            match std::fs::read_to_string(&path) {
                                Ok(text) => self.parse(&text, &path, &conditions, depth + 1)?,
                                // OpenSSH silently ignores missing includes.
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(source) => return Err(ConfigError::Read { path, source }),
                            }
                        }
                    }
                }
                _ => current.directives.push(Directive {
                    key: line.keyword,
                    args: line.args,
                    source,
                }),
            }
        }
        self.flush(&mut current, parent);
        Ok(())
    }

    /// Pushes `current` and resets it to an empty block under `parent`.
    fn flush(&mut self, current: &mut Block, parent: &[Condition]) {
        let block = std::mem::replace(
            current,
            Block {
                conditions: parent.to_vec(),
                directives: Vec::new(),
            },
        );
        if !block.directives.is_empty() {
            self.cfg.blocks.push(block);
        }
    }

    fn parse_match(
        &mut self,
        args: &[String],
        source: &Source,
    ) -> Result<Vec<MatchCriterion>, ConfigError> {
        let mut out = Vec::new();
        let mut iter = args.iter();
        while let Some(raw) = iter.next() {
            let (negate, word) = match raw.strip_prefix('!') {
                Some(w) => (true, w),
                None => (false, raw.as_str()),
            };
            let word = word.to_ascii_lowercase();
            let criterion = match word.as_str() {
                "all" => MatchCriterion::All,
                "canonical" | "final" => MatchCriterion::Unsupported(word),
                "host" | "originalhost" | "user" | "localuser" | "exec" | "localnetwork"
                | "tagged" => {
                    let Some(value) = iter.next() else {
                        return Err(syntax(
                            &source.file,
                            source.line,
                            &format!("Match {word} requires an argument"),
                        ));
                    };
                    let list = parse_comma_list(value);
                    match word.as_str() {
                        "host" => MatchCriterion::Host(list, negate),
                        "originalhost" => MatchCriterion::OriginalHost(list, negate),
                        "user" => MatchCriterion::User(list, negate),
                        "localuser" => MatchCriterion::LocalUser(list, negate),
                        _ => MatchCriterion::Unsupported(format!("{word} {value}")),
                    }
                }
                other => {
                    return Err(syntax(
                        &source.file,
                        source.line,
                        &format!("unknown Match criterion `{other}`"),
                    ));
                }
            };
            if let MatchCriterion::Unsupported(what) = &criterion {
                self.cfg.warnings.push(ConfigWarning {
                    source: source.clone(),
                    message: format!("Match `{what}` is not evaluated by Portico; block skipped"),
                });
            }
            out.push(criterion);
        }
        if out.is_empty() {
            return Err(syntax(&source.file, source.line, "Match requires criteria"));
        }
        Ok(out)
    }

    /// Expands `~` and relative paths, then globs the final path component.
    fn expand_include(&mut self, arg: &str, source: &Source) -> Vec<PathBuf> {
        let path = resolve::expand_tilde(arg, self.ssh_dir.parent());
        let path = if path.is_absolute() {
            path
        } else {
            self.ssh_dir.join(path)
        };
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            return Vec::new();
        };
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if has_wildcard(&dir.to_string_lossy()) {
            self.cfg.warnings.push(ConfigWarning {
                source: source.clone(),
                message: format!("Include `{arg}`: wildcards are only supported in the file name"),
            });
            return Vec::new();
        }
        if !has_wildcard(&name) {
            return vec![path];
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut matches: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file() || t.is_symlink()))
            .filter(|e| wildcard_match(&name, &e.file_name().to_string_lossy(), false))
            .map(|e| e.path())
            .collect();
        matches.sort();
        matches
    }
}

fn syntax(file: &Path, line: usize, message: &str) -> ConfigError {
    ConfigError::Syntax {
        file: file.to_owned(),
        line,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
