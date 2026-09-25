//! The HTTP bridge: NIP-98 signing, `POST /query`, and `POST /events`. The
//! contract is design/relay-transport.md. A write whose result is unknown is
//! reported as uncertain and never retried by the caller.

use base64::Engine;
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::failure::{Category, Failure};

/// Sign a NIP-98 kind 27235 event and return the Authorization header value.
/// The nonce tag is what makes two identical bodies two different events; the
/// relay keeps a replay guard.
pub fn build_nip98_header(
    keys: &Keys,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> Result<String, String> {
    let mut tags = vec![
        Tag::parse(["u", url]).map_err(|e| e.to_string())?,
        Tag::parse(["method", method]).map_err(|e| e.to_string())?,
        Tag::parse(["nonce", &uuid::Uuid::new_v4().to_string()]).map_err(|e| e.to_string())?,
    ];
    if let Some(bytes) = body {
        let hash = hex::encode(Sha256::digest(bytes));
        tags.push(Tag::parse(["payload", &hash]).map_err(|e| e.to_string())?);
    }
    let event = EventBuilder::new(Kind::Custom(27235), "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|e| format!("NIP-98 signing failed: {e}"))?;
    let json = serde_json::to_string(&event).map_err(|e| e.to_string())?;
    Ok(format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(json)
    ))
}

/// The outcome of a persistent write through `POST /events`.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteOutcome {
    /// The relay accepted the event under this stored id. The relay
    /// canonicalizes events, so this id, not the locally signed one, is the
    /// address later edits, deletions, and replies must target.
    Stored { event_id: String },
    /// The request never left the client, or the relay refused it. Nothing
    /// was stored.
    Refused { category: Category, reason: String },
    /// The request was sent and storage is not established: the answer was
    /// lost, or the relay failed after accepting. The caller reports
    /// uncertainty and does not resend.
    Unknown { category: Category, reason: String },
}

/// A failure raised before anything was sent is a refusal.
impl From<Failure> for WriteOutcome {
    fn from(failure: Failure) -> Self {
        WriteOutcome::Refused {
            category: failure.category,
            reason: failure.detail,
        }
    }
}

fn relay_reason(status: reqwest::StatusCode, body: &str) -> String {
    let extracted = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .or_else(|| v.get("message"))
                .and_then(|m| m.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.to_owned());
    format!("HTTP {status}: {extracted}")
}

/// The category an error status carries on a read. 401 and 403 are the
/// identity's answer; every other refusal is the relay's own, except 404,
/// which means a reference resolved to nothing.
fn status_category(status: reqwest::StatusCode) -> Category {
    match status.as_u16() {
        401 | 403 => Category::Forbidden,
        404 => Category::NotFound,
        _ => Category::RelayRejected,
    }
}

/// The category an error status carries on a write. A write has no
/// "resolved to nothing": 401 and 403 are the identity's answer, and every
/// other refusal is the relay's own, a 404 included.
fn write_category(status: reqwest::StatusCode) -> Category {
    match status.as_u16() {
        401 | 403 => Category::Forbidden,
        _ => Category::RelayRejected,
    }
}

/// Classify an answer to `POST /query`. A read that a relay refuses is an
/// error, never an empty collection.
fn read_response(status: reqwest::StatusCode, body: &str) -> Failure {
    Failure::new(status_category(status), relay_reason(status, body))
}

/// Classify an answer to `POST /events`. A success needs the relay's
/// canonical id: without it nothing can address the stored event, so the
/// result is unknown rather than confirmed. A 4xx refusal means nothing was
/// stored; a 5xx answer leaves storage unestablished.
fn write_response(status: reqwest::StatusCode, body: &str) -> WriteOutcome {
    if status.is_success() {
        let stored = serde_json::from_str::<Value>(body).ok().and_then(|v| {
            v.get("event_id")
                .and_then(|id| id.as_str())
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        });
        return match stored {
            Some(event_id) => WriteOutcome::Stored { event_id },
            None => WriteOutcome::Unknown {
                category: Category::TimeoutUnknown,
                reason: "the relay accepted the event without a canonical id".to_owned(),
            },
        };
    }
    if matches!(status.as_u16(), 502..=504) {
        return WriteOutcome::Unknown {
            category: Category::TimeoutUnknown,
            reason: relay_reason(status, body),
        };
    }
    if status.is_server_error() {
        return WriteOutcome::Unknown {
            category: Category::RelayRejected,
            reason: relay_reason(status, body),
        };
    }
    WriteOutcome::Refused {
        category: write_category(status),
        reason: relay_reason(status, body),
    }
}

