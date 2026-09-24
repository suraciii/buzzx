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
}

#[derive(Debug, Clone, Copy)]
enum SubKind {
    Timeline(Uuid),
    Aux(#[allow(dead_code)] Uuid),
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
        let delay = reconnect_delay(attempt);
        attempt += 1;
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            control = control.recv() => {
                if control.is_none() {
                    return;
                }
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

    async fn resubscribe_all(&mut self, conn: &mut NostrWsConnection) {
        let channels = self.timeline_channels.clone();
        for channel in channels {
            self.subscribe_timeline(conn, channel).await;
        }
        let aux = self.aux_ids.clone();
        for (channel, ids) in aux {
            self.subscribe_aux(conn, channel, ids).await;
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

/// Apply every queued control message. A None channel means shutdown.
async fn drain_control(
    conn: &mut NostrWsConnection,
    state: &mut PumpState,
    control: &mut mpsc::Receiver<SubControl>,
) -> Result<(), String> {
    loop {
        match control.try_recv() {
            Ok(SubControl::Timeline(channel)) => state.subscribe_timeline(conn, channel).await,
            Ok(SubControl::Aux { channel, ids }) => state.subscribe_aux(conn, channel, ids).await,
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
            None => {}
        },
        RelayMessage::Eose { .. } => {}
        RelayMessage::Closed {
            subscription_id,
            message,
        } => {
            if let Some(SubKind::Timeline(channel)) = state.sub_ids.remove(&subscription_id) {
                state.timeline_channels.retain(|c| c != &channel);
                let _ = events
                    .send(ChatEvent::ChannelGone {
                        channel,
                        reason: message,
                    })
                    .await;
            }
        }
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
