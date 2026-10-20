//! Transport wrapper that (1) reports EOF / I/O errors immediately, without
//! waiting for russh (which skips `Handler::disconnected` after a RST), and
//! (2) can be aborted so no russh task or socket outlives a failed session.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

pub(crate) struct WatchedStream<S> {
    inner: S,
    closed: CancellationToken,
    abort: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl<S> WatchedStream<S> {
    /// `closed` is cancelled on EOF / error; cancelling `abort` makes every
    /// further read and write fail.
    pub(crate) fn new(inner: S, closed: CancellationToken, abort: CancellationToken) -> Self {
        Self {
            inner,
            closed,
            abort: Box::pin(abort.cancelled_owned()),
        }
    }

    fn aborted(&mut self, cx: &mut Context<'_>) -> bool {
        self.abort.as_mut().poll(cx).is_ready()
    }
}

fn aborted_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "session closed")
}

impl<S: AsyncRead + Unpin> AsyncRead for WatchedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.aborted(cx) {
            return Poll::Ready(Err(aborted_error()));
        }
        let wanted = buf.remaining() > 0;
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Ok(())) if wanted && buf.filled().len() == before => self.closed.cancel(),
            Poll::Ready(Err(_)) => self.closed.cancel(),
            _ => {}
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WatchedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.aborted(cx) {
            return Poll::Ready(Err(aborted_error()));
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, data);
        if matches!(result, Poll::Ready(Err(_))) {
            self.closed.cancel();
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn eof_cancels_closed_and_abort_fails_io() {
        let (a, mut b) = tokio::io::duplex(64);
        let (closed, abort) = (CancellationToken::new(), CancellationToken::new());
        let mut s = WatchedStream::new(a, closed.clone(), abort.clone());
        b.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        s.read_exact(&mut buf).await.unwrap();
        assert!(!closed.is_cancelled());
        drop(b);
        assert_eq!(s.read(&mut buf).await.unwrap(), 0);
        assert!(closed.is_cancelled());

        abort.cancel();
        assert!(s.write_all(b"x").await.is_err());
    }
}
