//! The WebSocket pump: one task that owns the NIP-42 connection, the REQ
//! lifecycle, frame decoding, and reconnects. The contract is
//! design/relay-transport.md. Nothing here decides what a row means; it
//! forwards typed `ChatEvent`s.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use buzz_ws_client::{NostrWsConnection, RelayMessage};
use nostr::{Keys, Tag};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::session::ChatEvent;

/// How far back the live subscription reaches. The overlap with the HTTP
/// history query is closed by event-id dedupe upstream; the window exists so
/// a message stored between the history read and the REQ is not lost.
const LIVE_OVERLAP_SECS: u64 = 30;
/// Longest wait on the socket before the control channel is checked.
const SOCKET_POLL: Duration = Duration::from_millis(200);
/// Floor of the reconnect interval; the relay must not see a tight loop.
const RECONNECT_FLOOR_SECS: u64 = 25;
/// Ceiling of the reconnect interval.
const RECONNECT_CAP_SECS: u64 = 300;
/// Auxiliary ids per REQ. A wave of reactions must not dilute anything.
const AUX_CHUNK: usize = 100;

/// What the UI asks the pump to subscribe to.
#[derive(Debug)]
pub enum SubControl {
    /// The live timeline feed for one channel.
    Timeline(Uuid),
    /// The overlay feed for one channel's loaded ids, replacing the previous
    /// feed for that channel.
    Aux { channel: Uuid, ids: Vec<String> },
    /// The typing feed for every channel the identity belongs to, replacing
    /// the previous feed. The list is per-connection state: an empty list
    /// closes the feed.
    Typing(Vec<Uuid>),
}

#[derive(Debug, Clone, Copy)]
enum SubKind {
    Timeline(Uuid),
    Aux(#[allow(dead_code)] Uuid),
    Typing(Uuid),
}

/// What a `CLOSED` from the relay ended, for a subscription this client
/// opened. Everything about a subscription id is per connection, so the id is
/// removed either way; the caller decides what the user is told.
enum Closed {
    Timeline(Uuid),
    Typing(Uuid),
}

fn close_subscription(state: &mut PumpState, sub_id: &str) -> Option<Closed> {
    match state.sub_ids.remove(sub_id)? {
        SubKind::Timeline(channel) => {
            state.timeline_channels.retain(|c| c != &channel);
            Some(Closed::Timeline(channel))
        }
        SubKind::Typing(channel) => Some(Closed::Typing(channel)),
        SubKind::Aux(_) => None,
    }
}

fn timeline_filter(channel: &Uuid, since: u64) -> serde_json::Value {
    serde_json::json!({
        "kinds": crate::content::TIMELINE_KINDS,
        "#h": [channel.to_string()],
        "limit": 1000,
        "since": since,
    })
}

fn aux_filter(ids: &[String]) -> serde_json::Value {
    serde_json::json!({
        "kinds": crate::content::AUX_KINDS,
        "#e": ids,
        "limit": 1000,
    })
}

/// Typing is ephemeral, so the filter carries no `since` and no `limit`: there
/// is nothing stored to page through, and a time window could only drop a live
/// indicator. Kind and channel are the whole query.
///
/// One `#h` value, never several: the relay registers live fan-out per single
/// channel and falls back to a global subscription for a filter that names more
/// than one, which receives no channel event at all.
fn typing_filter(channel: &Uuid) -> serde_json::Value {
    serde_json::json!({
        "kinds": [crate::content::TYPING_KIND],
        "#h": [channel.to_string()],
    })
}

/// The channel a typing indicator names. An indicator without a usable `h`
/// tag cannot be placed on screen, so the pump drops it.
fn typing_channel(event: &nostr::Event) -> Option<Uuid> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some("h"))
            .then(|| parts.get(1))
            .flatten()
            .and_then(|id| Uuid::parse_str(id).ok())
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run the pump until the process ends. `started` carries the first
/// connection result so the entry point can exit with the right code before
/// the interface is up.
#[allow(clippy::too_many_arguments)]
pub async fn run_ws_pump(
    ws_url: String,
    keys: Keys,
    auth_tag: Option<Tag>,
    mut control: mpsc::Receiver<SubControl>,
    events: mpsc::Sender<ChatEvent>,
    started: oneshot::Sender<Result<(), (i32, String)>>,
) {
    let mut started = Some(started);
    let mut state = PumpState::default();
    let mut first = true;
    let mut attempt: u32 = 0;
    // Controls that arrive while the pump is backing off. They are applied
    // right after the next connection; dropping one would silently lose a
    // subscription for the rest of the session.
    let mut queued: Vec<SubControl> = Vec::new();
    loop {
        let connected =
            NostrWsConnection::connect_authenticated(&ws_url, &keys, auth_tag.as_ref()).await;
        match connected {
            Ok(mut conn) => {
                if first {
                    first = false;
                    if let Some(sender) = started.take() {
                        let _ = sender.send(Ok(()));
                    }
                }
                attempt = 0;
                let _ = events.send(ChatEvent::Connected).await;
                state.resubscribe_all(&mut conn).await;
                for control in queued.drain(..) {
                    apply_control(&mut conn, &mut state, control).await;
                }
                match pump_connection(
                    &mut conn,
                    &mut state,
                    &mut control,
                    &events,
                    &keys,
                    &auth_tag,
                )
                .await
                {
                    Ok(()) => return,
                    Err(reason) => {
                        let _ = events.send(ChatEvent::Disconnected(reason)).await;
                    }
                }
            }
            Err(e) => {
                if first {
                    // Startup failure: unreachable is 2, an auth refusal is 3.
                    let code = if matches!(e, buzz_ws_client::WsClientError::AuthFailed(_)) {
                        crate::config::EXIT_AUTH
                    } else {
                        crate::config::EXIT_NETWORK
                    };
                    if let Some(sender) = started.take() {
                        let _ = sender.send(Err((code, e.to_string())));
                    }
                    return;
                }
                let _ = events
                    .send(ChatEvent::Disconnected(format!("reconnect failed: {e}")))
                    .await;
            }
        }
        // A control arriving mid-backoff must not shorten the backoff: the
        // relay reconnect floor still applies. It is queued instead.
        let deadline = tokio::time::Instant::now() + reconnect_delay(attempt);
        attempt += 1;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                control = control.recv() => match control {
                    Some(control) => queued.push(control),
                    None => return,
                },
            }
        }
    }
}

