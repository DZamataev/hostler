//! A raw contract-5 connection for the integration tests (P5 Task 1): it says hello and reads
//! frames only when the test asks, so a test sees exactly what the service sent.
#![allow(dead_code)]

use std::time::Duration;

use hostler::contract::wire::{Event, ServerFrame};
use interprocess::local_socket::tokio::{Stream, prelude::*};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines, ReadHalf, WriteHalf};

use super::common::Home;

/// A raw connection that said hello; it reads only when the test asks.
pub struct Raw {
    lines: Lines<BufReader<ReadHalf<Stream>>>,
    send: WriteHalf<Stream>,
    pub hello: Value,
}

impl Raw {
    pub async fn connect(home: &Home) -> Raw {
        let paths = home.paths();
        let name = hostler::service::server::socket_name(&paths.socket).unwrap();
        let stream = Stream::connect(name).await.unwrap();
        let (recv, mut send) = tokio::io::split(stream);
        let hello = format!(
            "{{\"id\":1,\"method\":\"hello\",\"params\":{{\"contract_version\":{},\"client\":\"raw\"}}}}\n",
            hostler::contract::CONTRACT_VERSION
        );
        send.write_all(hello.as_bytes()).await.unwrap();
        let mut raw = Raw {
            lines: BufReader::new(recv).lines(),
            send,
            hello: Value::Null,
        };
        let reply = raw.frame().await.expect("the hello reply");
        raw.hello = serde_json::from_str::<Value>(&reply).unwrap()["result"].clone();
        raw
    }

    async fn frame(&mut self) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .ok()?
            .unwrap()
    }

    /// One request; its answer frame (events before it are skipped).
    pub async fn call(&mut self, id: u64, method: &str, params: Option<&str>) -> Value {
        let line = match params {
            Some(p) => format!("{{\"id\":{id},\"method\":\"{method}\",\"params\":{p}}}\n"),
            None => format!("{{\"id\":{id},\"method\":\"{method}\"}}\n"),
        };
        self.send.write_all(line.as_bytes()).await.unwrap();
        loop {
            let line = self.frame().await.expect("an answer");
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }

    /// The next event frame within 10 s.
    pub async fn event(&mut self) -> (u64, Event) {
        loop {
            let line = self.frame().await.expect("an event within 10 s");
            if let Ok(ServerFrame::Event { event, seq }) = serde_json::from_str(&line) {
                return (seq, event);
            }
        }
    }

    /// Every event that arrives within `quiet` of the previous frame.
    pub async fn events_within(&mut self, quiet: Duration) -> Vec<(u64, Event)> {
        let mut got = Vec::new();
        for line in self.lines_within(quiet).await {
            if let Ok(ServerFrame::Event { event, seq }) = serde_json::from_str(&line) {
                got.push((seq, event));
            }
        }
        got
    }

    /// Every frame line that arrives within `quiet` of the previous one, as text.
    pub async fn lines_within(&mut self, quiet: Duration) -> Vec<String> {
        let mut got = Vec::new();
        while let Ok(Ok(Some(line))) = tokio::time::timeout(quiet, self.lines.next_line()).await {
            got.push(line);
        }
        got
    }
}

/// `f` on its own thread: `Home::call` starts its own runtime, which may not run inside the
/// test's `block_on`.
pub fn blocking<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| scope.spawn(f).join().unwrap())
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Every file under `dir`, as text.
pub fn files_text(dir: &std::path::Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            text.push_str(&files_text(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    text
}
