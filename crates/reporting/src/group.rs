//! Bounded, demand-driven group summaries. This module has no collector access.
use crate::{backoff::Gate, wall_ms};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{io::Read, time::Duration};

pub const MAX_BYTES: u64 = 32_768;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub results: Vec<serde_json::Value>,
    pub total: usize,
    pub next: Option<usize>,
    pub group: Option<serde_json::Value>,
}
/// Credentials are deliberately absent from Debug and errors.
pub struct Client {
    base: String,
    hash: String,
    secret: String,
    agent: ureq::Agent,
    pub gate: Gate,
    pub page: Option<Page>,
    pub message: String,
    pub checked_ms: Option<u64>,
    etag: Option<String>,
    target: String,
    page_size: usize,
    /// Server-name search, scoped by the server to the selected group.
    pub search: String,
}
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupClient")
            .field("credentials", &"[redacted]")
            .finish()
    }
}
impl Client {
    pub(crate) fn new(base: String, hash: String, secret: String) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .max_redirects(0)
            .max_response_header_size(8192)
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(3)))
            .build()
            .into();
        Self {
            base,
            hash,
            secret,
            agent,
            gate: Gate::default(),
            page: None,
            message: "Waiting for Dataplicity".into(),
            checked_ms: None,
            etag: None,
            target: String::new(),
            page_size: 20,
            search: String::new(),
        }
    }
    pub fn remaining_seconds(&self) -> u64 {
        self.gate
            .not_before_ms
            .saturating_sub(wall_ms())
            .div_ceil(1000)
    }
    pub fn halted(&self) -> bool {
        self.gate.halted
    }
    /// Treat a new drill-down as navigation even when it uses the same page URL.
    /// poll clears the page and validator before checking the existing rate gate.
    pub fn require_fresh_page(&mut self) {
        self.target.clear();
    }
    /// One synchronous request, called by the UI's sole background worker.
    pub fn poll(&mut self, group: Option<&str>, offset: usize) -> Result<()> {
        let mut target = match group {
            Some(value) => format!("{}/?offset={offset}", uuid::Uuid::parse_str(value)?),
            None => format!("?offset={offset}"),
        };
        if !self.search.is_empty() {
            let encoded: String = self
                .search
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            target.push_str(&format!("&search={encoded}"));
        }
        if self.search.chars().count() > 120 {
            bail!("search is limited to 120 characters");
        }
        if offset > 100_000 {
            bail!("page is outside the supported range");
        }
        if self.target != target {
            // Navigation may use the device’s existing request budget. Never
            // shorten a failure/Retry-After hold or permit a burst of page reads.
            if !self.gate.halted && self.gate.failures == 0 {
                if let Some(checked) = self.checked_ms {
                    self.gate.not_before_ms =
                        self.gate.not_before_ms.min(checked.saturating_add(12_000));
                }
            }
            self.target = target;
            self.etag = None;
            self.page = None;
            self.checked_ms = None;
            if !self.gate.halted {
                self.message = "Waiting for an authorised response from Dataplicity.".into();
            }
        }
        if !self.gate.ready(wall_ms()) {
            return Ok(());
        }
        // This reader has no persisted upload queue. Keep initial retries at
        // the normal read interval; atomic server admission handles restarts.
        self.gate.reserve(wall_ms());
        match self.fetch() {
            Ok(()) => Ok(()),
            Err(_) => {
                self.transport_failure();
                Ok(())
            }
        }
    }
    fn transport_failure(&mut self) {
        self.gate.failure(wall_ms(), None);
        self.page_size = (self.page_size / 2).max(1);
        self.etag = None;
        self.message = format!(
            "Connection interrupted; last complete response is stale. Retrying after backoff with at most {} summaries.",
            self.page_size
        );
    }
    fn fetch(&mut self) -> Result<()> {
        let url = format!(
            "{}/device-gateway/lens/{}/groups/{}&limit={}",
            self.base, self.hash, self.target, self.page_size
        );
        let mut request = self
            .agent
            .get(&url)
            .header("Authorization", &format!("ApiKey {}", self.secret))
            .header("Accept", "application/json");
        if let Some(etag) = &self.etag {
            request = request.header("If-None-Match", etag);
        }
        let mut response = request.call()?;
        let status = response.status().as_u16();
        if status == 304 && self.page.is_some() {
            self.accepted();
            return Ok(());
        }
        if status != 200 {
            let retry = response
                .headers()
                .get("Retry-After")
                .and_then(|h| h.to_str().ok());
            let date = response.headers().get("Date").and_then(|h| h.to_str().ok());
            self.rejected(status, retry, date);
            return Ok(());
        }
        let etag = response
            .headers()
            .get("ETag")
            .and_then(|h| h.to_str().ok())
            .filter(|h| h.len() <= 128)
            .map(str::to_owned);
        let page = decode_page(response.body_mut().as_reader())?;
        // Swap only after the entire bounded response has decoded successfully.
        self.page = Some(page);
        self.etag = etag;
        self.accepted();
        Ok(())
    }
    fn rejected(&mut self, status: u16, retry: Option<&str>, date: Option<&str>) {
        self.gate.status(status, retry, date, wall_ms());
        if self.gate.halted {
            self.page = None;
            self.etag = None;
            self.checked_ms = None;
        }
        self.message = if self.gate.halted {
            "Group access unavailable or revoked. Cached group data cleared.".into()
        } else {
            format!(
                "Dataplicity returned HTTP {status}; waiting before retry. Previous data is stale."
            )
        };
    }
    fn accepted(&mut self) {
        let now = wall_ms();
        self.gate.success(now);
        if let Some(interval) = self.page.as_ref().and_then(|page| {
            page.results
                .iter()
                .filter_map(|row| row["report_interval_seconds"].as_u64())
                .min()
        }) {
            self.gate.not_before_ms = self
                .gate
                .not_before_ms
                .max(now + interval.clamp(60, 14400) * 1000);
        }
        self.checked_ms = Some(now);
        self.message = "Last response received from Dataplicity. Readings carry their own reporting timestamps.".into();
    }
}
pub fn decode_page(reader: impl Read) -> Result<Page> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("incomplete group response")?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("group response exceeds byte limit");
    }
    let page: Page =
        serde_json::from_slice(&bytes).context("incomplete or invalid group response")?;
    if page.results.len() > 20 || page.next.is_some_and(|n| n > 100_000) {
        bail!("group response exceeds page limit");
    }
    Ok(page)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_and_partial_responses_never_replace_complete_pages() {
        assert!(decode_page(&b"{\"results\":["[..]).is_err());
        assert!(decode_page(vec![b' '; MAX_BYTES as usize + 1].as_slice()).is_err());
        let page =
            decode_page(&b"{\"results\":[],\"total\":0,\"next\":null,\"group\":null}"[..]).unwrap();
        assert_eq!(page.total, 0);
    }
    #[test]
    fn opening_detail_rechecks_current_access() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for status in ["200 OK", "403 Forbidden"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                let mut reader = BufReader::new(&stream);
                while !request.ends_with("\r\n\r\n") {
                    assert!(reader.read_line(&mut request).unwrap() > 0);
                }
                assert!(request.starts_with("GET /device-gateway/lens/test/groups/"));
                assert!(!request.to_lowercase().contains("if-none-match"));
                let body = r#"{"results":[{"hash_id":"peer","name":"queensland-01"}],"total":1,"next":null,"group":{}}"#;
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: fixture\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let mut client = Client::new(base, "test".into(), "fixture".into());
        client.agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .build()
            .into();
        let group = Some("11111111-1111-4111-8111-111111111111");
        client.poll(group, 0).unwrap();
        let row = &client.page.as_ref().unwrap().results[0];
        println!("Authorised list: {}", row["name"]);
        client.require_fresh_page();
        client.poll(group, 0).unwrap();
        assert!(client.page.is_none());
        // Advance the test gate to the next permitted network read.
        client.gate.not_before_ms = 0;
        client.poll(group, 0).unwrap();
        assert!(client.halted() && client.page.is_none());
        println!("Drill-down after revocation: {}", client.message);
        server.join().unwrap();
    }
    #[test]
    fn throttle_retains_stale_view_and_revocation_clears_it() {
        let mut client = Client::new(
            "https://example.invalid".into(),
            "a".repeat(64),
            "private".into(),
        );
        client.page = Some(Page {
            results: vec![],
            total: 0,
            next: None,
            group: None,
        });
        let now = wall_ms();
        client.rejected(429, Some("3600"), None);
        assert!(client.page.is_some());
        assert!(client.gate.not_before_ms >= now + 3_600_000);
        assert!(client.message.contains("stale"));
        client.rejected(403, None, None);
        assert!(client.page.is_none());
        assert!(client.halted());
    }
    #[test]
    fn dropped_links_reduce_pages_and_retain_the_last_complete_view() {
        let mut client = Client::new(
            "https://example.invalid".into(),
            "a".repeat(64),
            "private".into(),
        );
        client.page = Some(Page {
            results: vec![],
            total: 0,
            next: None,
            group: None,
        });
        for expected in [10, 5, 2, 1, 1] {
            client.transport_failure();
            assert_eq!(client.page_size, expected);
            assert!(client.page.is_some());
            assert!(!client.gate.ready(wall_ms()));
        }
    }
    #[test]
    fn reset_during_body_is_an_error() {
        struct Reset;
        impl Read for Reset {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "reset",
                ))
            }
        }
        assert!(decode_page(Reset).is_err());
    }
    #[test]
    fn successful_navigation_is_paced_and_search_is_encoded() {
        let mut client = Client::new(
            "https://example.invalid".into(),
            "a".repeat(64),
            "private".into(),
        );
        client.accepted();
        let checked = client.checked_ms.unwrap();
        client.search = "r630 & db/東京".into();
        client
            .poll(Some("11111111-1111-4111-8111-111111111111"), 20)
            .unwrap();
        assert_eq!(client.gate.not_before_ms, checked + 12_000);
        assert!(
            client
                .target
                .contains("search=r630%20%26%20db%2F%E6%9D%B1%E4%BA%AC")
        );
        let deadline = client.gate.not_before_ms;
        client.poll(None, 0).unwrap();
        assert_eq!(client.gate.not_before_ms, deadline);
        assert!(!client.gate.ready(wall_ms()));
    }
    #[test]
    fn changing_pages_clears_cache_without_resetting_backoff() {
        let mut client = Client::new(
            "https://example.invalid".into(),
            "a".repeat(64),
            "private".into(),
        );
        client.gate.failure(wall_ms(), Some(600_000));
        let deadline = client.gate.not_before_ms;
        client.page = Some(Page {
            results: vec![],
            total: 0,
            next: None,
            group: None,
        });
        client.poll(None, 20).unwrap();
        assert!(client.page.is_none());
        assert_eq!(client.gate.not_before_ms, deadline);
        assert!(!format!("{client:?}").contains("private"));
    }
}
