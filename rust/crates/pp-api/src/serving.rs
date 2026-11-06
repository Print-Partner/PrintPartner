//! Serving a router in tests with a teardown that is complete when `stop` returns.
use anyhow::Result;
use std::net::SocketAddr;

/// A service one connection holds. Its `released` token drops after `service`
/// does, so `Serving::stop` can tell when no connection holds a router clone.
#[derive(Clone)]
struct Held<S> {
    service: S,
    _released: tokio::sync::mpsc::Sender<()>,
}

impl<S, R> tower::Service<R> for Held<S>
where
    S: tower::Service<R>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        self.service.call(request)
    }
}

/// Wraps each service `make` builds in a [`Held`].
struct Tracked<M> {
    make: M,
    released: tokio::sync::mpsc::Sender<()>,
}

impl<M, T> tower::Service<T> for Tracked<M>
where
    M: tower::Service<T>,
    M::Future: Send + 'static,
    M::Response: Send + 'static,
    M::Error: Send + 'static,
{
    type Response = Held<M::Response>;
    type Error = M::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.make.poll_ready(cx)
    }

    fn call(&mut self, target: T) -> Self::Future {
        let made = self.make.call(target);
        let released = self.released.clone();
        Box::pin(async move {
            Ok(Held {
                service: made.await?,
                _released: released,
            })
        })
    }
}

/// A router served on a listener until `stop`. axum's graceful shutdown returns
/// while a connection task may still hold its router clone, and with it the
/// storage owner the router captured. `stop` therefore also waits until every
/// clone is dropped, so the caller can take sole ownership of what the router
/// held without a retry or a sleep.
pub struct Serving {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    released: tokio::sync::mpsc::Receiver<()>,
}

impl Serving {
    pub fn start(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
        let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
        let (token, released) = tokio::sync::mpsc::channel(1);
        let make = Tracked {
            make: router.into_make_service_with_connect_info::<SocketAddr>(),
            released: token,
        };
        let task = tokio::spawn(async move {
            axum::serve(listener, make)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
        });
        Self {
            shutdown: Some(shutdown),
            task,
            released,
        }
    }

    /// Stops the listener, drains its connections, and returns once no
    /// connection or accept loop holds the router.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        (&mut self.task).await??;
        while self.released.recv().await.is_some() {}
        Ok(())
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.task.abort();
    }
}
