use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;

use super::*;

fn ctx() -> ResolveContext {
    ResolveContext {
        local_user: "alice".into(),
        home: PathBuf::from("/home/alice"),
        user_override: None,
    }
}

fn parse(text: &str) -> SshConfig {
    SshConfig::parse_str(
        text,
        Path::new("/home/alice/.ssh/config"),
        Path::new("/home/alice/.ssh"),
    )
    .expect("valid config")
}

const SAMPLE: &str = r"
# Global defaults first
IdentitiesOnly yes

Host bastion
    HostName bastion.example.com
    User ops
    Port 2222

Host web-* !web-legacy
    ProxyJump bastion
    User deploy

Host web-01 web-02
    HostName %h.internal
    IdentityFile ~/.ssh/id_web
    LocalForward 8080 localhost:80

Host *
    User fallback
    IdentityFile ~/.ssh/id_ed25519
    ServerAliveInterval 30
";

#[test]
fn lists_concrete_aliases_in_order() {
    let names: Vec<_> = parse(SAMPLE)
        .host_aliases()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, ["bastion", "web-01", "web-02"]);
}

#[test]
fn first_value_wins_and_multi_values_accumulate() {
    let h = parse(SAMPLE).resolve("web-01", &ctx());
    assert_eq!(h.hostname, "web-01.internal");
    assert_eq!(h.user.as_deref(), Some("deploy"));
    assert_eq!(h.port, 22);
    assert!(h.identities_only);
    assert_eq!(
        h.identity_files,
        [
            PathBuf::from("/home/alice/.ssh/id_web"),
            PathBuf::from("/home/alice/.ssh/id_ed25519")
        ]
    );
    assert_eq!(h.local_forwards, ["8080 localhost:80"]);
    assert_eq!(h.server_alive_interval, Some(30));
    assert_eq!(
        h.proxy_jump,
        [JumpHost {
            user: None,
            host: "bastion".into(),
            port: None
        }]
    );
}

#[test]
fn negated_pattern_excludes_block() {
    let h = parse(SAMPLE).resolve("web-legacy", &ctx());
    assert!(h.proxy_jump.is_empty());
    assert_eq!(h.user.as_deref(), Some("fallback"));
}

#[test]
fn unknown_alias_gets_defaults() {
    let h = parse(SAMPLE).resolve("example.org", &ctx());
    assert_eq!(h.hostname, "example.org");
    assert_eq!(h.user.as_deref(), Some("fallback"));
}

#[test]
fn user_override_beats_config() {
    let mut c = ctx();
    c.user_override = Some("root".into());
    let h = parse(SAMPLE).resolve("bastion", &c);
    assert_eq!(h.user.as_deref(), Some("root"));
    assert_eq!(h.port, 2222);
}

#[test]
fn match_host_uses_resolved_hostname() {
    let cfg = parse(
        "Host db\n  HostName db.prod.corp\nMatch host *.prod.corp\n  User dba\nMatch originalhost db\n  Port 5022\n",
    );
    let h = cfg.resolve("db", &ctx());
    assert_eq!(h.user.as_deref(), Some("dba"));
    assert_eq!(h.port, 5022);
}

#[test]
fn match_exec_is_skipped_with_warning() {
    let cfg = parse("Match exec \"true\"\n  User nope\n");
    assert_eq!(cfg.warnings().len(), 1);
    assert_eq!(cfg.resolve("x", &ctx()).user, None);
}

#[test]
fn token_expansion() {
    let cfg = parse(
        "Host t\n  HostName real\n  User bob\n  Port 2200\n  IdentityFile ~/.ssh/%r@%h-%p_%u_%n_%%\n",
    );
    let h = cfg.resolve("t", &ctx());
    assert_eq!(
        h.identity_files,
        [PathBuf::from("/home/alice/.ssh/bob@real-2200_alice_t_%")]
    );
}

#[test]
fn proxy_jump_forms() {
    use super::resolve::parse_proxy_jump;
    let hops = parse_proxy_jump("ops@b1:2200,ssh://b2,[::1]:22").expect("valid");
    assert_eq!(
        hops,
        [
            JumpHost {
                user: Some("ops".into()),
                host: "b1".into(),
                port: Some(2200)
            },
            JumpHost {
                user: None,
                host: "b2".into(),
                port: None
            },
            JumpHost {
                user: None,
                host: "::1".into(),
                port: Some(22)
            },
        ]
    );
    assert!(parse_proxy_jump("none").expect("valid").is_empty());
    assert!(parse_proxy_jump("host:0").is_err());
}

#[test]
fn invalid_port_is_a_warning_not_a_failure() {
    let h = parse("Host p\n  Port nope\n").resolve("p", &ctx());
    assert_eq!(h.port, 22);
    assert_eq!(h.warnings.len(), 1);
}

#[test]
fn syntax_errors_carry_location() {
    let err = SshConfig::parse_str("Host\n", Path::new("cfg"), Path::new("/tmp")).unwrap_err();
    assert_eq!(err.to_string(), "cfg:1: Host requires at least one pattern");
}