fn reconnect_delay(attempt: u32) -> Duration {
    let ceiling = RECONNECT_FLOOR_SECS
        .saturating_mul(1u64 << attempt.min(4))
        .min(RECONNECT_CAP_SECS);
    let span = ceiling - RECONNECT_FLOOR_SECS;
    let jitter = rand::random::<f64>() * span as f64;
    Duration::from_secs_f64(RECONNECT_FLOOR_SECS as f64 + jitter)
}

#[derive(Default)]
struct PumpState {
    sub_ids: HashMap<String, SubKind>,
    timeline_channels: Vec<Uuid>,
    aux_ids: HashMap<Uuid, Vec<String>>,
    typing_channels: Vec<Uuid>,
}

impl PumpState {
    async fn req_raw(conn: &mut NostrWsConnection, sub_id: &str, filter: &serde_json::Value) {
        let frame = serde_json::json!(["REQ", sub_id, filter]);
        if let Err(e) = conn.send_raw(&frame).await {
            // A failed REQ leaves the socket unusable; the read loop
            // observes that and reconnects.
            eprintln!("buzzx: REQ {sub_id} failed: {e}");
        }
    }

    async fn close(conn: &mut NostrWsConnection, sub_id: &str) {
        let frame = serde_json::json!(["CLOSE", sub_id]);
        let _ = conn.send_raw(&frame).await;
    }

    async fn subscribe_timeline(&mut self, conn: &mut NostrWsConnection, channel: Uuid) {
        let sub_id = format!("t:{channel}");
        if self.sub_ids.contains_key(&sub_id) {
            return;
        }
        Self::req_raw(
            conn,
            &sub_id,
            &timeline_filter(&channel, now_secs().saturating_sub(LIVE_OVERLAP_SECS)),
        )
        .await;
        self.sub_ids.insert(sub_id, SubKind::Timeline(channel));
        if !self.timeline_channels.contains(&channel) {
            self.timeline_channels.push(channel);
        }
    }

