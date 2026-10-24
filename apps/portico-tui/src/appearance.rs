//! Terminal background detection for the `auto` theme (phase 10).
//!
//! At startup, only when the theme setting is `auto`, Portico asks the
//! terminal for its background colour (OSC 11) followed by a Primary Device
//! Attributes request (DA1, `ESC [ c`), which every terminal answers. The
//! replies come back in order, so:
//! - an OSC 11 reply before the DA1 reply gives the colour: light when its
//!   CIE lightness L* is above 50;
//! - a DA1 reply alone means "unsupported", known at once (no timeout);
//! - nothing within [`QUERY_TIMEOUT`] (a terminal that answers nothing):
//!   the query gives up.
//!
//! Without an answer, `COLORFGBG` (rxvt, Konsole, some others) decides; and
//! without it, the system is taken as dark. The detection runs once: a
//! terminal that changes its background later keeps the theme chosen at
//! startup (restart Portico, or pick a theme with `t`).
//!
//! Invariants: the query runs in raw mode before Portico reads any input
//! (`crossterm`'s event reader is not started yet); bytes are read straight
//! from the stdin descriptor (no buffering that would steal keystrokes from
//! crossterm), and reading stops at the DA1 reply. Keys typed during these
//! few milliseconds are dropped, never interpreted. Blocking: call it from
//! `spawn_blocking`.

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

use portico_theme::Appearance;

/// Longest wait for the terminal's answers. Local terminals answer in about
/// a millisecond; over SSH, a round-trip. Spent in full only by a terminal
/// that answers neither query.
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_millis(100);

/// Largest answer read: two short replies; anything longer is not ours.
const MAX_REPLY_BYTES: usize = 512;

/// `OSC 11 ; ? ST` then `CSI c` (DA1).
const QUERY: &[u8] = b"\x1b]11;?\x1b\\\x1b[c";

/// Where the appearance came from (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The terminal answered OSC 11.
    Osc11,
    /// `COLORFGBG`.
    ColorFgBg,
    /// No information: dark.
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Detection {
    pub(crate) appearance: Appearance,
    pub(crate) source: Source,
    pub(crate) elapsed: Duration,
}

/// Queries the terminal (see the module doc), then `COLORFGBG`, then dark.
pub(crate) fn detect() -> Detection {
    let started = Instant::now();
    let replied = query_terminal(QUERY_TIMEOUT);
    let (appearance, source) = match replied.as_deref().and_then(parse_osc11) {
        Some(appearance) => (appearance, Source::Osc11),
        None => match std::env::var("COLORFGBG")
            .ok()
            .as_deref()
            .and_then(parse_colorfgbg)
        {
            Some(appearance) => (appearance, Source::ColorFgBg),
            None => (Appearance::Dark, Source::Fallback),
        },
    };
    Detection {
        appearance,
        source,
        elapsed: started.elapsed(),
    }
}

/// Sends the queries and reads until the DA1 reply, the size cap or the
/// deadline. `None` when nothing could be written or read.
fn query_terminal(timeout: Duration) -> Option<Vec<u8>> {
    let mut out = std::io::stdout();
    out.write_all(QUERY).ok()?;
    out.flush().ok()?;
    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let deadline = Instant::now().checked_add(timeout)?;
    let mut reply = Vec::new();
    let mut chunk = [0u8; 128];
    while reply.len() < MAX_REPLY_BYTES && !has_da1_reply(&reply) {
        let left = deadline.checked_duration_since(Instant::now())?;
        let wait = rustix::event::Timespec::try_from(left).ok()?;
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&wait)) {
            Ok(0) => break,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }
        match rustix::io::read(fd, &mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => reply.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
    }
    Some(reply)
}

/// A complete DA1 reply: `ESC [ ?` … `c`.
fn has_da1_reply(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(at, w)| {
        w == b"\x1b[?"
            && bytes
                .get(at.saturating_add(3)..)
                .and_then(|rest| rest.iter().find(|b| !(b.is_ascii_digit() || **b == b';')))
                == Some(&b'c')
    })
}