#[test]
fn includes_with_glob_and_conditional_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ssh = dir.path();
    std::fs::create_dir(ssh.join("conf.d")).expect("mkdir");
    std::fs::write(
        ssh.join("conf.d/10-a.conf"),
        "Host a\n  HostName a.example\n",
    )
    .expect("w");
    std::fs::write(
        ssh.join("conf.d/20-b.conf"),
        "Host b\n  HostName b.example\n",
    )
    .expect("w");
    std::fs::write(ssh.join("conf.d/ignored.txt"), "Host zzz\n").expect("w");
    std::fs::write(ssh.join("scoped"), "User scoped\n").expect("w");
    std::fs::write(
        ssh.join("config"),
        "Include conf.d/*.conf\nInclude missing-file\nHost a\n  Include scoped\n",
    )
    .expect("w");

    let cfg = SshConfig::load(&ssh.join("config"), ssh).expect("load");
    let names: Vec<_> = cfg.host_aliases().into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(cfg.resolve("a", &ctx()).user.as_deref(), Some("scoped"));
    assert_eq!(cfg.resolve("b", &ctx()).user, None);
    assert_eq!(cfg.resolve("b", &ctx()).hostname, "b.example");
}

#[test]
fn include_recursion_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ssh = dir.path();
    std::fs::write(ssh.join("config"), "Include config\n").expect("w");
    let err = SshConfig::load(&ssh.join("config"), ssh).unwrap_err();
    assert!(matches!(err, ConfigError::IncludeDepth { .. }));
}

#[test]
fn missing_config_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = SshConfig::load(&dir.path().join("nope"), dir.path()).expect("load");
    assert!(cfg.host_aliases().is_empty());
}

#[test]
fn proxy_command_and_proxy_jump_first_wins() {
    let cfg = parse(
        "Host a\n  ProxyCommand nc %h %p\n  ProxyJump bastion\nHost b\n  ProxyJump bastion\n  ProxyCommand nc %h %p\nHost c\n  ProxyCommand none\nHost *\n  ProxyJump j\n",
    );
    let a = cfg.resolve("a", &ctx());
    assert_eq!(a.proxy_command.as_deref(), Some("nc a 22"));
    assert!(a.proxy_jump.is_empty());
    let b = cfg.resolve("b", &ctx());
    assert!(b.proxy_command.is_none());
    assert_eq!(b.proxy_jump.len(), 1);
    // `ProxyCommand none` wins the slot and disables any later ProxyJump.
    let c = cfg.resolve("c", &ctx());
    assert!(c.proxy_command.is_none() && c.proxy_jump.is_empty());
}

#[test]
fn identity_agent_expansion() {
    let cfg =
        parse("Host a\n  IdentityAgent ~/.1password/agent.sock\nHost b\n  IdentityAgent none\n");
    assert_eq!(
        cfg.resolve("a", &ctx()).identity_agent.as_deref(),
        Some("/home/alice/.1password/agent.sock")
    );
    assert_eq!(
        cfg.resolve("b", &ctx()).identity_agent.as_deref(),
        Some("none")
    );
    assert_eq!(super::resolve::expand_env("${PORTICO_UNSET_VAR}/x"), "/x");
}

/// Loading a large config (docs/PERF.md): run with
/// `cargo test -p portico-core --release large_config -- --ignored --nocapture`.
#[test]
#[ignore = "timing, run on demand"]
#[allow(clippy::print_stderr)] // the timing is the output
fn large_config_resolves_every_alias() {
    use std::fmt::Write;
    let mut text = String::from(
        "Host *\n    ServerAliveInterval 30\n\nMatch host *.internal\n    User ops\n\n",
    );
    for i in 0..2000 {
        let _ = write!(
            text,
            "Host h{i}\n    HostName h{i}.internal\n    Port {}\n\n",
            2000 + i
        );
    }
    let started = std::time::Instant::now();
    let config = parse(&text);
    let aliases = config.host_aliases();
    for a in &aliases {
        let _ = config.resolve(&a.name, &ctx());
    }
    eprintln!("2000 hosts: {:?}", started.elapsed());
    assert_eq!(aliases.len(), 2000);
}

#[test]
fn block_index_keeps_file_order_and_case() {
    // Generic and literal blocks interleaved: first value still wins in file
    // order, names match without case, a negated or wildcard block stays
    // generic.
    let config = parse(
        "Host *\n    Port 1\nHost Web\n    Port 2\n    User a\nHost * !db\n    User b\n\
         Host web\n    User c\n    HostName web.example\nHost db web\n    Compression yes\n",
    );
    let web = config.resolve("WEB", &ctx());
    assert_eq!(web.port, 1, "Host * comes first");
    assert_eq!(web.user.as_deref(), Some("a"));
    assert_eq!(web.hostname, "web.example");
    assert!(web.options.contains_key("compression"));
    let db = config.resolve("db", &ctx());
    assert_eq!(db.user, None, "`Host * !db` does not apply to db");
    assert!(db.options.contains_key("compression"));
}