/// The NIP-98 HTTP bridge client. One instance per session.
pub struct HttpTransport {
    http: reqwest::Client,
    base: String,
    keys: Keys,
    auth_tag: Option<String>,
}

impl HttpTransport {
    pub fn new(base: &str, keys: Keys, auth_tag: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.to_owned(),
            keys,
            auth_tag,
        }
    }

    fn auth_header(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
    ) -> reqwest::header::HeaderValue {
        let header = build_nip98_header(&self.keys, method, url, body)
            .expect("NIP-98 signing only fails on an unusable key");
        reqwest::header::HeaderValue::from_str(&header).expect("base64 header is ASCII")
    }

    /// One filter per call, per the bridge contract. The body is the filter
    /// wrapped in an array, the same REQ shape the bridge accepts. A read
    /// changes nothing, so a lost answer is repeated once.
    pub async fn query(&self, filter: &Value) -> Result<Vec<Event>, Failure> {
        self.query_all(std::slice::from_ref(filter)).await
    }

    /// Several filters in one REQ. The bridge answers with their union, and the
    /// relay accepts a bounded number of filters per REQ, so a caller that
    /// needs one filter per conversation batches them. A read changes nothing,
    /// so a lost answer is repeated once.
    pub async fn query_all(&self, filters: &[Value]) -> Result<Vec<Event>, Failure> {
        match self.query_once(filters).await {
            Err(failure) if failure.category == Category::Network => self.query_once(filters).await,
            result => result,
        }
    }

    async fn query_once(&self, filters: &[Value]) -> Result<Vec<Event>, Failure> {
        let url = format!("{}/query", self.base);
        let body = serde_json::to_vec(&serde_json::json!(filters))
            .map_err(|e| Failure::invalid_input(format!("query body: {e}")))?;
        let header = self.auth_header("POST", &url, Some(&body));
        let mut request = self
            .http
            .post(&url)
            .header("Authorization", header)
            .body(body);
        if let Some(tag) = &self.auth_tag {
            request = request.header("x-auth-tag", tag);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Failure::network(format!("query failed: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| Failure::network(format!("query body lost: {e}")))?;
        if !status.is_success() {
            return Err(read_response(status, &text));
        }
        let values: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| Failure::new(Category::RelayRejected, format!("query response: {e}")))?;
        let mut events = Vec::with_capacity(values.len());
        let mut unreadable = 0usize;
        for value in values {
            match serde_json::from_value::<Event>(value) {
                Ok(event) => events.push(event),
                // One malformed row must not discard the channel's history.
                Err(_) => unreadable += 1,
            }
        }
        // An answer nothing parsed is not an empty read: a caller must not
        // read a relay that answered garbage as "no such event".
        if events.is_empty() && unreadable > 0 {
            return Err(Failure::new(
                Category::RelayRejected,
                format!("the relay answered with {unreadable} unreadable event(s)"),
            ));
        }
        Ok(events)
    }

    /// Submit one signed event. A connection failure is retried once, because
    /// a request that never left the client cannot have been stored. Every
    /// other transport failure after the request was sent is uncertain, not a
    /// refusal.
    pub async fn submit(&self, event: &Event) -> WriteOutcome {
        let url = format!("{}/events", self.base);
        let body = match serde_json::to_vec(event) {
            Ok(bytes) => bytes,
            Err(e) => {
                return WriteOutcome::Refused {
                    category: Category::InvalidInput,
                    reason: format!("serialize: {e}"),
                };
            }
        };
        for attempt in 0..2 {
            let header = self.auth_header("POST", &url, Some(&body));
            let mut request = self
                .http
                .post(&url)
                .header("Authorization", header)
                .header("Content-Type", "application/json")
                .body(body.clone());
            if let Some(tag) = &self.auth_tag {
                request = request.header("x-auth-tag", tag);
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text().await.unwrap_or_default();
                    return write_response(status, &text);
                }
                Err(e) => {
                    if e.is_connect() {
                        if attempt == 0 {
                            continue;
                        }
                        return WriteOutcome::Refused {
                            category: Category::Network,
                            reason: format!("write failed, nothing was sent: {e}"),
                        };
                    }
                    return WriteOutcome::Unknown {
                        category: Category::TimeoutUnknown,
                        reason: format!("write outcome unknown: {e}"),
                    };
                }
            }
        }
        unreachable!("the loop returns on the second attempt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nip98_header_carries_url_method_nonce_and_payload_hash() {
        let keys = Keys::generate();
        let header = build_nip98_header(&keys, "POST", "https://r/events", Some(b"hello")).unwrap();
        assert!(header.starts_with("Nostr "), "scheme is Nostr: {header}");
        let encoded = header.strip_prefix("Nostr ").unwrap();
        let json = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let event: Event = serde_json::from_slice(&json).unwrap();
        assert_eq!(event.kind.as_u16(), 27235);
        assert_eq!(event.content, "");
        let find = |name: &str| {
            event.tags.iter().find_map(|t| {
                let parts = t.as_slice();
                (parts.first().map(String::as_str) == Some(name))
                    .then(|| parts.get(1).unwrap().clone())
            })
        };
        assert_eq!(find("u").as_deref(), Some("https://r/events"));
        assert_eq!(find("method").as_deref(), Some("POST"));
        assert!(find("nonce").is_some());
        let expected = hex::encode(Sha256::digest(b"hello"));
        assert_eq!(find("payload").as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn nip98_payload_tag_is_absent_without_a_body() {
        let keys = Keys::generate();
        let header = build_nip98_header(&keys, "POST", "https://r/query", None).unwrap();
        let json = base64::engine::general_purpose::STANDARD
            .decode(header.strip_prefix("Nostr ").unwrap())
            .unwrap();
        let event: Event = serde_json::from_slice(&json).unwrap();
        assert!(
            !event
                .tags
                .iter()
                .any(|t| t.as_slice().first().map(String::as_str) == Some("payload"))
        );
    }

    #[test]
    fn two_nip98_signatures_differ_by_nonce() {
        let keys = Keys::generate();
        let a = build_nip98_header(&keys, "POST", "https://r/events", Some(b"x")).unwrap();
        let b = build_nip98_header(&keys, "POST", "https://r/events", Some(b"x")).unwrap();
        assert_ne!(a, b, "identical bodies must produce distinct events");
    }

    #[test]
    fn a_stored_id_needs_the_relays_own_id() {
        match write_response(reqwest::StatusCode::OK, r#"{"event_id":"ab"}"#) {
            WriteOutcome::Stored { event_id } => assert_eq!(event_id, "ab"),
            other => panic!("expected Stored, got {other:?}"),
        }
        // A success without the relay's id cannot address the stored event.
        match write_response(reqwest::StatusCode::OK, "{}") {
            WriteOutcome::Unknown { category, .. } => {
                assert_eq!(category, Category::TimeoutUnknown)
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        match write_response(reqwest::StatusCode::OK, r#"{"event_id":""}"#) {
            WriteOutcome::Unknown { .. } => {}
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_write_is_not_sent_and_a_server_failure_is_unknown() {
        match write_response(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":"not a member"}"#,
        ) {
            WriteOutcome::Refused { category, reason } => {
                assert_eq!(category, Category::Forbidden);
                assert!(reason.contains("not a member"), "{reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        match write_response(reqwest::StatusCode::BAD_REQUEST, r#"{"error":"bad tag"}"#) {
            WriteOutcome::Refused { category, .. } => assert_eq!(category, Category::RelayRejected),
            other => panic!("expected Refused, got {other:?}"),
        }
        // A write has no "resolved to nothing": a 404 is the relay refusing
        // the request, not a missing reference.
        match write_response(reqwest::StatusCode::NOT_FOUND, r#"{"error":"no path"}"#) {
            WriteOutcome::Refused { category, reason } => {
                assert_eq!(category, Category::RelayRejected);
                assert!(reason.contains("no path"), "{reason}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        // A proxy failure may have reached the relay, so the event may exist.
        match write_response(reqwest::StatusCode::BAD_GATEWAY, "upstream gone") {
            WriteOutcome::Unknown { category, .. } => {
                assert_eq!(category, Category::TimeoutUnknown)
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        // A 500 is the relay's own failure: it answered, and storage is not
        // established either way.
        match write_response(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom") {
            WriteOutcome::Unknown { category, .. } => {
                assert_eq!(category, Category::RelayRejected)
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_refused_read_carries_its_own_category() {
        let forbidden = read_response(reqwest::StatusCode::FORBIDDEN, "");
        assert_eq!(forbidden.category, Category::Forbidden);
        let missing = read_response(reqwest::StatusCode::NOT_FOUND, "");
        assert_eq!(missing.category, Category::NotFound);
        let rejected = read_response(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow down");
        assert_eq!(rejected.category, Category::RelayRejected);
        assert!(rejected.detail.contains("slow down"), "{rejected}");
    }
}