/// The background of an OSC 11 reply (`ESC ] 11 ; rgb:R/G/B` ended by BEL
/// or ST, 1 to 4 hex digits per channel; `rgba:` too), as an appearance.
pub(crate) fn parse_osc11(bytes: &[u8]) -> Option<Appearance> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find("\x1b]11;")?;
    let rest = text.get(start.saturating_add(5)..)?;
    let end = rest.find(['\x07', '\x1b'])?;
    let spec = rest.get(..end)?;
    let channels = spec
        .strip_prefix("rgb:")
        .or_else(|| spec.strip_prefix("rgba:"))?;
    let mut values = channels.split('/').map(channel);
    let (r, g, b) = (values.next()??, values.next()??, values.next()??);
    Some(if lightness(r, g, b) > 50.0 {
        Appearance::Light
    } else {
        Appearance::Dark
    })
}

/// One channel, `h` to `hhhh`, scaled to 0..=1.
fn channel(hex: &str) -> Option<f64> {
    if hex.is_empty() || hex.len() > 4 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    // len ≤ 4, so the shift stays below 16.
    let max = (1u32 << (4 * u32::try_from(hex.len()).ok()?)) - 1;
    Some(f64::from(value) / f64::from(max))
}

/// CIE L* (0..=100) of an sRGB colour.
fn lightness(r: f64, g: f64, b: f64) -> f64 {
    let lin = |v: f64| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let y = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    if y > 0.008_856 {
        116.0 * y.cbrt() - 16.0
    } else {
        903.3 * y
    }
}

/// `COLORFGBG` = `fg;bg` or `fg;default;bg`: the last field is the ANSI
/// index of the background; 7 and 9–15 are light (rxvt's convention).
pub(crate) fn parse_colorfgbg(value: &str) -> Option<Appearance> {
    let bg: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    match bg {
        7 | 9..=15 => Some(Appearance::Light),
        0..=6 | 8 => Some(Appearance::Dark),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_replies() {
        let light = b"\x1b]11;rgb:f4f4/f1f1/ecec\x1b\\\x1b[?62;22c";
        assert_eq!(parse_osc11(light), Some(Appearance::Light));
        let dark = b"\x1b]11;rgb:0b0b/0c0c/0e0e\x07\x1b[?1;2c";
        assert_eq!(parse_osc11(dark), Some(Appearance::Dark));
        // Short channels, rgba, a key typed before the reply.
        assert_eq!(
            parse_osc11(b"x\x1b]11;rgb:f/f/f\x07"),
            Some(Appearance::Light)
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgba:ffff/ffff/ffff/ffff\x07"),
            Some(Appearance::Light)
        );
        // Mid grey: L* 53.6 for #808080 is light, #767676 (L* 49.4) is dark.
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:80/80/80\x07"),
            Some(Appearance::Light)
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:76/76/76\x07"),
            Some(Appearance::Dark)
        );
    }

    #[test]
    fn unsupported_or_garbled_osc11() {
        // DA1 only: the terminal ignored OSC 11.
        assert_eq!(parse_osc11(b"\x1b[?62;c"), None);
        assert_eq!(parse_osc11(b""), None);
        // Unterminated, wrong format, too many digits, not hex.
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ffff/ffff/ffff"), None);
        assert_eq!(parse_osc11(b"\x1b]11;#ffffff\x07"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:fffff/0/0\x07"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:zz/00/00\x07"), None);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ff/ff\x07"), None);
    }

    #[test]
    fn da1_reply_ends_the_read() {
        assert!(has_da1_reply(b"\x1b]11;rgb:0/0/0\x07\x1b[?62;22c"));
        assert!(has_da1_reply(b"\x1b[?6c"));
        assert!(!has_da1_reply(b"\x1b[?62;22"));
        assert!(!has_da1_reply(b"\x1b]11;rgb:0/0/0\x07"));
    }

    #[test]
    fn colorfgbg() {
        assert_eq!(parse_colorfgbg("0;15"), Some(Appearance::Light));
        assert_eq!(parse_colorfgbg("0;default;7"), Some(Appearance::Light));
        assert_eq!(parse_colorfgbg("15;0"), Some(Appearance::Dark));
        assert_eq!(parse_colorfgbg("7;8"), Some(Appearance::Dark));
        assert_eq!(parse_colorfgbg("15;default"), None);
        assert_eq!(parse_colorfgbg("0;99"), None);
        assert_eq!(parse_colorfgbg(""), None);
    }
}
