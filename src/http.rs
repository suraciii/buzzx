//! The HTTP bridge: NIP-98 signing, `POST /query`, and `POST /events`. The
//! contract is design/relay-transport.md. A write whose result is unknown is
//! reported as uncertain and never retried by the caller.

use base64::Engine;
use nostr::{Event, EventBuilder, Keys, Kind, Tag};
use serde_json::Value;
use sha2::{Digest, Sha256};

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
    Ok { event_id: String },
    /// The relay refused the event. The reason is the relay's own.
    Failed { reason: String },
    /// The request was sent and the answer never arrived. The event may have
    /// been admitted. The caller reports uncertainty and does not resend.
    Uncertain { reason: String },
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
    /// wrapped in an array, the same REQ shape the bridge accepts.
    pub async fn query(&self, filter: &Value) -> Result<Vec<Event>, String> {
        let url = format!("{}/query", self.base);
        let body = serde_json::to_vec(&serde_json::json!([filter])).map_err(|e| e.to_string())?;
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
            .map_err(|e| format!("query failed: {e}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| format!("query body lost: {e}"))?;
        if !status.is_success() {
            return Err(relay_reason(status, &text));
        }
        let values: Vec<Value> =
            serde_json::from_str(&text).map_err(|e| format!("query response: {e}"))?;
        let mut events = Vec::with_capacity(values.len());
        for value in values {
            match serde_json::from_value::<Event>(value) {
                Ok(event) => events.push(event),
                // One malformed row must not discard the channel's history.
                Err(_) => continue,
            }
        }
        Ok(events)
    }

    /// Submit one signed event. A connection failure is retried once; a
    /// timeout after the request was sent is uncertain, not a failure.
    pub async fn submit(&self, event: &Event) -> WriteOutcome {
        let url = format!("{}/events", self.base);
        let body = match serde_json::to_vec(event) {
            Ok(bytes) => bytes,
            Err(e) => {
                return WriteOutcome::Failed {
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
                Ok(response) => return self.submit_response(response).await,
                Err(e) => {
                    let connect = e.is_connect();
                    if attempt == 0 && connect {
                        continue;
                    }
                    if connect || e.is_timeout() || e.is_request() || e.is_body() {
                        return WriteOutcome::Uncertain {
                            reason: format!("write outcome unknown: {e}"),
                        };
                    }
                    return WriteOutcome::Failed {
                        reason: format!("write failed: {e}"),
                    };
                }
            }
        }
        unreachable!("the loop returns on the second attempt")
    }

    async fn submit_response(&self, response: reqwest::Response) -> WriteOutcome {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            let stored = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("event_id")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            return WriteOutcome::Ok { event_id: stored };
        }
        // A proxy failure after the relay may have stored the event is
        // uncertain. A direct refusal is a failure.
        if matches!(status.as_u16(), 502..=504) {
            return WriteOutcome::Uncertain {
                reason: relay_reason(status, &text),
            };
        }
        WriteOutcome::Failed {
            reason: relay_reason(status, &text),
        }
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
}
