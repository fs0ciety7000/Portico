//! Server side of SOCKS5 (RFC 1928) for `-D` dynamic forwarding.
//!
//! Deliberately minimal and strict (see docs/STACK.md, ADR `-D`):
//!
//! - method `0x00` "no authentication" only; a client that offers nothing
//!   else gets `0xFF` and is closed;
//! - command `CONNECT` only; `BIND` and `UDP ASSOCIATE` get reply `0x07`;
//! - address types IPv4, IPv6 and domain name. A domain is **never resolved
//!   locally**: it is handed as is to the server's `direct-tcpip`, so DNS
//!   happens on the remote side (what `curl --socks5-hostname` expects);
//! - SOCKS4 / 4a is refused with its own "request rejected" reply, so the
//!   client reports a clear error instead of a protocol mismatch.
//!
//! Invariants: every read has a fixed upper bound (at most 257 bytes for the
//! greeting, 262 for the request), only the exact bytes of the handshake are
//! consumed (data a client pipelines after its request stays in the socket
//! for the tunnel), and the caller bounds the whole exchange with a timeout.

use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::forward::Endpoint;

const VERSION: u8 = 5;
const SOCKS4_VERSION: u8 = 4;
/// SOCKS4 reply: version 0, code 91 "request rejected or failed".
const SOCKS4_REJECTED: [u8; 8] = [0, 91, 0, 0, 0, 0, 0, 0];
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Reply codes of RFC 1928 §6 used by Portico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    Succeeded,
    GeneralFailure,
    /// The SSH server forbids the forward (`AllowTcpForwarding`, `PermitOpen`).
    NotAllowed,
    HostUnreachable,
    ConnectionRefused,
    CommandNotSupported,
    AddressTypeNotSupported,
}

impl Reply {
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Succeeded => 0x00,
            Self::GeneralFailure => 0x01,
            Self::NotAllowed => 0x02,
            Self::HostUnreachable => 0x04,
            Self::ConnectionRefused => 0x05,
            Self::CommandNotSupported => 0x07,
            Self::AddressTypeNotSupported => 0x08,
        }
    }
}

/// Why a client was turned away during the handshake (logged, per
/// connection; the tunnel keeps running).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum SocksError {
    #[error("not a SOCKS5 client (version byte {0:#04x})")]
    Version(u8),
    #[error("SOCKS4 client refused: configure the client for SOCKS5")]
    Socks4,
    #[error("the client offers no method Portico accepts (only \"no authentication\")")]
    NoAcceptableMethod,
    #[error("unsupported SOCKS command {0:#04x} (only CONNECT)")]
    Command(u8),
    #[error("unsupported SOCKS address type {0:#04x}")]
    AddressType(u8),
    #[error("malformed SOCKS request: {0}")]
    Malformed(&'static str),
    #[error("SOCKS handshake timed out")]
    Timeout,
    #[error("SOCKS handshake: {0}")]
    Io(#[from] std::io::Error),
}

/// Runs the handshake up to (not including) the final reply and returns the
/// destination the client asked for. Refusals are answered here, before the
/// error is returned; the caller sends [`reply`] once the channel opened or
/// failed.
pub(crate) async fn negotiate<S>(stream: &mut S) -> Result<Endpoint, SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Greeting: VER NMETHODS METHODS…
    let [version, count] = read_array::<_, 2>(stream).await?;
    match version {
        VERSION => {}
        SOCKS4_VERSION => {
            send(stream, &SOCKS4_REJECTED).await?;
            return Err(SocksError::Socks4);
        }
        other => return Err(SocksError::Version(other)),
    }
    let mut methods = [0_u8; 255];
    let methods = methods
        .get_mut(..usize::from(count))
        .ok_or(SocksError::Malformed("method count"))?;
    stream.read_exact(methods).await?;
    if !methods.contains(&METHOD_NO_AUTH) {
        send(stream, &[VERSION, METHOD_NONE_ACCEPTABLE]).await?;
        return Err(SocksError::NoAcceptableMethod);
    }
    send(stream, &[VERSION, METHOD_NO_AUTH]).await?;

    // Request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let [version, command, reserved, address_type] = read_array::<_, 4>(stream).await?;
    if version != VERSION {
        return refuse(
            stream,
            Reply::GeneralFailure,
            SocksError::Malformed("version"),
        )
        .await;
    }
    if reserved != 0 {
        return refuse(
            stream,
            Reply::GeneralFailure,
            SocksError::Malformed("reserved byte"),
        )
        .await;
    }
    // The whole request is read before any refusal (when its length is
    // known): closing on unread bytes would reset the connection and lose
    // the reply.
    let host = match address_type {
        ATYP_IPV4 => Some(Ipv4Addr::from(read_array::<_, 4>(stream).await?).to_string()),
        ATYP_IPV6 => Some(Ipv6Addr::from(read_array::<_, 16>(stream).await?).to_string()),
        ATYP_DOMAIN => {
            let [len] = read_array::<_, 1>(stream).await?;
            let mut name = [0_u8; 255];
            let name = name
                .get_mut(..usize::from(len))
                .ok_or(SocksError::Malformed("domain length"))?;
            stream.read_exact(name).await?;
            // An invalid name is refused below, after the port is read.
            Some(domain(name).unwrap_or_default())
        }
        _ => None,
    };
    let port = match host {
        Some(_) => u16::from_be_bytes(read_array::<_, 2>(stream).await?),
        None => 0,
    };
    if command != CMD_CONNECT {
        return refuse(
            stream,
            Reply::CommandNotSupported,
            SocksError::Command(command),
        )
        .await;
    }
    let Some(host) = host else {
        return refuse(
            stream,
            Reply::AddressTypeNotSupported,
            SocksError::AddressType(address_type),
        )
        .await;
    };
    if host.is_empty() {
        return refuse(
            stream,
            Reply::GeneralFailure,
            SocksError::Malformed("domain name"),
        )
        .await;
    }
    if port == 0 {
        return refuse(
            stream,
            Reply::GeneralFailure,
            SocksError::Malformed("port 0"),
        )
        .await;
    }
    Ok(Endpoint { host, port })
}

