//! GRE-1264: `http1::serve` keeps reading after an early answer, for a bounded
//! time, and leaves an upgraded connection to its handler.
use http_body_util::Empty;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    server::conn::http1::Builder,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use pp_net::http1::{LINGER, serve};
use std::{convert::Infallible, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::{Instant, timeout},
};

/// More than loopback socket buffers hold, so a client can only finish
/// writing it if the server keeps reading.
const OVER_LIMIT: usize = 32 * 1024 * 1024;

/// Accepts one connection and answers 413 without reading the body.
/// Resolves when `serve` returns.
async fn rejecting_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(|_: Request<Incoming>| async {
            let mut response = Response::new(Empty::<Bytes>::new());
            *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
            Ok::<_, Infallible>(response)
        });
        serve(&Builder::new(), stream, service).await;
    });
    (address, served)
}

fn post_head(address: &str, length: usize) -> String {
    format!("POST /upload HTTP/1.1\r\nhost: {address}\r\ncontent-length: {length}\r\n\r\n")
}

#[tokio::test]
async fn http1_early_413_reaches_a_client_that_sends_the_whole_body() {
    let (address, served) = rejecting_server().await;
    let mut client = TcpStream::connect(&address).await.unwrap();
    client
        .write_all(post_head(&address, OVER_LIMIT).as_bytes())
        .await
        .unwrap();
    client
        .write_all(&vec![b'x'; OVER_LIMIT])
        .await
        .expect("the server drains the body instead of resetting");
    client.shutdown().await.unwrap();
    let mut reply = String::new();
    client.read_to_string(&mut reply).await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 413 "), "{reply}");
    timeout(Duration::from_secs(1), served)
        .await
        .expect("the client's EOF ends the linger")
        .unwrap();
}

#[tokio::test]
async fn http1_linger_closes_a_client_that_keeps_sending() {
    let (address, served) = rejecting_server().await;
    let mut client = TcpStream::connect(&address).await.unwrap();
    client
        .write_all(post_head(&address, 1 << 40).as_bytes())
        .await
        .unwrap();
    let mut reply = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let read = client.read(&mut chunk).await.unwrap();
        if read == 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..read]);
    }
    let answered = Instant::now();
    assert!(reply.starts_with(b"HTTP/1.1 413 "));
    let failed = timeout(LINGER * 3, async {
        loop {
            if client.write_all(&[b'x'; 16 * 1024]).await.is_err() {
                return answered.elapsed();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the server closes once the linger ends");
    assert!(failed >= LINGER / 2, "closed after {failed:?}");
    served.await.unwrap();
}

/// What the upgrade handler does with the connection after the 101.
#[derive(Clone, Copy)]
enum Handler {
    /// Echoes four bytes, after `serve` has returned.
    Echo,
    /// Takes the upgraded socket and drops it at once.
    Drop,
    /// Never calls `hyper::upgrade::on`, so hyper drops the socket itself.
    Abandon,
}

/// Accepts one connection and answers it with a 101. Resolves when `serve`
/// returns.
async fn upgrading_server(handler: Handler) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let served = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = service_fn(move |mut request: Request<Incoming>| async move {
            match handler {
                Handler::Echo => {
                    tokio::spawn(async move {
                        let upgraded = hyper::upgrade::on(&mut request).await.unwrap();
                        let mut upgraded = TokioIo::new(upgraded);
                        let mut word = [0; 4];
                        upgraded.read_exact(&mut word).await.unwrap();
                        upgraded.write_all(&word).await.unwrap();
                    });
                }
                Handler::Drop => {
                    tokio::spawn(async move {
                        drop(hyper::upgrade::on(&mut request).await.unwrap());
                    });
                }
                Handler::Abandon => drop(request),
            }
            let mut response = Response::new(Empty::<Bytes>::new());
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            response
                .headers_mut()
                .insert("connection", "upgrade".parse().unwrap());
            response
                .headers_mut()
                .insert("upgrade", "echo".parse().unwrap());
            Ok::<_, Infallible>(response)
        });
        serve(&Builder::new(), stream, service).await;
    });
    (address, served)
}

/// Sends an upgrade request and reads the 101 head. The client's write side
/// stays open, so a lingering server would wait out the full `LINGER`.
async fn upgraded_client(address: &str) -> TcpStream {
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(
            format!(
                "GET /echo HTTP/1.1\r\nhost: {address}\r\nconnection: upgrade\r\nupgrade: echo\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        client.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    assert!(head.starts_with(b"HTTP/1.1 101 "));
    client
}

#[tokio::test]
async fn http1_upgrade_keeps_its_connection_and_skips_the_linger() {
    let (address, served) = upgrading_server(Handler::Echo).await;
    let mut client = upgraded_client(&address).await;
    timeout(LINGER / 4, served)
        .await
        .expect("serve returns without lingering after an upgrade")
        .unwrap();
    client.write_all(b"ping").await.unwrap();
    let mut echoed = [0; 4];
    client.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");
}

/// The handler's drop can hand the transport back before `serve` checks for
/// it. The 101 still marks the connection upgraded, so it closes at once.
#[tokio::test]
async fn http1_upgrade_closed_by_its_handler_skips_the_linger() {
    for handler in [Handler::Drop, Handler::Abandon] {
        let (address, served) = upgrading_server(handler).await;
        let mut client = upgraded_client(&address).await;
        timeout(LINGER / 4, served)
            .await
            .expect("serve returns without lingering after an upgrade")
            .unwrap();
        let mut rest = Vec::new();
        timeout(LINGER / 4, client.read_to_end(&mut rest))
            .await
            .expect("the upgraded socket closes at once")
            .unwrap();
    }
}
