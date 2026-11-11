//! GRE-1044: streamed replies and the 8 MiB listing limit on `HttpGuard`,
//! against loopback servers only.
use pp_net::outbound::{
    BodyDeadline, HttpGuard, IdleTimeout, MAX_BODY, MAX_LISTING_BODY, StreamReply, SystemResolver,
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// A streamed GET with 5 s head and idle deadlines.
async fn open(url: &str) -> StreamReply {
    guard()
        .stream(
            url,
            &[],
            Duration::from_secs(5),
            BodyDeadline::Idle(Duration::from_secs(5)),
        )
        .await
        .unwrap()
}

fn guard() -> HttpGuard {
    HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), MAX_BODY).unwrap()
}

/// Binds a loopback listener for `path` and answers its first request with
/// a 200 head of `kind` and `length`, returning the socket to write the body.
async fn answer(
    path: &str,
    kind: &'static str,
    length: usize,
) -> (String, impl Future<Output = TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}{path}", listener.local_addr().unwrap());
    let socket = async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut head = [0; 1024];
        let _ = socket.read(&mut head).await;
        let reply =
            format!("HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {length}\r\n\r\n");
        socket.write_all(reply.as_bytes()).await.unwrap();
        socket
    };
    (url, socket)
}

/// Answers one request with `length` bytes of body, writing `sent` of them
/// and then holding the connection open without writing more.
async fn server(length: usize, sent: usize) -> String {
    let (url, socket) = answer("/file", "application/octet-stream", length).await;
    tokio::spawn(async move {
        let mut socket = socket.await;
        let body = vec![b'x'; sent];
        for part in body.chunks(64 * 1024) {
            if socket.write_all(part).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    url
}

#[tokio::test]
async fn ticket_t_22_stream_reads_a_body_past_the_buffered_limit() {
    let length = 3 * MAX_BODY;
    let url = server(length, length).await;
    let mut reply = open(&url).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_length, Some(length as u64));
    assert_eq!(
        reply.content_type.as_deref(),
        Some("application/octet-stream")
    );
    let mut total = 0;
    while let Some(chunk) = reply.body.chunk().await.unwrap() {
        total += chunk.len();
    }
    assert_eq!(total, length);

    let url = server(length, length).await;
    assert!(
        guard().get(&url, None).await.is_err(),
        "buffered reads keep 2 MiB"
    );
}

#[tokio::test]
async fn ticket_t_22_stream_fails_after_the_idle_deadline() {
    let url = server(1000, 10).await;
    let mut reply = guard()
        .stream(
            &url,
            &[],
            Duration::from_secs(5),
            BodyDeadline::Idle(Duration::from_millis(200)),
        )
        .await
        .unwrap();
    assert_eq!(reply.body.chunk().await.unwrap().unwrap().len(), 10);
    let stalled = reply.body.chunk().await.unwrap_err();
    assert!(
        stalled.downcast_ref::<IdleTimeout>().is_some(),
        "{stalled:#}"
    );
}

#[tokio::test]
async fn ticket_t_22_prefix_stops_reading_at_its_limit() {
    let url = server(4 * MAX_BODY, 4 * MAX_BODY).await;
    let reply = open(&url).await;
    assert_eq!(reply.body.prefix(65_536).await.unwrap().len(), 65_536);
}

#[tokio::test]
async fn ticket_t_22_listing_limit_allows_eight_mib_and_no_more() {
    assert!(guard().with_body_limit(MAX_LISTING_BODY + 1).is_err());
    assert!(guard().with_body_limit(0).is_err());
    let listing = guard().with_body_limit(MAX_LISTING_BODY).unwrap();
    let url = server(MAX_LISTING_BODY, MAX_LISTING_BODY).await;
    assert_eq!(
        listing.get(&url, None).await.unwrap().body.len(),
        MAX_LISTING_BODY
    );
    let url = server(MAX_LISTING_BODY + 1, MAX_LISTING_BODY + 1).await;
    assert!(listing.get(&url, None).await.is_err());
}

#[tokio::test]
async fn ticket_t_22_stream_refuses_reserved_headers() {
    let error = guard()
        .stream(
            "http://127.0.0.1:9/file",
            &[("Host", "other.test")],
            Duration::from_secs(1),
            BodyDeadline::Total,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Reserved request header");
}

/// Answers one request with an endless body, one byte every 50 ms.
async fn trickle() -> String {
    let (url, socket) = answer("/snap", "image/jpeg", 1_000_000).await;
    tokio::spawn(async move {
        let mut socket = socket.await;
        while socket.write_all(b"x").await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    url
}

async fn read_for(mut reply: StreamReply, cap: Duration) -> anyhow::Result<usize> {
    let started = tokio::time::Instant::now();
    let mut total = 0;
    while started.elapsed() < cap {
        match reply.body.chunk().await? {
            Some(chunk) => total += chunk.len(),
            None => break,
        }
    }
    Ok(total)
}

#[tokio::test]
async fn ticket_t_22_total_deadline_covers_a_trickling_body() {
    let url = trickle().await;
    let started = tokio::time::Instant::now();
    let reply = guard()
        .stream(&url, &[], Duration::from_millis(400), BodyDeadline::Total)
        .await
        .unwrap();
    let error = read_for(reply, Duration::from_secs(5)).await.unwrap_err();
    assert_eq!(error.to_string(), "HTTP body deadline");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    let url = trickle().await;
    let reply = guard()
        .stream(
            &url,
            &[],
            Duration::from_millis(400),
            BodyDeadline::Idle(Duration::from_millis(400)),
        )
        .await
        .unwrap();
    assert!(
        read_for(reply, Duration::from_millis(1200)).await.unwrap() > 0,
        "an idle deadline lets a steady body run past the head deadline"
    );
}