/// Final reply. The bound address is not meaningful through SSH: like
/// OpenSSH, Portico answers `0.0.0.0:0`.
pub(crate) async fn reply<S>(stream: &mut S, reply: Reply) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    send(
        stream,
        &[VERSION, reply.code(), 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0],
    )
    .await
}

/// A domain name as sent to the server: non-empty UTF-8 without control
/// characters or spaces (it ends up in logs and in the server's resolver).
fn domain(bytes: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(bytes).ok()?;
    let valid = !name.is_empty() && !name.chars().any(|c| c.is_control() || c.is_whitespace());
    valid.then(|| name.to_owned())
}

async fn refuse<S, T>(stream: &mut S, code: Reply, error: SocksError) -> Result<T, SocksError>
where
    S: AsyncWrite + Unpin,
{
    reply(stream, code).await?;
    Err(error)
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await
}

async fn read_array<S: AsyncRead + Unpin, const N: usize>(
    stream: &mut S,
) -> std::io::Result<[u8; N]> {
    let mut buf = [0_u8; N];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// Plays `client` against [`negotiate`]; returns its result and every
    /// byte the server answered.
    async fn run(client: &[u8]) -> (Result<Endpoint, SocksError>, Vec<u8>) {
        let (mut ours, mut theirs) = duplex(1024);
        ours.write_all(client).await.unwrap();
        // EOF after the scripted bytes: a truncated exchange fails cleanly.
        ours.shutdown().await.unwrap();
        let result = negotiate(&mut theirs).await;
        drop(theirs);
        let mut answer = Vec::new();
        ours.read_to_end(&mut answer).await.unwrap();
        (result, answer)
    }

    const HELLO: [u8; 3] = [5, 1, 0];
    const CHOSEN: [u8; 2] = [5, 0];

    fn request(command: u8, address: &[u8], port: u16) -> Vec<u8> {
        let mut out = HELLO.to_vec();
        out.extend([5, command, 0]);
        out.extend(address);
        out.extend(port.to_be_bytes());
        out
    }

    fn failure(code: u8) -> Vec<u8> {
        let mut out = CHOSEN.to_vec();
        out.extend([5, code, 0, 1, 0, 0, 0, 0, 0, 0]);
        out
    }

    fn target(result: Result<Endpoint, SocksError>) -> (String, u16) {
        let e = result.unwrap();
        (e.host, e.port)
    }

    #[tokio::test]
    async fn connect_ipv4_ipv6_and_domain() {
        let (r, answer) = run(&request(1, &[1, 127, 0, 0, 1], 8080)).await;
        assert_eq!(target(r), ("127.0.0.1".into(), 8080));
        assert_eq!(answer, CHOSEN, "no final reply before the channel opens");

        let mut v6 = vec![4];
        v6.extend(Ipv6Addr::LOCALHOST.octets());
        let (r, _) = run(&request(1, &v6, 443)).await;
        assert_eq!(target(r), ("::1".into(), 443));

        let mut name = vec![3, 15];
        name.extend(b"db.internal.lan");
        let (r, _) = run(&request(1, &name, 5432)).await;
        assert_eq!(target(r), ("db.internal.lan".into(), 5432));
    }

    #[tokio::test]
    async fn no_auth_is_picked_among_several_methods() {
        let mut bytes = vec![5, 3, 2, 1, 0];
        bytes.extend(&request(1, &[1, 10, 0, 0, 1], 22)[3..]);
        let (r, answer) = run(&bytes).await;
        assert_eq!(target(r), ("10.0.0.1".into(), 22));
        assert_eq!(answer, CHOSEN);
    }

    #[tokio::test]
    async fn other_methods_are_refused() {
        // Username/password only.
        let (r, answer) = run(&[5, 1, 2]).await;
        assert!(matches!(r, Err(SocksError::NoAcceptableMethod)));
        assert_eq!(answer, [5, 0xFF]);
        // No method at all.
        let (r, answer) = run(&[5, 0]).await;
        assert!(matches!(r, Err(SocksError::NoAcceptableMethod)));
        assert_eq!(answer, [5, 0xFF]);
    }

    #[tokio::test]
    async fn wrong_versions() {
        let (r, answer) = run(b"GET / HTTP/1.1\r\n").await;
        assert!(matches!(r, Err(SocksError::Version(b'G'))));
        assert!(answer.is_empty(), "nothing is said to a non-SOCKS client");
        // SOCKS4 CONNECT 127.0.0.1:80, user "u".
        let (r, answer) = run(&[4, 1, 0, 80, 127, 0, 0, 1, b'u', 0]).await;
        assert!(matches!(r, Err(SocksError::Socks4)));
        assert_eq!(answer, SOCKS4_REJECTED);
        // SOCKS5 greeting, then a request with another version.
        let mut bytes = HELLO.to_vec();
        bytes.extend([4, 1, 0, 1, 127, 0, 0, 1, 0, 80]);
        let (r, answer) = run(&bytes).await;
        assert!(matches!(r, Err(SocksError::Malformed("version"))));
        assert_eq!(answer, failure(0x01));
    }

    #[tokio::test]
    async fn only_connect_is_supported() {
        for command in [2, 3, 9] {
            let (r, answer) = run(&request(command, &[1, 127, 0, 0, 1], 80)).await;
            assert!(matches!(r, Err(SocksError::Command(c)) if c == command));
            assert_eq!(answer, failure(0x07));
        }
    }

    #[tokio::test]
    async fn unknown_address_type_and_bad_fields() {
        let (r, answer) = run(&request(1, &[2, 1, 2, 3, 4], 80)).await;
        assert!(matches!(r, Err(SocksError::AddressType(2))));
        assert_eq!(answer, failure(0x08));

        let (r, answer) = run(&request(1, &[3, 0], 80)).await;
        assert!(matches!(r, Err(SocksError::Malformed("domain name"))));
        assert_eq!(answer, failure(0x01));

        let (r, answer) = run(&request(1, &[3, 5, b'a', b'\n', b'b', b'.', b'c'], 80)).await;
        assert!(matches!(r, Err(SocksError::Malformed("domain name"))));
        assert_eq!(answer, failure(0x01));

        let (r, _) = run(&request(1, &[3, 2, 0xC3, 0x28], 80)).await;
        assert!(
            matches!(r, Err(SocksError::Malformed("domain name"))),
            "invalid UTF-8"
        );

        let (r, answer) = run(&request(1, &[1, 127, 0, 0, 1], 0)).await;
        assert!(matches!(r, Err(SocksError::Malformed("port 0"))));
        assert_eq!(answer, failure(0x01));

        let mut bytes = HELLO.to_vec();
        bytes.extend([5, 1, 7, 1, 127, 0, 0, 1, 0, 80]);
        let (r, answer) = run(&bytes).await;
        assert!(matches!(r, Err(SocksError::Malformed("reserved byte"))));
        assert_eq!(answer, failure(0x01));
    }

    #[tokio::test]
    async fn truncated_exchanges_fail_without_panicking() {
        let full = request(
            1,
            &[3, 9, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'o'],
            80,
        );
        for cut in 0..full.len() {
            let (r, _) = run(&full[..cut]).await;
            assert!(matches!(r, Err(SocksError::Io(_))), "cut at {cut}");
        }
        // Domain length longer than what follows.
        let (r, _) = run(&request(1, &[3, 200, b'a'], 80)).await;
        assert!(matches!(r, Err(SocksError::Io(_))));
        // More methods announced than sent.
        let (r, _) = run(&[5, 255, 0]).await;
        assert!(matches!(r, Err(SocksError::Io(_))));
    }

    #[tokio::test]
    async fn pipelined_data_stays_in_the_stream() {
        let (mut ours, mut theirs) = duplex(1024);
        let mut bytes = request(1, &[1, 127, 0, 0, 1], 80);
        bytes.extend(b"GET / HTTP/1.0\r\n\r\n");
        ours.write_all(&bytes).await.unwrap();
        ours.shutdown().await.unwrap();
        target(negotiate(&mut theirs).await);
        let mut rest = Vec::new();
        theirs.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"GET / HTTP/1.0\r\n\r\n");
    }

    #[tokio::test]
    async fn final_replies() {
        let (mut ours, mut theirs) = duplex(64);
        reply(&mut theirs, Reply::ConnectionRefused).await.unwrap();
        reply(&mut theirs, Reply::Succeeded).await.unwrap();
        drop(theirs);
        let mut answer = Vec::new();
        ours.read_to_end(&mut answer).await.unwrap();
        assert_eq!(
            answer,
            [5, 5, 0, 1, 0, 0, 0, 0, 0, 0, 5, 0, 0, 1, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(Reply::HostUnreachable.code(), 4);
        assert_eq!(Reply::NotAllowed.code(), 2);
        assert_eq!(Reply::GeneralFailure.code(), 1);
    }
}
