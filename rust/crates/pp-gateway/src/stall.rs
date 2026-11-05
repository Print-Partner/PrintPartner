//! Closes a connection when a write stays pending because the peer accepts no
//! bytes for [`WRITE_STALL`]. The server only polls a response body when the
//! socket can take more data, so without this a client that stops reading
//! would hold its response, and whatever that response holds, until it
//! disconnects. A peer that stops reading while socket buffers still have room
//! is not detected: its writes still succeed.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Sleep,
};

pub(crate) const WRITE_STALL: Duration = Duration::from_secs(30);

pub(crate) struct StallGuard<T> {
    inner: T,
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<T> StallGuard<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            stalled: None,
        }
    }

    /// Progress resets the timer; a write that stays pending past
    /// [`WRITE_STALL`] fails.
    fn track<R>(&mut self, cx: &mut Context<'_>, poll: Poll<io::Result<R>>) -> Poll<io::Result<R>> {
        if poll.is_ready() {
            self.stalled = None;
            return poll;
        }
        let timer = self
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(WRITE_STALL)));
        match timer.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "peer stopped reading",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for StallGuard<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for StallGuard<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.track(cx, poll)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let poll = Pin::new(&mut self.inner).poll_flush(cx);
        self.track(cx, poll)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test(start_paused = true)]
    async fn stall_guard_fails_a_write_the_peer_never_reads() {
        let (near, _far) = tokio::io::duplex(64);
        let mut guarded = StallGuard::new(near);
        let started = tokio::time::Instant::now();
        let error = guarded.write_all(&[0; 1024]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= WRITE_STALL);
    }

    #[tokio::test(start_paused = true)]
    async fn stall_guard_keeps_a_slow_reader_that_makes_progress() {
        let (near, mut far) = tokio::io::duplex(64);
        let reader = tokio::spawn(async move {
            let mut received = Vec::new();
            let mut chunk = [0; 64];
            while received.len() < 1024 {
                tokio::time::sleep(WRITE_STALL / 2).await;
                let read = far.read(&mut chunk).await.unwrap();
                received.extend_from_slice(&chunk[..read]);
            }
            received.len()
        });
        let mut guarded = StallGuard::new(near);
        guarded.write_all(&[0; 1024]).await.unwrap();
        assert_eq!(reader.await.unwrap(), 1024);
    }
}
