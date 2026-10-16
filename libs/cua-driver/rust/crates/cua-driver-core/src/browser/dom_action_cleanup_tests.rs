//! Cleanup uses the real transport even when a synthetic key-up has no acknowledgement.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_tungstenite::tungstenite::Message;

async fn peer() -> (
    Arc<CdpConnection>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let keys = Arc::new(AtomicUsize::new(0));
    let groups = Arc::new(AtomicUsize::new(0));
    let key_count = keys.clone();
    let group_count = groups.clone();
    let released = Arc::new(tokio::sync::Notify::new());
    let signal = released.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let call: Value = serde_json::from_str(&text).unwrap();
            if call["method"] == "Runtime.callFunctionOn" {
                key_count.fetch_add(1, Ordering::SeqCst);
                continue; // Deliberately retain an unknown effect, not a negative acknowledgement.
            }
            if call["method"] == "Runtime.releaseObjectGroup" {
                group_count.fetch_add(1, Ordering::SeqCst);
                signal.notify_one();
            }
            socket
                .send(Message::Text(
                    json!({"id":call["id"],"result":{}}).to_string(),
                ))
                .await
                .unwrap();
        }
    });
    let conn = Arc::new(
        CdpConnection::connect(&format!("ws://{address}"))
            .await
            .unwrap(),
    );
    (conn, keys, groups, released, task)
}

#[tokio::test]
async fn unacknowledged_cleanup_keyup_does_not_skip_owned_object_group_release() {
    let (conn, keys, groups, _released, task) = peer().await;
    let confirmed = cleanup(
        &conn,
        "original-session",
        "owned-group",
        "original-unique",
        "original-node",
        Some(json!({"name":"keyup","argument":{"key":"Enter","code":"Enter","modifiers":0}})),
    )
    .await;
    assert!(!confirmed);
    assert_eq!(
        keys.load(Ordering::SeqCst),
        1,
        "cleanup never retries an unknown key-up"
    );
    assert_eq!(
        groups.load(Ordering::SeqCst),
        1,
        "group cleanup is independent of key-up acknowledgement"
    );
    task.abort();
}

#[tokio::test]
async fn dropping_the_cleanup_waiter_does_not_abort_or_restart_its_owner() {
    let (conn, keys, groups, released, task) = peer().await;
    let progress = DomActionProgress::default();
    let owner = spawn_cleanup(
        conn,
        "original-session".into(),
        "owned-group".into(),
        "original-unique".into(),
        "original-node".into(),
        Some(json!({"name":"keyup","argument":{"key":"Enter","code":"Enter","modifiers":0}})),
        progress.clone(),
        None,
    );
    // Dropping the actual join handle detaches its owner, never aborts or restarts it.
    drop(owner);
    tokio::time::timeout(Duration::from_secs(2), released.notified())
        .await
        .unwrap();
    assert_eq!(keys.load(Ordering::SeqCst), 1);
    assert_eq!(groups.load(Ordering::SeqCst), 1);
    assert_eq!(progress.snapshot()["cleanup"], "unconfirmed");
    task.abort();
}
