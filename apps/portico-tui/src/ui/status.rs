//! Connection status → colour + label (shared by the host list and details).

use portico_core::events::{AuthMethod, SessionPhase};
use portico_theme::Rgb;

use crate::app::SessionView;
use crate::theme::Styles;

use std::time::Duration;

/// Above this RTT the latency is shown as a warning.
const SLOW_RTT: Duration = Duration::from_millis(250);

pub(crate) struct Status {
    pub(crate) color: Rgb,
    pub(crate) label: String,
    pub(crate) busy: bool,
    pub(crate) latency: Option<String>,
}

pub(crate) fn status(view: Option<&SessionView>, s: Styles) -> Status {
    let st = s.theme.status;
    let mk = |color, label: &str, busy| Status {
        color,
        label: label.to_owned(),
        busy,
        latency: None,
    };
    let Some(view) = view else {
        return mk(st.idle, "Disconnected", false);
    };
    match &view.phase {
        SessionPhase::Resolving => mk(st.connecting, "Resolving", true),
        SessionPhase::Connecting { hop, hops, target } if *hops > 1 => Status {
            label: format!("Connecting · {target} ({hop}/{hops})"),
            ..mk(st.connecting, "", true)
        },
        SessionPhase::Connecting { .. } => mk(st.connecting, "Connecting", true),
        SessionPhase::Handshake => mk(st.connecting, "Negotiating", true),
        SessionPhase::VerifyingHostKey => mk(st.authenticating, "Verifying host key", true),
        SessionPhase::Authenticating { method } => Status {
            label: format!("Authenticating · {}", method_label(*method)),
            ..mk(st.authenticating, "", true)
        },
        SessionPhase::Connected => {
            let slow = view.latency.is_some_and(|d| d > SLOW_RTT);
            Status {
                latency: view.latency.map(format_rtt),
                ..mk(
                    if slow { st.warning } else { st.connected },
                    "Connected",
                    false,
                )
            }
        }
        SessionPhase::Disconnecting => mk(st.idle, "Disconnecting", true),
        SessionPhase::Failed { error } => Status {
            label: format!("Failed · {}", error.code.replace('_', " ")),
            ..mk(st.error, "", false)
        },
        _ => mk(st.idle, "Disconnected", false),
    }
}

fn method_label(m: AuthMethod) -> &'static str {
    match m {
        AuthMethod::Agent => "agent",
        AuthMethod::Certificate => "certificate",
        AuthMethod::PublicKey => "key",
        AuthMethod::KeyboardInteractive => "interactive",
        AuthMethod::Password => "password",
        AuthMethod::None => "none",
    }
}

/// `<1 ms`, `4.2 ms`, `38 ms`.
pub(crate) fn format_rtt(d: Duration) -> String {
    let micros = d.as_micros();
    match micros {
        0..1_000 => "<1 ms".to_owned(),
        1_000..10_000 => format!("{}.{} ms", micros / 1_000, (micros % 1_000) / 100),
        _ => format!("{} ms", micros / 1_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtt_formatting() {
        assert_eq!(format_rtt(Duration::from_micros(300)), "<1 ms");
        assert_eq!(format_rtt(Duration::from_micros(4_250)), "4.2 ms");
        assert_eq!(format_rtt(Duration::from_millis(38)), "38 ms");
    }
}
