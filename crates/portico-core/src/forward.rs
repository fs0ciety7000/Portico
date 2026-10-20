//! Port forwarding specs (`-L`, `-R`, later `-D`).
//!
//! Each running forward is an actor owned by its session; state changes are
//! published as [`crate::events::TunnelEvent`]s. Implementation: Phase 2.

use std::fmt;

use serde::{Deserialize, Serialize};

/// `[bind_address:]port`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForwardSpec {
    /// Listen locally, connect from the server to `target`.
    Local { bind: Endpoint, target: Endpoint },
    /// Ask the server to listen on `bind`, connect locally to `target`.
    Remote { bind: Endpoint, target: Endpoint },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid forward `{input}`: {reason}")]
pub struct ForwardParseError {
    pub input: String,
    pub reason: &'static str,
}

impl ForwardSpec {
    /// Parses OpenSSH `LocalForward` / `-L` syntax:
    /// `[bind_address:]port host:hostport` or `[bind_address:]port:host:hostport`.
    /// The bind address defaults to `localhost` (never `0.0.0.0` implicitly).
    pub fn parse_local(input: &str) -> Result<Self, ForwardParseError> {
        let (bind, target) = parse_pair(input)?;
        Ok(Self::Local { bind, target })
    }

    /// Same syntax as [`Self::parse_local`] for `RemoteForward` / `-R`.
    pub fn parse_remote(input: &str) -> Result<Self, ForwardParseError> {
        let (bind, target) = parse_pair(input)?;
        Ok(Self::Remote { bind, target })
    }
}

fn parse_pair(input: &str) -> Result<(Endpoint, Endpoint), ForwardParseError> {
    let err = |reason| ForwardParseError {
        input: input.to_owned(),
        reason,
    };
    let parts: Vec<String> = if input.split_whitespace().count() == 2 {
        input.split_whitespace().flat_map(split_colons).collect()
    } else {
        split_colons(input.trim())
    };
    let (bind_host, bind_port, host, port) = match parts.as_slice() {
        [bp, h, p] => ("localhost".to_owned(), bp, h, p),
        [bh, bp, h, p] => (bh.clone(), bp, h, p),
        _ => return Err(err("expected [bind:]port:host:hostport")),
    };
    let parse = |p: &str| p.parse::<u16>().ok().filter(|&p| p != 0);
    let bind_port = parse(bind_port).ok_or_else(|| err("invalid bind port"))?;
    let port = parse(port).ok_or_else(|| err("invalid target port"))?;
    let bind_host = if bind_host.is_empty() || bind_host == "*" {
        // OpenSSH: empty or `*` means all interfaces, explicitly requested.
        "0.0.0.0".to_owned()
    } else {
        bind_host
    };
    Ok((
        Endpoint {
            host: bind_host,
            port: bind_port,
        },
        Endpoint {
            host: host.clone(),
            port,
        },
    ))
}

/// Splits on `:` while keeping `[ipv6]` literals intact.
fn split_colons(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_brackets = false;
    for c in s.chars() {
        match c {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            ':' if !in_brackets => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(h: &str, p: u16) -> Endpoint {
        Endpoint {
            host: h.into(),
            port: p,
        }
    }

    #[test]
    fn parses_cli_and_config_forms() {
        let expected = ForwardSpec::Local {
            bind: ep("localhost", 8080),
            target: ep("db", 5432),
        };
        assert_eq!(ForwardSpec::parse_local("8080:db:5432").unwrap(), expected);
        assert_eq!(ForwardSpec::parse_local("8080 db:5432").unwrap(), expected);
        assert_eq!(
            ForwardSpec::parse_local("127.0.0.1:8080:[::1]:80").unwrap(),
            ForwardSpec::Local {
                bind: ep("127.0.0.1", 8080),
                target: ep("::1", 80)
            }
        );
        assert_eq!(
            ForwardSpec::parse_remote("*:9000:localhost:3000").unwrap(),
            ForwardSpec::Remote {
                bind: ep("0.0.0.0", 9000),
                target: ep("localhost", 3000)
            }
        );
        assert!(ForwardSpec::parse_local("8080").is_err());
        assert!(ForwardSpec::parse_local("0:db:1").is_err());
    }
}