    async fn subscribe_aux(
        &mut self,
        conn: &mut NostrWsConnection,
        channel: Uuid,
        ids: Vec<String>,
    ) {
        for sub in self
            .sub_ids
            .keys()
            .filter(|k| k.starts_with(&format!("a:{channel}")))
        {
            Self::close(conn, sub).await;
        }
        self.sub_ids
            .retain(|k, _| !k.starts_with(&format!("a:{channel}")));
        self.aux_ids.insert(channel, ids.clone());
        for (index, chunk) in ids.chunks(AUX_CHUNK).enumerate() {
            let sub_id = format!("a:{channel}:{index}");
            Self::req_raw(conn, &sub_id, &aux_filter(chunk)).await;
            self.sub_ids.insert(sub_id, SubKind::Aux(channel));
        }
    }

    /// Replace the typing feed with one covering exactly these channels. Every
    /// subscription is replaced rather than added to: the channel list is the
    /// authority on membership, and a stale channel would keep its indicators
    /// alive after the identity left it. One REQ per channel, because the relay
    /// indexes live fan-out by a single `#h` value.
    async fn subscribe_typing(&mut self, conn: &mut NostrWsConnection, channels: Vec<Uuid>) {
        for sub in self
            .sub_ids
            .keys()
            .filter(|k| k.starts_with("y:"))
            .cloned()
            .collect::<Vec<String>>()
        {
            Self::close(conn, &sub).await;
        }
        self.sub_ids.retain(|k, _| !k.starts_with("y:"));
        self.typing_channels = channels.clone();
        for channel in channels {
            let sub_id = format!("y:{channel}");
            Self::req_raw(conn, &sub_id, &typing_filter(&channel)).await;
            self.sub_ids.insert(sub_id, SubKind::Typing(channel));
        }
    }

    async fn resubscribe_all(&mut self, conn: &mut NostrWsConnection) {
        // A subscription id belongs to the socket that opened it. The socket
        // that just died took all of them with it, so this connection starts
        // from nothing: the REQs below are first REQs, not duplicates, and
        // `subscribe_timeline`'s own guard no longer sees an id that no live
        // subscription has.
        self.sub_ids.clear();
        let channels = self.timeline_channels.clone();
        for channel in channels {
            self.subscribe_timeline(conn, channel).await;
        }
        let aux = self.aux_ids.clone();
        for (channel, ids) in aux {
            self.subscribe_aux(conn, channel, ids).await;
        }
        if !self.typing_channels.is_empty() {
            let typing = self.typing_channels.clone();
            self.subscribe_typing(conn, typing).await;
        }
    }
}

