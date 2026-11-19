//! Serves one inbound HTTP/1 connection and closes it without losing the last
//! response.
//!
//! A socket closed while request bytes are still unread sends a reset. When a
//! server answers before it reads the whole body, for example a 413 for an
//! over-limit body, the client can lose that answer: on Windows it gets os
//! error 10053 instead. So once hyper is done with a connection, [`serve`]
//! shuts down the write side and reads and discards what the client still
//! sends, for up to [`LINGER`], before it closes (RFC 9112, section 9.6). An
//! upgraded connection belongs to its upgrade handler and closes as before.

use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Incoming},
    server::conn::http1::Builder,
    service::{Service, service_fn},
};
use hyper_util::rt::TokioIo;
use std::{
    error::Error,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    sync::oneshot,
};

/// How long a closing connection keeps reading what the client still sends.
pub const LINGER: Duration = Duration::from_secs(2);

/// Serve `io` with upgrades until hyper is done with it, then linger unless
/// the connection was upgraded. Dropping the future closes the connection at
/// once, as before.
pub async fn serve<I, S, B>(builder: &Builder, io: I, service: S)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: Service<Request<Incoming>, Response = Response<B>>,
    S::Error: Into<Box<dyn Error + Send + Sync>>,
    B: Body + 'static,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    // hyper upgrades a server connection on a 101, or on a 2xx to CONNECT. An
    // upgrade handler that already dropped its socket hands the transport back
    // too, so the response decides, not whether the transport came back.
    let upgraded = Arc::new(AtomicBool::new(false));
    let marks = upgraded.clone();
    let service = service_fn(move |request: Request<Incoming>| {
        let connect = request.method() == Method::CONNECT;
        let response = service.call(request);
        let marks = marks.clone();
        async move {
            let response = response.await?;
            let status = response.status();
            if status == StatusCode::SWITCHING_PROTOCOLS || (connect && status.is_success()) {
                marks.store(true, Ordering::Relaxed);
            }
            Ok::<_, S::Error>(response)
        }
    });
    let (back, mut returned) = oneshot::channel();
    let io = Returning {
        io: Some(io),
        back: Some(back),
    };
    let _ = builder
        .serve_connection(TokioIo::new(io), service)
        .with_upgrades()
        .await;
    // The connection future is gone, so the transport is back here unless an
    // upgrade handler still owns it.
    if let Ok(mut io) = returned.try_recv()
        && !upgraded.load(Ordering::Relaxed)
    {
        let _ = tokio::time::timeout(LINGER, async {
            let _ = io.shutdown().await;
            let mut discarded = [0; 8192];
            while matches!(io.read(&mut discarded).await, Ok(read) if read > 0) {}
        })
        .await;
    }
}

/// Hands the transport back through `back` when hyper drops it.
struct Returning<T> {
    io: Option<T>,
    back: Option<oneshot::Sender<T>>,
}

impl<T: Unpin> Returning<T> {
    fn io(&mut self) -> Pin<&mut T> {
        Pin::new(self.io.as_mut().expect("transport is present until drop"))
    }
}

impl<T> Drop for Returning<T> {
    fn drop(&mut self) {
        if let (Some(io), Some(back)) = (self.io.take(), self.back.take()) {
            let _ = back.send(io);
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Returning<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.io().poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Returning<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.io().poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.io().poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.as_ref().is_some_and(T::is_write_vectored)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io().poll_shutdown(cx)
    }
}
