//! A raw HTTP/1.1 mock of a chat-completions endpoint on 127.0.0.1 for the tests.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One scripted answer.
#[derive(Clone, Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
    pub headers: Vec<(String, String)>,
}

impl Reply {
    pub fn ok(body: String) -> Self {
        Self::status(200, body)
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            delay: Duration::ZERO,
            headers: Vec::new(),
        }
    }

    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A completion body in the shape the real API uses.
pub(crate) fn completion(content: &str, prompt_tokens: u64, completion_tokens: u64) -> String {
    serde_json::json!({
        "id": "mock-1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    })
    .to_string()
}

/// A request as the mock saw it.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub request_line: String,
    /// Header names are lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).expect("request body is JSON")
    }
}

/// Answers the n-th request with `replies[n]`; the last reply repeats once the script runs out.
pub(crate) struct MockServer {
    pub addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    pub async fn start(replies: Vec<Reply>) -> Self {
        assert!(!replies.is_empty());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(replies);
        {
            let hits = hits.clone();
            let recorded = recorded.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let (hits, recorded, replies) =
                        (hits.clone(), recorded.clone(), replies.clone());
                    tokio::spawn(async move {
                        let _ = serve(stream, hits, recorded, replies).await;
                    });
                }
            });
        }
        Self {
            addr,
            hits,
            recorded,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Requests received so far.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    pub fn request(&self, index: usize) -> Recorded {
        self.recorded.lock()[index].clone()
    }
}

async fn serve(
    mut stream: TcpStream,
    hits: Arc<AtomicUsize>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    replies: Arc<Vec<Reply>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    recorded.lock().push(Recorded {
        request_line,
        headers,
        body,
    });
    let n = hits.fetch_add(1, Ordering::SeqCst);
    let reply = replies[n.min(replies.len() - 1)].clone();

    tokio::time::sleep(reply.delay).await;
    let mut out = format!(
        "HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&reply.body);
    stream.write_all(out.as_bytes()).await?;
    stream.shutdown().await
}