/// Serve one live connection. Returns Ok when the session is shutting down
/// and Err with the reason the connection ended.
async fn pump_connection(
    conn: &mut NostrWsConnection,
    state: &mut PumpState,
    control: &mut mpsc::Receiver<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
    keys: &Keys,
    auth_tag: &Option<Tag>,
) -> Result<(), String> {
    loop {
        drain_control(conn, state, control).await?;
        match conn.next_event(SOCKET_POLL).await {
            Ok(message) => handle_message(conn, state, events, message, keys, auth_tag).await?,
            Err(buzz_ws_client::WsClientError::Timeout) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

async fn apply_control(conn: &mut NostrWsConnection, state: &mut PumpState, control: SubControl) {
    match control {
        SubControl::Timeline(channel) => state.subscribe_timeline(conn, channel).await,
        SubControl::Aux { channel, ids } => state.subscribe_aux(conn, channel, ids).await,
        SubControl::Typing(channels) => state.subscribe_typing(conn, channels).await,
    }
}

/// Apply every queued control message. A closed channel means shutdown.
async fn drain_control(
    conn: &mut NostrWsConnection,
    state: &mut PumpState,
    control: &mut mpsc::Receiver<SubControl>,
) -> Result<(), String> {
    loop {
        match control.try_recv() {
            Ok(control) => apply_control(conn, state, control).await,
            Err(mpsc::error::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::error::TryRecvError::Disconnected) => return Err("shutting down".to_owned()),
        }
    }
}

async fn handle_message(
    conn: &mut NostrWsConnection,
    state: &mut PumpState,
    events: &mpsc::Sender<ChatEvent>,
    message: RelayMessage,
    keys: &Keys,
    auth_tag: &Option<Tag>,
) -> Result<(), String> {
    match message {
        RelayMessage::Event {
            subscription_id,
            event,
        } => match state.sub_ids.get(&subscription_id) {
            Some(SubKind::Timeline(channel)) => {
                let _ = events
                    .send(ChatEvent::Timeline {
                        channel: *channel,
                        event: *event,
                    })
                    .await;
            }
            Some(SubKind::Aux(_)) => {
                let _ = events.send(ChatEvent::Overlay(*event)).await;
            }
            Some(SubKind::Typing(_)) => {
                if let Some(channel) = typing_channel(&event) {
                    let _ = events
                        .send(ChatEvent::Typing {
                            channel,
                            pubkey: event.pubkey.to_hex(),
                            at: event.created_at.as_secs(),
                        })
                        .await;
                }
            }
            None => {}
        },
        RelayMessage::Eose { .. } => {}
        RelayMessage::Closed {
            subscription_id,
            message,
        } => match close_subscription(state, &subscription_id) {
            Some(Closed::Timeline(channel)) => {
                let _ = events
                    .send(ChatEvent::ChannelGone {
                        channel,
                        reason: message,
                    })
                    .await;
            }
            Some(Closed::Typing(channel)) => {
                let _ = events
                    .send(ChatEvent::TypingClosed {
                        channel,
                        reason: message,
                    })
                    .await;
            }
            None => {}
        },
        RelayMessage::Notice { message } => {
            let _ = events.send(ChatEvent::Status(message)).await;
        }
        RelayMessage::Ok(ok) => {
            if !ok.accepted {
                let _ = events
                    .send(ChatEvent::Status(format!(
                        "publish refused: {}",
                        ok.message
                    )))
                    .await;
            }
        }
        RelayMessage::Auth { .. } => {
            // A late re-challenge: answer it or every following REQ dies.
            if let Err(e) = conn.authenticate(keys, auth_tag.as_ref()).await {
                return Err(format!("re-authentication failed: {e}"));
            }
        }
        RelayMessage::Count { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indicator(tags: Vec<Vec<&str>>) -> nostr::Event {
        let keys = Keys::generate();
        let tags: Vec<Tag> = tags
            .into_iter()
            .map(|parts| Tag::parse(parts).expect("a tag"))
            .collect();
        nostr::EventBuilder::new(nostr::Kind::Custom(crate::content::TYPING_KIND as u16), "")
            .tags(tags)
            .sign_with_keys(&keys)
            .expect("a signed event")
    }

    #[test]
    fn an_indicator_is_routed_by_its_h_tag_or_not_at_all() {
        let channel = Uuid::new_v4();
        let event = indicator(vec![vec!["h", &channel.to_string()]]);
        assert_eq!(typing_channel(&event), Some(channel));

        // No `h`, an `h` with no value, and an `h` that is not a channel are
        // all indicators without a channel: there is nowhere to show them.
        for tags in [vec![], vec![vec!["h"]], vec![vec!["h", "not-a-uuid"]]] {
            assert_eq!(typing_channel(&indicator(tags)), None);
        }
    }

    #[test]
    fn a_closed_typing_feed_is_told_apart_from_a_closed_timeline() {
        let (one, two) = (Uuid::new_v4(), Uuid::new_v4());
        let mut state = PumpState::default();
        state
            .sub_ids
            .insert(format!("y:{one}"), SubKind::Typing(one));
        state
            .sub_ids
            .insert(format!("t:{two}"), SubKind::Timeline(two));
        state.timeline_channels.push(two);
        state
            .sub_ids
            .insert("a:03de0a1b".to_owned(), SubKind::Aux(Uuid::new_v4()));

        assert!(
            matches!(close_subscription(&mut state, &format!("y:{one}")), Some(Closed::Typing(channel)) if channel == one),
            "a refused feed is its own event, not a channel that went away"
        );
        assert!(matches!(
            close_subscription(&mut state, &format!("t:{two}")),
            Some(Closed::Timeline(channel)) if channel == two
        ));
        assert_eq!(state.timeline_channels, Vec::<Uuid>::new());
        assert!(close_subscription(&mut state, "a:03de0a1b").is_none());
        assert!(close_subscription(&mut state, "y:never-opened").is_none());
        assert!(
            state.sub_ids.is_empty(),
            "a closed subscription is forgotten"
        );
    }

    #[test]
    fn the_typing_filter_names_one_channel_and_no_time_window() {
        let channel = Uuid::new_v4();
        let filter = typing_filter(&channel);
        assert_eq!(filter["kinds"], serde_json::json!([20002]));
        assert_eq!(filter["#h"], serde_json::json!([channel.to_string()]));
        // Ephemeral: nothing is stored, so a window could only hide a live one.
        assert!(filter.get("since").is_none());
        assert!(filter.get("limit").is_none());
    }
}
