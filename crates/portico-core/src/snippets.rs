//! Command snippets: reusable commands, global or scoped to hosts / tags.
//!
//! Stored in `snippets.toml`. Snippets may contain `{{placeholders}}` that the
//! UI asks for before sending. Snippet bodies are typed into the PTY, never run
//! locally.

use serde::{Deserialize, Serialize};

use crate::hosts::HostEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "values", rename_all = "snake_case")]
pub enum SnippetScope {
    Global,
    Hosts(Vec<String>),
    Tags(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    /// Stable slug, unique in the file.
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    pub scope: SnippetScope,
    /// Append a newline (execute) instead of just pasting.
    #[serde(default = "default_true")]
    pub run: bool,
}

fn default_true() -> bool {
    true
}

/// Content of `snippets.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnippetFile {
    #[serde(rename = "snippet")]
    pub snippets: Vec<Snippet>,
}

impl Snippet {
    pub fn applies_to(&self, host: &HostEntry) -> bool {
        match &self.scope {
            SnippetScope::Global => true,
            SnippetScope::Hosts(aliases) => aliases.contains(&host.alias),
            SnippetScope::Tags(tags) => tags
                .iter()
                .any(|t| host.meta.tags.iter().any(|ht| ht.eq_ignore_ascii_case(t))),
        }
    }

    /// Distinct `{{name}}` placeholders, in order of appearance.
    pub fn placeholders(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        let mut rest = self.command.as_str();
        while let Some(start) = rest.find("{{") {
            let after = rest.get(start + 2..).unwrap_or_default();
            let Some(end) = after.find("}}") else { break };
            let name = after.get(..end).unwrap_or_default().trim();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            rest = after.get(end + 2..).unwrap_or_default();
        }
        out
    }

    /// Substitutes placeholders; unknown ones are left untouched.
    pub fn render(&self, values: &[(&str, &str)]) -> String {
        let mut out = self.command.clone();
        for (k, v) in values {
            out = out.replace(&format!("{{{{{k}}}}}"), v);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_and_render() {
        let s = Snippet {
            id: "logs".into(),
            name: "Tail logs".into(),
            command: "journalctl -u {{unit}} -n {{ lines }} | grep {{unit}}".into(),
            description: None,
            scope: SnippetScope::Global,
            run: true,
        };
        assert_eq!(s.placeholders(), ["unit", "lines"]);
        assert_eq!(
            s.render(&[("unit", "nginx")]),
            "journalctl -u nginx -n {{ lines }} | grep nginx"
        );
    }

    #[test]
    fn toml_roundtrip() {
        let file = SnippetFile {
            snippets: vec![Snippet {
                id: "df".into(),
                name: "Disk usage".into(),
                command: "df -h".into(),
                description: Some("Human readable".into()),
                scope: SnippetScope::Tags(vec!["prod".into()]),
                run: true,
            }],
        };
        let text = toml::to_string_pretty(&file).unwrap();
        assert_eq!(toml::from_str::<SnippetFile>(&text).unwrap(), file);
    }
}
