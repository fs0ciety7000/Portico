//! Links in a terminal (phase 9): `open_url` opens what the user confirmed
//! in the view ("Open this link?") in the system browser.
//!
//! Invariants (ADR "Liens du terminal", docs/STACK.md):
//! - the URL is validated again here, whatever the view checked: at most
//!   [`MAX_LINK_CHARS`] characters, no control or white space, parsed with
//!   `url`, scheme `http` or `https` only, a host, no credentials
//!   (`user:pass@`); a refusal has the code `link_refused`;
//! - the parsed (normalized) form is opened, never the raw text: an
//!   internationalized host is in its punycode form, as the dialog showed it;
//! - the opener is the platform's (`open` on macOS, `xdg-open` elsewhere),
//!   started without a shell with the URL as its only argument (it starts
//!   with `http`, never with `-`); no webview permission is involved;
//! - only the host is logged, never the path or the query string.

use std::process::{Command, Stdio};

use portico_core::ErrorSummary;

/// Longest URL accepted, in characters (the view refuses longer ones with
/// "Link too long to open").
pub(crate) const MAX_LINK_CHARS: usize = 2048;

const REFUSED: &str = "link_refused";

fn refused(message: &str, hint: Option<&str>) -> ErrorSummary {
    ErrorSummary {
        code: REFUSED.into(),
        message: message.into(),
        hint: hint.map(Into::into),
    }
}

/// Bidi controls, zero-width characters and other invisible formatting.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// The link as it will be opened, or why it is refused.
pub(crate) fn checked_link(raw: &str) -> Result<url::Url, ErrorSummary> {
    if raw.chars().count() > MAX_LINK_CHARS {
        return Err(refused("link too long to open", None));
    }
    // The WHATWG parser silently drops tabs and line breaks: refuse them
    // instead, the user must open exactly what the dialog showed. Invisible
    // format characters (bidi, zero width) are refused too, like the view.
    if raw
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || is_invisible_format(c))
    {
        return Err(refused(
            "this link contains spaces or control characters",
            None,
        ));
    }
    let link = url::Url::parse(raw).map_err(|_| refused("this is not a valid link", None))?;
    if !matches!(link.scheme(), "http" | "https") {
        return Err(refused(
            "only http and https links can be opened",
            Some("Copy the address and open it yourself if you trust it."),
        ));
    }
    if !link.username().is_empty() || link.password().is_some() {
        return Err(refused(
            "links with a user name or password are never opened",
            Some("Copy the address and open it yourself if you trust it."),
        ));
    }
    if link.host_str().is_none_or(str::is_empty) {
        return Err(refused("this link has no host", None));
    }
    Ok(link)
}

/// The platform opener, started without a shell; output discarded.
fn launch(link: &str) -> std::io::Result<std::process::Child> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program)
        .arg(link)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Opens a confirmed link in the system browser.
#[tauri::command]
pub(crate) async fn open_url(url: String) -> Result<(), ErrorSummary> {
    let link = checked_link(&url)?;
    let host = link.host_str().unwrap_or_default().to_owned();
    tracing::info!(%host, "opening a terminal link in the system browser");
    let started = tauri::async_runtime::spawn_blocking(move || {
        let mut child = launch(link.as_str())?;
        // Reaped in the background: `xdg-open` may wait for the browser.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok::<(), std::io::Error>(())
    })
    .await;
    match started {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::warn!(%host, error = %e, "the system opener could not start");
            Err(ErrorSummary {
                code: "link_open_failed".into(),
                message: "the system browser could not be started".into(),
                hint: Some(if cfg!(target_os = "macos") {
                    "Copy the address and open it in your browser.".into()
                } else {
                    "Check that xdg-open is installed, or copy the address and open it in your browser.".into()
                }),
            })
        }
        Err(_) => Err(ErrorSummary {
            code: "link_open_failed".into(),
            message: "the system browser could not be started".into(),
            hint: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(raw: &str) -> String {
        checked_link(raw).map_or_else(|e| e.code, |_| String::new())
    }

    #[test]
    fn http_and_https_links_are_accepted_and_normalized() {
        let link = checked_link("HTTPS://Example.COM/docs?q=1#x").unwrap();
        assert_eq!(link.as_str(), "https://example.com/docs?q=1#x");
        assert_eq!(
            checked_link("http://10.0.0.2:8080/").unwrap().host_str(),
            Some("10.0.0.2")
        );
        assert_eq!(
            checked_link("http://[::1]/").unwrap().host_str(),
            Some("[::1]")
        );
        // An internationalized host is opened in the form the dialog showed.
        assert_eq!(
            checked_link("https://bücher.example/").unwrap().host_str(),
            Some("xn--bcher-kva.example")
        );
    }

    #[test]
    fn other_schemes_credentials_and_odd_text_are_refused() {
        for raw in [
            "file:///etc/passwd",
            "ssh://host",
            "javascript:alert(1)",
            "data:text/html,x",
            "portico://x",
            "mailto:a@b.c",
            "https://user:pass@example.com/",
            "https://user@example.com/",
            "https://:pass@example.com/",
            "https://exa\tmple.com/",
            "https://example.com/\nx",
            "https://example.com/\u{1b}[31m",
            " https://example.com/",
            "example.com",
            "https://",
            "",
        ] {
            assert_eq!(code(raw), REFUSED, "{raw:?}");
        }
    }

    #[test]
    fn length_is_capped_in_characters() {
        let base = "https://example.com/";
        let fits = format!("{base}{}", "a".repeat(MAX_LINK_CHARS - base.len()));
        assert!(checked_link(&fits).is_ok());
        let long = format!("{fits}a");
        let err = checked_link(&long).unwrap_err();
        assert_eq!(err.code, REFUSED);
        assert_eq!(err.message, "link too long to open");
    }

    #[test]
    fn refusals_never_echo_the_link() {
        let err = checked_link("https://user:secret@example.com/?token=abc").unwrap_err();
        assert!(!err.message.contains("secret") && !err.message.contains("token"));
        assert!(err.hint.is_some_and(|h| !h.contains("secret")));
    }

    #[test]
    fn format_characters_are_refused() {
        assert_eq!(code("https://example.com/\u{202E}x"), REFUSED);
        assert_eq!(code("https://exa\u{200B}mple.com/"), REFUSED);
    }
}
