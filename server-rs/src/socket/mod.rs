pub mod gameplay;
pub mod state;

use crate::db::{self, DbPools};
use crate::routes::game::{self, GameSettings};
use crate::routes::stats;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use socketioxide::SocketIo;
use socketioxide::extract::{Data, SocketRef, State};
use state::{
    AvatarId, CharacterPayload, NonstopWinner, NonstopWinnerBonuses, Player, Room, RoomCommand,
    ServerState, TagBanEntry, WinnerMarker,
};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tracing::{info, warn};

const OUTBOX_CAPACITY: usize = 256;
const HIGH_OUTBOX_OVERFLOW_CAPACITY: usize = 256;
const LOBBY_OUTBOX_CAPACITY: usize = 64;
pub const MAX_ROOM_PLAYERS: usize = 8;
const ANSWER_SETTER_TIMEOUT_SECS: u64 = 120;

static TARGET_OUTBOXES: LazyLock<dashmap::DashMap<String, TargetOutbox>> =
    LazyLock::new(dashmap::DashMap::new);
static COALESCED_OUTBOX_EVENTS: LazyLock<dashmap::DashMap<String, Value>> =
    LazyLock::new(dashmap::DashMap::new);
static HIGH_OUTBOX_OVERFLOW: LazyLock<
    dashmap::DashMap<String, Arc<StdMutex<VecDeque<OutboxMessage>>>>,
> = LazyLock::new(dashmap::DashMap::new);
static LOBBY_OUTBOX: LazyLock<StdMutex<Option<mpsc::Sender<Value>>>> =
    LazyLock::new(|| StdMutex::new(None));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmitPriority {
    High,
    Normal,
    Low,
}

#[derive(Clone, Debug)]
struct OutboxMessage {
    event: &'static str,
    payload: Value,
    priority: EmitPriority,
    coalescing: bool,
}

#[derive(Clone)]
struct TargetOutbox {
    high: mpsc::Sender<OutboxMessage>,
    normal: mpsc::Sender<OutboxMessage>,
}

fn emit_error(socket: &SocketRef, event: &str, message: &str) {
    let _ = socket.emit(
        "error",
        &json!({
            "event": event,
            "message": format!("{}: {}", event, message),
        }),
    );
}

fn character_name_for_stats(character: &CharacterPayload) -> String {
    ["nameCn", "name_cn", "name"]
        .iter()
        .find_map(|key| {
            character
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn active_room_player_count(room: &Room) -> usize {
    room.players.iter().filter(|p| !p.disconnected).count()
}

fn can_player_change_team(room: &Room, player: &Player) -> Result<(), &'static str> {
    if room.current_game.is_some() {
        return Err("游戏进行中不能切换队伍或旁观状态");
    }
    if room.waiting_for_answer {
        return Err("等待出题时不能切换队伍或旁观状态");
    }
    if player.ready {
        return Err("已准备后不能切换队伍或旁观状态，请先取消准备");
    }
    if player.disconnected {
        return Err("断线玩家不能切换队伍或旁观状态");
    }
    Ok(())
}

fn validate_answer_setter(
    room: &Room,
    actor_id: &str,
    setter_id: &str,
) -> Result<String, &'static str> {
    if !room
        .players
        .iter()
        .any(|p| p.id == actor_id && p.is_host && !p.disconnected)
    {
        return Err("只有房主可以选择出题人");
    }
    if room.current_game.is_some() {
        return Err("游戏进行中不能更换出题人");
    }
    if room.waiting_for_answer {
        return Err("正在等待出题，不能重复指定出题人");
    }

    let Some(setter) = room.players.iter().find(|p| p.id == setter_id) else {
        return Err("找不到选中的玩家");
    };
    if setter.disconnected {
        return Err("不能选择断线玩家出题");
    }
    if setter.team.as_deref() == Some("0") || setter.temp_observer {
        return Err("旁观者不能作为出题人");
    }
    if !setter.is_host && !setter.ready {
        return Err("只能选择已准备玩家出题");
    }
    Ok(setter.username.clone())
}

fn build_wait_for_answer_canceled_payload(message: impl Into<String>) -> Value {
    json!({
        "message": message.into(),
    })
}

fn cancel_waiting_for_answer_with_message(
    room: &mut Room,
    message: impl Into<String>,
) -> Option<(Value, Value)> {
    gameplay::cancel_waiting_for_answer(room).map(|_| {
        let players_payload = prepare_players_broadcast(
            room,
            Some(json!({
                "answerSetterId": Value::Null,
                "forceImmediate": true,
            })),
        );
        (
            build_wait_for_answer_canceled_payload(message),
            players_payload,
        )
    })
}

fn schedule_answer_setter_timeout(
    state: Arc<ServerState>,
    io: SocketIo,
    room_id: String,
    setter_id: String,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(ANSWER_SETTER_TIMEOUT_SECS)).await;

        let result = state
            .run_room_command(
                &room_id,
                RoomCommand::SocketEvent("answerSetterTimeout"),
                |room| {
                    if !room.waiting_for_answer
                        || room.answer_setter_id.as_deref() != Some(setter_id.as_str())
                    {
                        return None;
                    }

                    let setter_username = room
                        .players
                        .iter()
                        .find(|p| p.id == setter_id)
                        .map(|p| p.username.clone())
                        .unwrap_or_else(|| "出题人".to_string());
                    cancel_waiting_for_answer_with_message(
                        room,
                        format!("指定的出题人 {} 超时未提交，等待已取消", setter_username),
                    )
                },
            )
            .await;

        if let Some(Some((cancel_payload, players_payload))) = result {
            emit_to_room(&io, room_id.clone(), "updatePlayers", players_payload);
            emit_to_room(
                &io,
                room_id.clone(),
                "waitForAnswerCanceled",
                cancel_payload,
            );
            broadcast_lobby_rooms_updated(&io);
        }
    });
}

fn merge_extra(a: Option<Value>, b: Option<Value>) -> Option<Value> {
    match (a, b) {
        (None, None) => None,
        (Some(v), None) | (None, Some(v)) => Some(v),
        (Some(Value::Object(mut a)), Some(Value::Object(b))) => {
            for (k, v) in b {
                a.insert(k, v);
            }
            Some(Value::Object(a))
        }
        // If types mismatch, last wins.
        (_, Some(b)) => Some(b),
    }
}

fn is_coalescing_event(event: &str) -> bool {
    matches!(
        event,
        "updatePlayers" | "guessHistoryUpdate" | "syncWaiting" | "nonstopProgress"
    )
}

fn emit_priority(event: &str) -> EmitPriority {
    if is_coalescing_event(event) {
        return EmitPriority::Low;
    }

    match event {
        "gameStart"
        | "gameEnded"
        | "answerReveal"
        | "roundResult"
        | "playerKicked"
        | "roomClosed"
        | "hostTransferred"
        | "waitForAnswerCanceled" => EmitPriority::High,
        _ => EmitPriority::Normal,
    }
}

fn coalesced_key(target: &str, event: &str) -> String {
    format!("{}\u{0}{}", target, event)
}

fn take_coalesced_for_target(target: &str) -> Vec<OutboxMessage> {
    let prefix = format!("{}\u{0}", target);
    let keys: Vec<String> = COALESCED_OUTBOX_EVENTS
        .iter()
        .filter_map(|entry| {
            if entry.key().starts_with(&prefix) {
                Some(entry.key().clone())
            } else {
                None
            }
        })
        .collect();

    let mut events = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some((key, payload)) = COALESCED_OUTBOX_EVENTS.remove(&key) {
            let event = key.strip_prefix(&prefix).and_then(|name| match name {
                "updatePlayers" => Some("updatePlayers"),
                "guessHistoryUpdate" => Some("guessHistoryUpdate"),
                "syncWaiting" => Some("syncWaiting"),
                "nonstopProgress" => Some("nonstopProgress"),
                _ => None,
            });
            if let Some(event) = event {
                events.push(OutboxMessage {
                    event,
                    payload,
                    priority: EmitPriority::Low,
                    coalescing: true,
                });
            }
        }
    }
    events
}

fn push_high_overflow(target: &str, message: OutboxMessage) -> bool {
    let queue = HIGH_OUTBOX_OVERFLOW
        .entry(target.to_string())
        .or_insert_with(|| Arc::new(StdMutex::new(VecDeque::new())))
        .clone();
    let Ok(mut queue) = queue.lock() else {
        return false;
    };
    if queue.len() >= HIGH_OUTBOX_OVERFLOW_CAPACITY {
        return false;
    }
    queue.push_back(message);
    true
}

fn take_high_overflow(target: &str) -> Option<OutboxMessage> {
    let queue = HIGH_OUTBOX_OVERFLOW.get(target)?.clone();
    let Ok(mut queue) = queue.lock() else {
        return None;
    };
    let message = queue.pop_front();
    let should_remove = queue.is_empty();
    drop(queue);
    if should_remove {
        HIGH_OUTBOX_OVERFLOW.remove(target);
    }
    message
}

async fn deliver_outbox_message(io: &SocketIo, target: &str, message: OutboxMessage) {
    let _ = io
        .to(target.to_string())
        .emit(message.event, &message.payload)
        .await;

    for coalesced in take_coalesced_for_target(target) {
        let _ = io
            .to(target.to_string())
            .emit(coalesced.event, &coalesced.payload)
            .await;
    }
}

fn get_or_spawn_outbox(io: &SocketIo, target: &str) -> TargetOutbox {
    if let Some(outbox) = TARGET_OUTBOXES.get(target) {
        return outbox.clone();
    }

    let (high_sender, mut high_receiver) = mpsc::channel::<OutboxMessage>(OUTBOX_CAPACITY);
    let (normal_sender, mut normal_receiver) = mpsc::channel::<OutboxMessage>(OUTBOX_CAPACITY);
    let outbox = TargetOutbox {
        high: high_sender,
        normal: normal_sender,
    };
    let target_id = target.to_string();
    let io_worker = io.clone();
    TARGET_OUTBOXES.insert(target_id.clone(), outbox.clone());

    tokio::spawn(async move {
        let mut high_closed = false;
        let mut normal_closed = false;

        loop {
            if high_closed && normal_closed {
                break;
            }

            let message = if let Some(message) = take_high_overflow(&target_id) {
                Some(message)
            } else {
                tokio::select! {
                    biased;
                    message = high_receiver.recv(), if !high_closed => {
                        match message {
                            Some(message) => Some(message),
                            None => {
                                high_closed = true;
                                None
                            }
                        }
                    }
                    message = normal_receiver.recv(), if !normal_closed => {
                        match message {
                            Some(message) => Some(message),
                            None => {
                                normal_closed = true;
                                None
                            }
                        }
                    }
                }
            };

            let Some(message) = message else {
                continue;
            };
            deliver_outbox_message(&io_worker, &target_id, message).await;
        }
        TARGET_OUTBOXES.remove(&target_id);
    });

    outbox
}

fn enqueue_emit(io: &SocketIo, target: String, event: &'static str, payload: Value) {
    enqueue_emit_priority(io, target, event, payload, emit_priority(event));
}

fn enqueue_emit_priority(
    io: &SocketIo,
    target: String,
    event: &'static str,
    payload: Value,
    priority: EmitPriority,
) {
    let coalescing = is_coalescing_event(event);
    let message = OutboxMessage {
        event,
        payload,
        priority,
        coalescing,
    };
    let outbox = get_or_spawn_outbox(io, &target);
    let sender = match message.priority {
        EmitPriority::High => outbox.high.clone(),
        EmitPriority::Normal | EmitPriority::Low => outbox.normal.clone(),
    };

    match sender.try_send(message) {
        Ok(()) => {}
        Err(TrySendError::Full(message))
            if message.coalescing || message.priority == EmitPriority::Low =>
        {
            COALESCED_OUTBOX_EVENTS.insert(coalesced_key(&target, message.event), message.payload);
        }
        Err(TrySendError::Full(message)) if message.priority == EmitPriority::High => {
            if !push_high_overflow(&target, message) {
                warn!(
                    target = %target,
                    "dropping high-priority socket message because target overflow is full"
                );
            }
        }
        Err(TrySendError::Full(message)) => {
            warn!(
                target = %target,
                event = message.event,
                "dropping normal-priority socket message because target outbox is full"
            );
        }
        Err(TrySendError::Closed(message)) => {
            TARGET_OUTBOXES.remove(&target);
            let outbox = get_or_spawn_outbox(io, &target);
            let sender = match message.priority {
                EmitPriority::High => outbox.high,
                EmitPriority::Normal | EmitPriority::Low => outbox.normal,
            };
            if let Err(err) = sender.try_send(message) {
                warn!(
                    target = %target,
                    error = ?err,
                    "failed to requeue socket message after closed outbox"
                );
            }
        }
    }
}

fn player_socket_key(room_id: &str, player: &Player) -> String {
    if player.stable_player_id.trim().is_empty() {
        state::legacy_player_key(room_id, &player.username)
    } else {
        player.stable_player_id.clone()
    }
}

fn bind_socket_to_player(state: &ServerState, room_id: &str, player: &Player) {
    let key = player_socket_key(room_id, player);
    state
        .socket_rooms
        .insert(player.id.clone(), room_id.to_string());
    state
        .socket_player_keys
        .insert(player.id.clone(), key.clone());
    state.player_sockets.insert(key, player.id.clone());
}

fn unbind_socket_from_room(state: &ServerState, socket_id: &str) -> Option<String> {
    if let Some((_, key)) = state.socket_player_keys.remove(socket_id) {
        state.player_sockets.remove(&key);
    }
    state
        .socket_rooms
        .remove(socket_id)
        .map(|(_, room_id)| room_id)
}

fn avatar_id_from_payload(data: &Value) -> Option<AvatarId> {
    data.get("avatarId").and_then(AvatarId::from_value)
}

fn hints_from_payload(data: &Value) -> Option<Vec<String>> {
    let hints = data.get("hints")?.as_array()?;
    Some(
        hints
            .iter()
            .filter_map(|hint| hint.as_str().map(str::to_string))
            .collect(),
    )
}

fn player_session_id_from_payload(data: &Value) -> Option<&str> {
    data.get("playerSessionId").and_then(Value::as_str)
}

fn is_empty_avatar(value: &Option<AvatarId>) -> bool {
    value.as_ref().is_none_or(AvatarId::is_empty)
}

fn avatar_to_string(value: &Option<AvatarId>) -> String {
    value
        .as_ref()
        .map(AvatarId::as_key_string)
        .unwrap_or_default()
}

fn prepare_players_broadcast(room: &mut Room, extra: Option<Value>) -> Value {
    // Coalesce short bursts of updatePlayers broadcasts for smoother UI.
    const COOLDOWN_MS: i64 = 120;
    let now = Utc::now().timestamp_millis();

    let mut force_immediate = false;
    let mut extra_no_force: Option<Value> = None;
    if let Some(Value::Object(mut obj)) = extra {
        if obj
            .remove("forceImmediate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            force_immediate = true;
        }
        if !obj.is_empty() {
            extra_no_force = Some(Value::Object(obj));
        }
    }

    if !force_immediate
        && let Some(last) = room._last_players_broadcast_at
        && now - last < COOLDOWN_MS
    {
        room._pending_player_broadcast_extra =
            merge_extra(room._pending_player_broadcast_extra.take(), extra_no_force);
        extra_no_force = None;
    }

    let merged_extra = merge_extra(room._pending_player_broadcast_extra.take(), extra_no_force);
    room._player_broadcast_due_at = None;
    room._last_players_broadcast_at = Some(now);

    let mut payload = serde_json::Map::new();
    payload.insert("players".to_string(), json!(room.players));
    payload.insert("isPublic".to_string(), json!(room.is_public));
    payload.insert(
        "answerSetterId".to_string(),
        room.answer_setter_id
            .as_ref()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
    );

    if let Some(Value::Object(extra_obj)) = merged_extra {
        for (k, v) in extra_obj {
            payload.insert(k, v);
        }
    }

    Value::Object(payload)
}

fn broadcast_players(io: &SocketIo, room_id: &str, room: &mut Room, extra: Option<Value>) {
    let payload = prepare_players_broadcast(room, extra);
    emit_to_room(io, room_id.to_string(), "updatePlayers", payload);
}

fn prepare_players_flush_if_due(room: &mut Room) -> Option<Value> {
    let due_at = room._player_broadcast_due_at?;
    let now = Utc::now().timestamp_millis();
    if now < due_at {
        return None;
    }
    // Clear due flag first to avoid recursion loops.
    room._player_broadcast_due_at = None;
    // Flush pending extras even if no new extra was requested.
    Some(prepare_players_broadcast(room, None))
}

fn flush_players_if_due(io: &SocketIo, room_id: &str, room: &mut Room) {
    if let Some(payload) = prepare_players_flush_if_due(room) {
        emit_to_room(io, room_id.to_string(), "updatePlayers", payload);
    }
}

pub(crate) fn broadcast_lobby_rooms_updated(io: &SocketIo) {
    let sender = {
        let mut slot = LOBBY_OUTBOX.lock().expect("lobby outbox mutex poisoned");
        if let Some(sender) = slot.as_ref() {
            sender.clone()
        } else {
            let (sender, mut receiver) = mpsc::channel::<Value>(LOBBY_OUTBOX_CAPACITY);
            let io = io.clone();
            tokio::spawn(async move {
                while let Some(payload) = receiver.recv().await {
                    let mut latest = payload;
                    while let Ok(next) = receiver.try_recv() {
                        latest = next;
                    }
                    let _ = io.emit("roomsUpdated", &latest).await;
                }
            });
            *slot = Some(sender.clone());
            sender
        }
    };

    match sender.try_send(json!({})) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            warn!("dropping roomsUpdated lobby notification because lobby outbox is full");
        }
        Err(TrySendError::Closed(_)) => {
            if let Ok(mut slot) = LOBBY_OUTBOX.lock() {
                *slot = None;
            }
        }
    }
}

pub(crate) fn emit_to_room(
    io: &SocketIo,
    target: impl Into<String>,
    event: &'static str,
    payload: Value,
) {
    enqueue_emit(io, target.into(), event, payload);
}

fn emit_guess_appended_to_allowed(
    io: &SocketIo,
    room: &Room,
    actor_id: &str,
    entry_payload: Value,
) {
    let actor = match room.players.iter().find(|p| p.id == actor_id) {
        Some(p) => p,
        None => return,
    };

    let payload = json!({
        "username": actor.username,
        "entry": entry_payload,
    });

    for target in &room.players {
        let allowed = target.id == actor.id
            || target.is_answer_setter
            || target.team.as_deref() == Some("0")
            || target.temp_observer
            || (target.team.is_some() && actor.team.is_some() && target.team == actor.team);
        if allowed {
            emit_to_room(io, target.id.clone(), "guessAppended", payload.clone());
        }
    }
}

fn visible_answer_for(game: &crate::socket::state::CurrentGame, player: Option<&Player>) -> Value {
    let can_see_answer = player
        .map(|p| {
            p.is_answer_setter
                || p.team.as_deref() == Some("0")
                || p.temp_observer
                || gameplay::player_has_result(p)
        })
        .unwrap_or(false);

    if can_see_answer {
        game.character.to_value()
    } else {
        Value::Null
    }
}

fn emit_game_start_snapshot(socket: &SocketRef, room: &Room, player_idx: Option<usize>) {
    let Some(game) = room.current_game.as_ref() else {
        return;
    };
    let player = player_idx.and_then(|idx| room.players.get(idx));
    let is_setter = player.map(|p| p.is_answer_setter).unwrap_or(false);
    let _ = socket.emit(
        "gameStart",
        &json!({
            "character": visible_answer_for(game, player),
            "settings": game.settings,
            "players": room.players,
            "isPublic": room.is_public,
            "hints": game.hints,
            "isAnswerSetter": is_setter,
        }),
    );
}

fn emit_game_start_to_room_sockets(io: &SocketIo, room_id: &str, room: &Room) {
    let sockets = io.within(room_id.to_string()).sockets();
    for (idx, player) in room.players.iter().enumerate() {
        let Some(socket) = sockets.iter().find(|s| s.id.to_string() == player.id) else {
            continue;
        };
        emit_game_start_snapshot(socket, room, Some(idx));
    }
}

fn emit_join_room_success(
    socket: &SocketRef,
    io: &SocketIo,
    state: &ServerState,
    room_id: &str,
    success: JoinRoomSuccess,
) {
    let JoinRoomSuccess {
        room,
        player_idx,
        previous_socket_id,
        notify_lobby,
        pre_flush,
    } = success;

    if let Some(payload) = pre_flush {
        emit_to_room(io, room_id.to_string(), "updatePlayers", payload);
    }
    socket.join(room_id.to_string());
    if let Some(previous_socket_id) = previous_socket_id {
        let _ = unbind_socket_from_room(state, &previous_socket_id);
    }
    if let Some(player) = room.players.get(player_idx) {
        bind_socket_to_player(state, room_id, player);
    }
    emit_to_room(
        io,
        room_id.to_string(),
        "updatePlayers",
        json!({
            "players": room.players.clone(),
            "isPublic": room.is_public,
            "answerSetterId": room.answer_setter_id.clone(),
        }),
    );
    let _ = socket.emit(
        "roomNameUpdated",
        &json!({ "roomName": room.room_name.clone() }),
    );

    if notify_lobby {
        broadcast_lobby_rooms_updated(io);
    }

    if let Some(ref game) = room.current_game {
        emit_game_start_snapshot(socket, &room, Some(player_idx));
        let _ = socket.emit(
            "guessHistoryUpdate",
            &json!({
                "guesses": game.guesses,
            }),
        );
        let _ = socket.emit(
            "tagBanStateUpdate",
            &json!({
                "tagBanState": game.tag_ban_state,
            }),
        );
    }
}

fn emit_player_broadcast_result(
    io: &SocketIo,
    room_id: &str,
    result: PlayerBroadcastCommandResult,
) {
    if let Some(payload) = result.pre_flush {
        emit_to_room(io, room_id.to_string(), "updatePlayers", payload);
    }
    if let Some(payload) = result.players_update {
        emit_to_room(io, room_id.to_string(), "updatePlayers", payload);
    }
}

fn answer_reveal_for(room: &Room, player_id: &str) -> Option<CharacterPayload> {
    let game = room.current_game.as_ref()?;
    let player = room.players.iter().find(|p| p.id == player_id)?;
    if visible_answer_for(game, Some(player)).is_null() {
        return None;
    }
    Some(game.character.clone())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicGuess {
    id: i64,
    icon: Value,
    name: Value,
    name_cn: Value,
    name_en: Value,
    gender: Value,
    gender_feedback: Value,
    latest_appearance: Value,
    latest_appearance_feedback: Value,
    earliest_appearance: Value,
    earliest_appearance_feedback: Value,
    highest_rating: Value,
    rating_feedback: Value,
    appearances_count: usize,
    appearances_count_feedback: Value,
    popularity: Value,
    popularity_feedback: Value,
    appearance_ids: Value,
    shared_appearances: Value,
    meta_tags: Value,
    shared_meta_tags: Value,
    is_answer: bool,
}

impl PublicGuess {
    fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug)]
enum PlayerGuessCommandResult {
    Error(&'static str),
    Accepted(Box<PlayerGuessAccepted>),
}

#[derive(Debug)]
struct PlayerGuessAccepted {
    public_guess: PublicGuess,
    is_correct: bool,
    is_partial_correct: bool,
    answer_reveal: Option<CharacterPayload>,
}

#[derive(Debug)]
enum JoinRoomCommandResult {
    Error {
        message: &'static str,
        pre_flush: Option<Value>,
    },
    Joined(Box<JoinRoomSuccess>),
}

#[derive(Debug)]
struct JoinRoomSuccess {
    room: Room,
    player_idx: usize,
    previous_socket_id: Option<String>,
    notify_lobby: bool,
    pre_flush: Option<Value>,
}

#[derive(Debug)]
struct DisconnectCommandResult {
    host_transferred: Option<Value>,
    wait_for_answer_canceled: Option<Value>,
    players_payload: Value,
}

#[derive(Debug)]
struct PlayerBroadcastCommandResult {
    pre_flush: Option<Value>,
    players_update: Option<Value>,
}

fn build_public_guess(guess_data: &CharacterPayload, feedback: &Value) -> PublicGuess {
    PublicGuess {
        id: guess_data.id,
        icon: guess_data.get("image").cloned().unwrap_or(Value::Null),
        name: guess_data.get("name").cloned().unwrap_or(Value::Null),
        name_cn: guess_data.get("nameCn").cloned().unwrap_or(Value::Null),
        name_en: guess_data.get("nameEn").cloned().unwrap_or(Value::Null),
        gender: guess_data.get("gender").cloned().unwrap_or(Value::Null),
        gender_feedback: feedback
            .pointer("/gender/feedback")
            .cloned()
            .unwrap_or(json!("no")),
        latest_appearance: guess_data
            .get("latestAppearance")
            .cloned()
            .unwrap_or(Value::Null),
        latest_appearance_feedback: feedback
            .pointer("/latestAppearance/feedback")
            .cloned()
            .unwrap_or(json!("?")),
        earliest_appearance: guess_data
            .get("earliestAppearance")
            .cloned()
            .unwrap_or(Value::Null),
        earliest_appearance_feedback: feedback
            .pointer("/earliestAppearance/feedback")
            .cloned()
            .unwrap_or(json!("?")),
        highest_rating: guess_data
            .get("highestRating")
            .cloned()
            .unwrap_or(Value::Null),
        rating_feedback: feedback
            .pointer("/rating/feedback")
            .cloned()
            .unwrap_or(json!("?")),
        appearances_count: guess_data
            .get("appearances")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0),
        appearances_count_feedback: feedback
            .pointer("/appearancesCount/feedback")
            .cloned()
            .unwrap_or(json!("?")),
        popularity: guess_data.get("popularity").cloned().unwrap_or(Value::Null),
        popularity_feedback: feedback
            .pointer("/popularity/feedback")
            .cloned()
            .unwrap_or(json!("?")),
        appearance_ids: guess_data
            .get("appearanceIds")
            .cloned()
            .unwrap_or(json!([])),
        shared_appearances: feedback
            .get("shared_appearances")
            .cloned()
            .unwrap_or(json!({ "first": "", "count": 0 })),
        meta_tags: feedback
            .pointer("/metaTags/guess")
            .cloned()
            .unwrap_or(json!([])),
        shared_meta_tags: feedback
            .pointer("/metaTags/shared")
            .cloned()
            .unwrap_or(json!([])),
        is_answer: feedback
            .get("isCorrect")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

pub fn register_handlers(io: SocketIo, server_state: Arc<ServerState>, _db_pools: Arc<DbPools>) {
    // We attach the DB pools as state for the socket layer
    io.clone().ns("/", move |socket: SocketRef, State(_pools): State<Arc<DbPools>>| async move {
        let state = Arc::clone(&server_state);
        let io_clone = io.clone();

        // Handle connection
        info!("Client connected: {}", socket.id);

        let state_create = Arc::clone(&state);
        let io_create = io_clone.clone();
        socket.on("createRoom", move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_create);
            let io_clone = io_create.clone();
            async move {
                // flush pending broadcast if due (event boundary)
                let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let username = data.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string();

                if username.trim().is_empty() {
                    emit_error(&socket, "createRoom", "请输入用户名");
                    return;
                }

                if state.contains_room(&room_id) {
                    emit_error(&socket, "createRoom", "房间已存在，请换一个房间号或直接加入该房间");
                    return;
                }

                if state.room_count() >= 259 {
                    emit_error(&socket, "createRoom", "服务器房间数量已满，请稍后再试");
                    return;
                }

                let avatar_id = avatar_id_from_payload(&data);
                let avatar_image = data.get("avatarImage").and_then(|v| v.as_str()).map(|s| s.to_string());
                let stable_player_id = state::new_player_stable_id_for_session(
                    &room_id,
                    &username,
                    player_session_id_from_payload(&data),
                );

                let player = Player {
                    id: socket.id.to_string(),
                    stable_player_id,
                    username: username.clone(),
                    is_host: true,
                    score: 0,
                    ready: false,
                    attempt_marks: Vec::new(),
                    round_result: None,
                    message: String::new(),
                    team: None,
                    disconnected: false,
                    avatar_id,
                    avatar_image,
                    joined_during_game: None,
                    temp_observer: false,
                    sync_completed_round: None,
                    is_answer_setter: false,
                };

                let room = Room {
                    host: socket.id.to_string(),
                    is_public: true,
                    room_name: String::new(),
                    players: vec![player],
                    last_active: Utc::now().timestamp_millis(),
                    current_game: None,
                    answer_setter_id: None,
                    waiting_for_answer: false,
                    settings: None,
                    _last_players_broadcast_at: None,
                    _pending_player_broadcast_extra: None,
                    _player_broadcast_due_at: None,
                    _player_broadcast_flush_scheduled: false,
                };

                state.insert_room(room_id.clone(), room.clone());
                socket.join(room_id.clone());
                bind_socket_to_player(&state, &room_id, &room.players[0]);

                // Broadcast
                let payload = json!({
                    "players": room.players,
                    "isPublic": room.is_public,
                    "answerSetterId": room.answer_setter_id,
                });
                emit_to_room(
                    &io_clone,
                    room_id.clone(),
                    "updatePlayers",
                    payload,
                );
                let _ = socket.emit("roomNameUpdated", &json!({ "roomName": "" }));
                broadcast_lobby_rooms_updated(&io_clone);

                info!("room {} created by {}", room_id, username);
            }
        });

        let state_join = Arc::clone(&state);
        let io_join = io_clone.clone();
        socket.on("joinRoom", move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_join);
            let io_clone = io_join.clone(); // Clone for use inside the async block
            async move {
                let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let username = data.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string();

                if username.trim().is_empty() {
                    emit_error(&socket, "joinRoom", "请输入用户名");
                    return;
                }

                let incoming_avatar_id = avatar_id_from_payload(&data);
                let incoming_avatar_image = data.get("avatarImage").and_then(|v| v.as_str()).map(|s| s.to_string());
                let incoming_stable_player_id = state::new_player_stable_id_for_session(
                    &room_id,
                    &username,
                    player_session_id_from_payload(&data),
                );

                // 1. Room doesn't exist -> Fallback to createRoom logic
                if !state.contains_room(&room_id) {
                    let player = Player {
                        id: socket.id.to_string(),
                        stable_player_id: incoming_stable_player_id.clone(),
                        username: username.clone(),
                        is_host: true,
                        score: 0,
                        ready: false,
                        attempt_marks: Vec::new(),
                        round_result: None,
                        message: String::new(),
                        team: None,
                        disconnected: false,
                        avatar_id: incoming_avatar_id,
                        avatar_image: incoming_avatar_image,
                        joined_during_game: None,
                        temp_observer: false,
                        sync_completed_round: None,
                        is_answer_setter: false,
                    };

                    let room = Room {
                        host: socket.id.to_string(),
                        is_public: true,
                        room_name: String::new(),
                        players: vec![player],
                        last_active: Utc::now().timestamp_millis(),
                        current_game: None,
                        answer_setter_id: None,
                        waiting_for_answer: false,
                        settings: None,
                        _last_players_broadcast_at: None,
                        _pending_player_broadcast_extra: None,
                        _player_broadcast_due_at: None,
                        _player_broadcast_flush_scheduled: false,
                    };

                    state.insert_room(room_id.clone(), room.clone());
                    socket.join(room_id.clone());
                    bind_socket_to_player(&state, &room_id, &room.players[0]);

                    let payload = json!({
                        "players": room.players,
                        "isPublic": room.is_public,
                        "answerSetterId": room.answer_setter_id,
                    });
                    emit_to_room(
                        &io_clone,
                        room_id.clone(),
                        "updatePlayers",
                        payload,
                    );
                    let _ = socket.emit("roomNameUpdated", &json!({ "roomName": "" }));
                    broadcast_lobby_rooms_updated(&io_clone);

                    info!("room {} created by {} via joinRoom fallback", room_id, username);
                    return;
                }

                // Room exists, mutate through the room command boundary.
                let socket_id = socket.id.to_string();
                let active_socket_ids: HashSet<String> = io_clone
                    .within(room_id.clone())
                    .sockets()
                    .iter()
                    .map(|s| s.id.to_string())
                    .collect();
                let result = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("joinRoom"), |room| {
                        let pre_flush = prepare_players_flush_if_due(room);
                        room.last_active = Utc::now().timestamp_millis();

                        if room.current_game.is_some() {
                            info!("[join observer] room {} in progress", room_id);
                        }

                        let username_lower = username.to_lowercase();
                        let existing_idx = room
                            .players
                            .iter()
                            .position(|p| p.stable_player_id == incoming_stable_player_id)
                            .or_else(|| {
                                room.players
                                    .iter()
                                    .position(|p| p.username.to_lowercase() == username_lower)
                            });

                        if let Some(idx) = existing_idx {
                            if room.players[idx].id == socket_id {
                                info!("{} rejoined room {} idempotently", username, room_id);
                                return JoinRoomCommandResult::Joined(Box::new(JoinRoomSuccess {
                                    room: room.clone(),
                                    player_idx: idx,
                                    previous_socket_id: None,
                                    notify_lobby: false,
                                    pre_flush,
                                }));
                            }

                            if room.players[idx].disconnected {
                                let prev_avatar_id = room.players[idx].avatar_id.clone();

                                let is_incoming_empty = is_empty_avatar(&incoming_avatar_id);
                                let is_prev_empty = is_empty_avatar(&prev_avatar_id);

                                if !is_incoming_empty && !is_prev_empty {
                                    let prev_str = avatar_to_string(&prev_avatar_id);
                                    let incoming_str = avatar_to_string(&incoming_avatar_id);
                                    if prev_str != incoming_str {
                                        warn!(
                                            "avatar mismatch for {} during reconnect: expected {} got {}",
                                            username, prev_str, incoming_str
                                        );
                                        return JoinRoomCommandResult::Error {
                                            message: "头像信息和原玩家不一致，无法作为同一玩家重连；请换一个名字加入",
                                            pre_flush,
                                        };
                                    }
                                }

                                let previous_socket_id = room.players[idx].id.clone();
                                room.players[idx].id = socket_id.clone();
                                room.players[idx].disconnected = false;
                                state::ensure_player_stable_id(&room_id, &mut room.players[idx]);

                                if !is_incoming_empty {
                                    room.players[idx].avatar_id = incoming_avatar_id.clone();
                                    if incoming_avatar_image.is_some() {
                                        room.players[idx].avatar_image =
                                            incoming_avatar_image.clone();
                                    }
                                }

                                if let Some(ref mut game) = room.current_game {
                                    let new_id = socket_id.clone();

                                    let replace_revealers = |states: &mut Vec<TagBanEntry>| {
                                        for entry in states.iter_mut() {
                                            for rev in entry.revealer.iter_mut() {
                                                if rev == &previous_socket_id {
                                                    *rev = new_id.clone();
                                                }
                                            }
                                        }
                                    };

                                    replace_revealers(&mut game.tag_ban_state);
                                    replace_revealers(&mut game.tag_ban_state_pending);
                                }

                                info!("{} reconnected to room {}", username, room_id);
                                return JoinRoomCommandResult::Joined(Box::new(JoinRoomSuccess {
                                    room: room.clone(),
                                    player_idx: idx,
                                    previous_socket_id: Some(previous_socket_id),
                                    notify_lobby: true,
                                    pre_flush,
                                }));
                            }

                            let is_stale = !active_socket_ids.contains(&room.players[idx].id);

                            if is_stale {
                                warn!("stale socket detected for {}; forcing reconnect bind", username);

                                let previous_socket_id = room.players[idx].id.clone();
                                room.players[idx].id = socket_id.clone();
                                room.players[idx].disconnected = false;
                                state::ensure_player_stable_id(&room_id, &mut room.players[idx]);

                                let is_incoming_empty = is_empty_avatar(&incoming_avatar_id);
                                if !is_incoming_empty {
                                    room.players[idx].avatar_id = incoming_avatar_id.clone();
                                    if incoming_avatar_image.is_some() {
                                        room.players[idx].avatar_image =
                                            incoming_avatar_image.clone();
                                    }
                                }

                                if let Some(ref mut game) = room.current_game {
                                    let new_id = socket_id.clone();

                                    let replace_revealers = |states: &mut Vec<TagBanEntry>| {
                                        for entry in states.iter_mut() {
                                            for rev in entry.revealer.iter_mut() {
                                                if rev == &previous_socket_id {
                                                    *rev = new_id.clone();
                                                }
                                            }
                                        }
                                    };
                                    replace_revealers(&mut game.tag_ban_state);
                                    replace_revealers(&mut game.tag_ban_state_pending);
                                }

                                info!("{} rebound to room {} from stale socket", username, room_id);
                                return JoinRoomCommandResult::Joined(Box::new(JoinRoomSuccess {
                                    room: room.clone(),
                                    player_idx: idx,
                                    previous_socket_id: Some(previous_socket_id),
                                    notify_lobby: true,
                                    pre_flush,
                                }));
                            }

                            return JoinRoomCommandResult::Error {
                                message: "这个名字已经在房间里，请换一个名字；如果这是你自己的旧标签页，请关闭旧标签页或刷新当前页",
                                pre_flush,
                            };
                        }

                        if active_room_player_count(room) >= MAX_ROOM_PLAYERS {
                            return JoinRoomCommandResult::Error {
                                message: "房间人数已满，请加入其他房间",
                                pre_flush,
                            };
                        }

                        if let Some(ref inc_id) = incoming_avatar_id {
                            let inc_str = inc_id.as_key_string();
                            let is_taken = room.players.iter().any(|p| {
                                !p.disconnected
                                    && p.avatar_id.is_some()
                                    && p.avatar_id.as_ref().is_some_and(|avatar| {
                                        !avatar.is_empty() && avatar.as_key_string() == inc_str
                                    })
                            });
                            if is_taken {
                                return JoinRoomCommandResult::Error {
                                    message: "头像已被房间内其他玩家使用，请重新抽取头像或换一个名字",
                                    pre_flush,
                                };
                            }
                        }

                        let new_player = Player {
                            id: socket_id,
                            stable_player_id: incoming_stable_player_id,
                            username: username.clone(),
                            is_host: false,
                            score: 0,
                            ready: false,
                            attempt_marks: Vec::new(),
                            round_result: None,
                            message: String::new(),
                            team: if room.current_game.is_some() {
                                Some("0".to_string())
                            } else {
                                None
                            },
                            joined_during_game: Some(room.current_game.is_some()),
                            disconnected: false,
                            avatar_id: incoming_avatar_id,
                            avatar_image: incoming_avatar_image,
                            temp_observer: false,
                            sync_completed_round: None,
                            is_answer_setter: false,
                        };

                        room.players.push(new_player);
                        let new_idx = room.players.len().saturating_sub(1);
                        info!("{} joined room {}", username, room_id);
                        JoinRoomCommandResult::Joined(Box::new(JoinRoomSuccess {
                            room: room.clone(),
                            player_idx: new_idx,
                            previous_socket_id: None,
                            notify_lobby: true,
                            pre_flush,
                        }))
                    })
                    .await;

                match result {
                    Some(JoinRoomCommandResult::Joined(success)) => {
                        emit_join_room_success(&socket, &io_clone, &state, &room_id, *success);
                    }
                    Some(JoinRoomCommandResult::Error { message, pre_flush }) => {
                        if let Some(payload) = pre_flush {
                            emit_to_room(&io_clone, room_id.clone(), "updatePlayers", payload);
                        }
                        emit_error(&socket, "joinRoom", message);
                    }
                    None => {}
                }
            }
        });

        // Register room configuration and gameplay events
        register_room_handlers(socket.clone(), Arc::clone(&state), io_clone.clone(), Arc::clone(&_pools));

        let state_disconnect = Arc::clone(&state);
        let io_disconnect = io_clone.clone();
        socket.on_disconnect(move |socket: SocketRef| async move {
            info!("Client disconnected: {}", socket.id);
            let state = Arc::clone(&state_disconnect);
            let io_clone = io_disconnect.clone();
            let socket_id = socket.id.to_string();
            let Some(room_id) = unbind_socket_from_room(&state, &socket_id) else {
                return;
            };
            let result = state
                .run_room_command(&room_id, RoomCommand::SocketEvent("disconnect"), |room| {
                    let idx = room.players.iter().position(|p| p.id == socket_id)?;

                    let mut host_transferred = None;
                    let mut wait_for_answer_canceled = None;
                    let disconnected_username = room.players[idx].username.clone();
                    let disconnected_was_host = room.host == socket_id;

                    if disconnected_was_host {
                        if let Some((new_host_id, new_host_name)) = room
                            .players
                            .iter()
                            .find(|p| !p.disconnected && p.id != socket_id)
                            .map(|p| (p.id.clone(), p.username.clone()))
                        {
                            room.host = new_host_id.clone();
                            let old_host_name = room.players[idx].username.clone();

                            for p in &mut room.players {
                                if p.id == new_host_id {
                                    p.is_host = true;
                                    p.ready = false;
                                }
                            }
                            room.players[idx].is_host = false;
                            room.players[idx].disconnected = true;

                            host_transferred = Some(json!({
                                    "oldHostName": old_host_name,
                                    "newHostId": new_host_id,
                                    "newHostName": new_host_name
                            }));
                        } else {
                            // No one else left, wait for cleanup or remove.
                            room.players[idx].disconnected = true;
                        }
                    } else {
                        room.players[idx].disconnected = true;
                    }

                    let cancel_message = if room.answer_setter_id.as_deref()
                        == Some(socket_id.as_str())
                    {
                        Some(format!(
                            "指定的出题人 {} 已离开，等待被取消",
                            disconnected_username
                        ))
                    } else if disconnected_was_host && room.waiting_for_answer {
                        Some(format!("房主 {} 已离开，等待出题已取消", disconnected_username))
                    } else {
                        None
                    };
                    if let Some(message) = cancel_message
                        && gameplay::cancel_waiting_for_answer(room).is_some() {
                            wait_for_answer_canceled =
                                Some(build_wait_for_answer_canceled_payload(message));
                        }

                    if let Some(ref mut game) = room.current_game {
                        game.sync_players_completed.remove(&socket_id);
                    }

                    let players_payload =
                        prepare_players_broadcast(room, Some(json!({ "forceImmediate": true })));
                    Some(DisconnectCommandResult {
                        host_transferred,
                        wait_for_answer_canceled,
                        players_payload,
                    })
                })
                .await;
            if let Some(Some(result)) = result {
                if let Some(payload) = result.host_transferred {
                    emit_to_room(&io_clone, room_id.clone(), "hostTransferred", payload);
                }
                if let Some(payload) = result.wait_for_answer_canceled {
                    emit_to_room(&io_clone, room_id.clone(), "waitForAnswerCanceled", payload);
                }
                emit_to_room(
                    &io_clone,
                    room_id.clone(),
                    "updatePlayers",
                    result.players_payload,
                );
                broadcast_lobby_rooms_updated(&io_clone);
            }
        });
    });
}

fn register_room_handlers(
    socket: SocketRef,
    state: Arc<ServerState>,
    io: SocketIo,
    db_pools: Arc<DbPools>,
) {
    let state_ready = Arc::clone(&state);
    let io_ready = io.clone();
    socket.on(
        "toggleReady",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_ready);
            let io_clone = io_ready.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let toggled = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("toggleReady"), |room| {
                        flush_players_if_due(&io_clone, &room_id, room);
                        let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id)
                        else {
                            return Err("连接中断了");
                        };
                        if room.players[player_idx].is_host {
                            return Err("房主不需要准备");
                        }
                        if room.current_game.is_some() {
                            return Err("游戏进行中不能更改准备状态");
                        }
                        if room.waiting_for_answer {
                            return Err("等待出题时不能更改准备状态");
                        }
                        room.players[player_idx].ready = !room.players[player_idx].ready;
                        Ok(PlayerBroadcastCommandResult {
                            pre_flush: None,
                            players_update: Some(prepare_players_broadcast(
                                room,
                                Some(json!({ "forceImmediate": true })),
                            )),
                        })
                    })
                    .await;

                let broadcast = match toggled {
                    Some(Ok(broadcast)) => broadcast,
                    Some(Err(message)) => {
                        emit_error(&socket, "toggleReady", message);
                        return;
                    }
                    None => {
                        emit_error(
                            &socket,
                            "toggleReady",
                            "房间不存在或已经被清理，请回到多人页面重新加入",
                        );
                        return;
                    }
                };

                emit_player_broadcast_result(&io_clone, &room_id, broadcast);
                broadcast_lobby_rooms_updated(&io_clone);
            }
        },
    );

    let state_set = Arc::clone(&state);
    let io_set = io.clone();
    socket.on(
        "updateGameSettings",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_set);
            let io_clone = io_set.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let settings = data.get("settings").map(GameSettings::from_json);
                let updated_settings = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("updateGameSettings"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                                return Err("只有房主可以更改设置");
                            }
                            let Some(settings) = settings else {
                                return Ok((pre_flush, None));
                            };
                            room.settings = Some(settings.clone());
                            room.last_active = Utc::now().timestamp_millis();
                            Ok((pre_flush, Some(settings)))
                        },
                    )
                    .await;

                match updated_settings {
                    Some(Ok((pre_flush, settings))) => {
                        if let Some(payload) = pre_flush {
                            emit_to_room(&io_clone, room_id.clone(), "updatePlayers", payload);
                        }
                        if let Some(settings) = settings {
                            emit_to_room(
                                &io_clone,
                                room_id.clone(),
                                "updateGameSettings",
                                json!({ "settings": settings }),
                            );
                        }
                    }
                    Some(Err(message)) => emit_error(&socket, "updateGameSettings", message),
                    None => {}
                }
            }
        },
    );

    let state_req = Arc::clone(&state);
    socket.on(
        "requestGameSettings",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_req);
            async move {
                let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(room) = state.room_snapshot(room_id).await
                    && let Some(ref settings) = room.settings
                {
                    let _ = socket.emit("updateGameSettings", &json!({ "settings": settings }));
                }
            }
        },
    );

    let state_vis = Arc::clone(&state);
    let io_vis = io.clone();
    socket.on(
        "toggleRoomVisibility",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_vis);
            let io_clone = io_vis.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let changed = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("toggleRoomVisibility"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                                return Err("只有房主可以更改房间状态");
                            }
                            room.is_public = !room.is_public;
                            Ok(PlayerBroadcastCommandResult {
                                pre_flush,
                                players_update: Some(prepare_players_broadcast(
                                    room,
                                    Some(json!({ "forceImmediate": true })),
                                )),
                            })
                        },
                    )
                    .await;
                let broadcast = match changed {
                    Some(Ok(broadcast)) => broadcast,
                    Some(Err(message)) => {
                        emit_error(&socket, "toggleRoomVisibility", message);
                        return;
                    }
                    None => return,
                };
                emit_player_broadcast_result(&io_clone, &room_id, broadcast);
                broadcast_lobby_rooms_updated(&io_clone);
            }
        },
    );

    let state_name = Arc::clone(&state);
    let io_name = io.clone();
    socket.on(
        "updateRoomName",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_name);
            let io_clone = io_name.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let room_name = data
                    .get("roomName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(30)
                    .collect::<String>();
                let actor_id = socket.id.to_string();
                let updated_name = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("updateRoomName"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                                return Err("只有房主可以修改房名");
                            }
                            room.room_name = room_name.clone();
                            Ok((pre_flush, room_name))
                        },
                    )
                    .await;

                match updated_name {
                    Some(Ok((pre_flush, room_name))) => {
                        if let Some(payload) = pre_flush {
                            emit_to_room(&io_clone, room_id.clone(), "updatePlayers", payload);
                        }
                        emit_to_room(
                            &io_clone,
                            room_id.clone(),
                            "roomNameUpdated",
                            json!({ "roomName": room_name }),
                        );
                        broadcast_lobby_rooms_updated(&io_clone);
                    }
                    Some(Err(message)) => emit_error(&socket, "updateRoomName", message),
                    None => {}
                }
            }
        },
    );

    let state_manual = Arc::clone(&state);
    let io_manual = io.clone();
    socket.on(
        "enterManualMode",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_manual);
            let io_clone = io_manual.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("enterManualMode"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                                return Err("只有房主可以进入出题模式");
                            }
                            if room.current_game.is_some() {
                                return Err("游戏进行中不能进入出题模式");
                            }
                            if room.waiting_for_answer {
                                return Err("正在等待出题，不能重复进入出题模式");
                            }
                            for p in &mut room.players {
                                if !p.is_host {
                                    p.ready = true;
                                }
                            }
                            Ok(PlayerBroadcastCommandResult {
                                pre_flush,
                                players_update: Some(prepare_players_broadcast(
                                    room,
                                    Some(json!({ "forceImmediate": true })),
                                )),
                            })
                        },
                    )
                    .await;
                match result {
                    Some(Ok(broadcast)) => {
                        emit_player_broadcast_result(&io_clone, &room_id, broadcast);
                    }
                    Some(Err(message)) => emit_error(&socket, "enterManualMode", message),
                    None => {}
                }
            }
        },
    );

    let state_msg = Arc::clone(&state);
    let io_msg = io.clone();
    socket.on(
        "updatePlayerMessage",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_msg);
            let io_clone = io_msg.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let message = data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("updatePlayerMessage"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            let mut players_update = None;
                            if let Some(p) = room.players.iter_mut().find(|p| p.id == actor_id) {
                                p.message = message;
                                players_update = Some(prepare_players_broadcast(
                                    room,
                                    Some(json!({ "forceImmediate": true })),
                                ));
                            }
                            PlayerBroadcastCommandResult {
                                pre_flush,
                                players_update,
                            }
                        },
                    )
                    .await;
                if let Some(result) = result {
                    emit_player_broadcast_result(&io_clone, &room_id, result);
                }
            }
        },
    );

    let state_team = Arc::clone(&state);
    let io_team = io.clone();
    socket.on(
        "updatePlayerTeam",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_team);
            let io_clone = io_team.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let team = data
                    .get("team")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                // Allow empty string to reset team, else '0'-'8'
                let team_parsed = match team.as_deref() {
                    Some("") | None => None,
                    Some(t)
                        if t.len() == 1
                            && t.chars().next().unwrap_or('9').is_ascii_digit()
                            && t <= "8" =>
                    {
                        Some(t.to_string())
                    }
                    _ => {
                        let _ = socket.emit("error", &json!({ "message": "Invalid team value" }));
                        return;
                    }
                };

                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("updatePlayerTeam"),
                        |room| {
                            let pre_flush = prepare_players_flush_if_due(room);
                            let mut players_update = None;
                            if let Some(player_idx) =
                                room.players.iter().position(|p| p.id == actor_id)
                            {
                                if let Err(message) =
                                    can_player_change_team(room, &room.players[player_idx])
                                {
                                    return Err((message, pre_flush));
                                }
                                room.players[player_idx].team = team_parsed;
                                players_update = Some(prepare_players_broadcast(
                                    room,
                                    Some(json!({ "forceImmediate": true })),
                                ));
                            }
                            Ok(PlayerBroadcastCommandResult {
                                pre_flush,
                                players_update,
                            })
                        },
                    )
                    .await;
                match result {
                    Some(Ok(result)) => emit_player_broadcast_result(&io_clone, &room_id, result),
                    Some(Err((message, pre_flush))) => {
                        if let Some(payload) = pre_flush {
                            emit_to_room(&io_clone, room_id.clone(), "updatePlayers", payload);
                        }
                        emit_error(&socket, "updatePlayerTeam", message);
                    }
                    None => {}
                }
            }
        },
    );

    let state_kick = Arc::clone(&state);
    let io_kick = io.clone();
    socket.on(
        "kickPlayer",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_kick);
            let io_clone = io_kick.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let player_id = data
                    .get("playerId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let kick_effect = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("kickPlayer"), |room| {
                        if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                            return Err("只有房主可以踢出玩家");
                        }
                        if player_id == actor_id {
                            return Err("无法踢出自己");
                        }

                        let Some(idx) = room.players.iter().position(|p| p.id == player_id) else {
                            return Err("找不到要踢出的玩家");
                        };

                        let player_to_kick = room.players.remove(idx);
                        let cancel_message = if room.answer_setter_id.as_deref()
                            == Some(player_to_kick.id.as_str())
                        {
                            let message = format!(
                                "指定的出题人 {} 已被踢出，等待已取消",
                                player_to_kick.username
                            );
                            gameplay::cancel_waiting_for_answer(room).map(|_| message)
                        } else {
                            None
                        };

                        if let Some(ref mut game) = room.current_game {
                            game.sync_players_completed.remove(&player_to_kick.id);
                        }
                        let players_payload = prepare_players_broadcast(
                            room,
                            Some(json!({ "forceImmediate": true })),
                        );

                        Ok((
                            player_to_kick.id,
                            player_to_kick.username,
                            cancel_message,
                            players_payload,
                        ))
                    })
                    .await;

                let (kicked_id, kicked_username, cancel_message, players_payload) =
                    match kick_effect {
                        Some(Ok(effect)) => effect,
                        Some(Err(message)) => {
                            emit_error(&socket, "kickPlayer", message);
                            return;
                        }
                        None => return,
                    };
                emit_to_room(&io_clone, room_id.clone(), "updatePlayers", players_payload);

                if let Some(message) = cancel_message {
                    emit_to_room(
                        &io_clone,
                        room_id.clone(),
                        "waitForAnswerCanceled",
                        json!({ "message": message }),
                    );
                }

                let payload = json!({ "playerId": kicked_id, "username": kicked_username });
                let _ = io_clone
                    .to(kicked_id.clone())
                    .emit("playerKicked", &payload)
                    .await;
                let _ = socket
                    .to(room_id.clone())
                    .emit("playerKicked", &payload)
                    .await;

                let sockets = io_clone.within(room_id).sockets();
                if let Some(kicked) = sockets.iter().find(|s| s.id.to_string() == kicked_id) {
                    let _ = kicked.clone().disconnect();
                }
            }
        },
    );

    let state_transfer = Arc::clone(&state);
    let io_transfer = io.clone();
    socket.on(
        "transferHost",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_transfer);
            let io_clone = io_transfer.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_host_id = data
                    .get("newHostId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let transfer = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("transferHost"), |room| {
                        let current_host_name = {
                            let current = match room.players.iter().find(|p| p.id == actor_id) {
                                Some(p) => p,
                                None => return Err("连接中断了"),
                            };
                            if !current.is_host {
                                return Err("只有房主可以转移权限");
                            }
                            current.username.clone()
                        };

                        if let Some(new_host) = room.players.iter().find(|p| p.id == new_host_id) {
                            if new_host.disconnected {
                                return Err("无法将房主转移给该玩家");
                            }
                        } else {
                            return Err("无法将房主转移给该玩家");
                        }

                        room.host = new_host_id.clone();
                        let mut new_host_name = String::new();
                        for p in &mut room.players {
                            p.is_host = p.id == new_host_id;
                            if p.is_host {
                                p.ready = false;
                                new_host_name = p.username.clone();
                            } else if p.id == actor_id {
                                p.ready = true;
                            }
                        }

                        let wait_for_answer_canceled = if room.waiting_for_answer {
                            let message =
                                format!("房主权限已转移给 {}，等待出题已取消", new_host_name);
                            gameplay::cancel_waiting_for_answer(room)
                                .map(|_| build_wait_for_answer_canceled_payload(message))
                        } else {
                            None
                        };

                        let players_payload = prepare_players_broadcast(
                            room,
                            Some(json!({
                                "answerSetterId": Value::Null,
                                "forceImmediate": true,
                            })),
                        );
                        Ok((
                            current_host_name,
                            new_host_id.clone(),
                            new_host_name,
                            wait_for_answer_canceled,
                            players_payload,
                        ))
                    })
                    .await;

                let (
                    current_host_name,
                    new_host_id,
                    new_host_name,
                    wait_for_answer_canceled,
                    players_payload,
                ) = match transfer {
                    Some(Ok(transfer)) => transfer,
                    Some(Err(message)) => {
                        emit_error(&socket, "transferHost", message);
                        return;
                    }
                    None => return,
                };

                emit_to_room(&io_clone, room_id.clone(), "updatePlayers", players_payload);
                emit_to_room(
                    &io_clone,
                    room_id.clone(),
                    "hostTransferred",
                    json!({
                            "oldHostName": current_host_name,
                            "newHostId": new_host_id,
                            "newHostName": new_host_name
                    }),
                );
                if let Some(payload) = wait_for_answer_canceled {
                    emit_to_room(&io_clone, room_id.clone(), "waitForAnswerCanceled", payload);
                }
            }
        },
    );

    let state_setter = Arc::clone(&state);
    let io_setter = io.clone();
    socket.on(
        "setAnswerSetter",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_setter);
            let io_clone = io_setter.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let setter_id = data
                    .get("setterId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let setter = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("setAnswerSetter"),
                        |room| {
                            let setter_name = validate_answer_setter(room, &actor_id, &setter_id)?;

                            for p in &mut room.players {
                                if p.temp_observer {
                                    p.temp_observer = false;
                                }
                            }
                            room.answer_setter_id = Some(setter_id.clone());
                            room.waiting_for_answer = true;

                            if let Some(team_id) = room
                                .players
                                .iter()
                                .find(|p| p.id == setter_id)
                                .and_then(|p| p.team.clone())
                                && team_id != "0"
                            {
                                for p in &mut room.players {
                                    if p.team.as_deref() == Some(team_id.as_str())
                                        && p.id != setter_id
                                        && !p.is_answer_setter
                                        && !p.disconnected
                                    {
                                        p.temp_observer = true;
                                    }
                                }
                            }

                            let players_payload = prepare_players_broadcast(
                                room,
                                Some(json!({
                                    "answerSetterId": setter_id.clone()
                                })),
                            );

                            Ok((setter_id, setter_name, players_payload))
                        },
                    )
                    .await;

                let (setter_id, setter_name, players_payload) = match setter {
                    Some(Ok(setter)) => setter,
                    Some(Err(message)) => {
                        emit_error(&socket, "setAnswerSetter", message);
                        return;
                    }
                    None => return,
                };

                emit_to_room(&io_clone, room_id.clone(), "updatePlayers", players_payload);
                emit_to_room(
                    &io_clone,
                    room_id.clone(),
                    "waitForAnswer",
                    json!({
                            "answerSetterId": setter_id.clone(),
                            "setterUsername": setter_name
                    }),
                );
                schedule_answer_setter_timeout(
                    Arc::clone(&state),
                    io_clone.clone(),
                    room_id,
                    setter_id,
                );
            }
        },
    );

    let state_start = Arc::clone(&state);
    let io_start = io.clone();
    let pools_start = Arc::clone(&db_pools);
    socket.on(
        "gameStart",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_start);
            let io_clone = io_start.clone();
            let pools = Arc::clone(&pools_start);
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let Some(room_snapshot) = state.room_snapshot(&room_id).await else {
                    return;
                };
                if !room_snapshot
                    .players
                    .iter()
                    .any(|p| p.id == actor_id && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以开始游戏" }));
                    return;
                }
                if room_snapshot.current_game.is_some() {
                    let _ = socket.emit("error", &json!({ "message": "游戏已经在进行中" }));
                    return;
                }

                let all_ready = room_snapshot
                    .players
                    .iter()
                    .all(|p| p.is_host || p.ready || p.disconnected);
                if !all_ready {
                    let _ = socket.emit(
                        "error",
                        &json!({ "message": "所有玩家必须准备好才能开始游戏" }),
                    );
                    return;
                }

                let settings_value = data.get("settings").cloned().unwrap_or_else(|| json!({}));
                let game_settings = GameSettings::from_json(&settings_value);
                let character = if let Some(payload) = data.get("character").cloned() {
                    match CharacterPayload::from_value(payload) {
                        Ok(character) => character,
                        Err(e) => {
                            emit_error(&socket, "gameStart", &format!("指定角色数据异常: {}", e));
                            return;
                        }
                    }
                } else {
                    let game_settings_for_query = game_settings.clone();
                    match db::with_archive_db_timed(
                        Arc::clone(&pools),
                        "socket_game_start_random_character",
                        Duration::from_secs(2),
                        move |conn| {
                            game::random_character_with_conn(conn, &game_settings_for_query)
                        },
                    )
                    .await
                    {
                        Ok((_, payload)) => match CharacterPayload::from_value(payload) {
                            Ok(character) => character,
                            Err(e) => {
                                emit_error(
                                    &socket,
                                    "gameStart",
                                    &format!("随机角色数据异常: {}", e),
                                );
                                return;
                            }
                        },
                        Err(e) => {
                            emit_error(&socket, "gameStart", &format!("随机角色失败: {}", e));
                            return;
                        }
                    }
                };
                let answer_stats = (character.id, character_name_for_stats(&character));

                let started = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("gameStart"), |room| {
                        if !room.players.iter().any(|p| p.id == actor_id && p.is_host) {
                            return Err("只有房主可以开始游戏");
                        }
                        if room.current_game.is_some() {
                            return Err("游戏已经在进行中");
                        }
                        let all_ready = room
                            .players
                            .iter()
                            .all(|p| p.is_host || p.ready || p.disconnected);
                        if !all_ready {
                            return Err("所有玩家必须准备好才能开始游戏");
                        }

                        // Remove disconnected players with 0 score
                        room.players.retain(|p| !p.disconnected || p.score > 0);

                        // gameStart is the non-manual start path: clear any pending setter state
                        let _ = gameplay::cancel_waiting_for_answer(room);

                        gameplay::init_game_state(room, character, Some(game_settings), None, None);

                        emit_game_start_to_room_sockets(&io_clone, &room_id, room);
                        emit_to_room(
                            &io_clone,
                            room_id.clone(),
                            "tagBanStateUpdate",
                            json!({ "tagBanState": [] }),
                        );

                        // Initial sync/nonstop progress (syncWaiting / nonstopProgress)
                        gameplay::emit_sync_and_nonstop_state(room, &room_id, &io_clone, true);

                        info!("game started in {}", room_id);

                        // Ensure updatePlayers gets answerSetterId=null immediately after gameStart.
                        broadcast_players(
                            &io_clone,
                            &room_id,
                            room,
                            Some(json!({
                                "answerSetterId": Value::Null
                            })),
                        );
                        Ok(true)
                    })
                    .await;

                match started {
                    Some(Ok(true)) => {
                        broadcast_lobby_rooms_updated(&io_clone);
                        let stats_pools = Arc::clone(&pools);
                        let (char_id, char_name) = answer_stats;
                        tokio::spawn(async move {
                            stats::record_answer_character_count(stats_pools, char_id, char_name)
                                .await;
                        });
                    }
                    Some(Err(message)) => emit_error(&socket, "gameStart", message),
                    Some(Ok(false)) | None => {}
                }
            }
        },
    );

    let state_cancel_wait = Arc::clone(&state);
    let io_cancel_wait = io.clone();
    socket.on(
        "cancelWaitForAnswer",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_cancel_wait);
            let io_clone = io_cancel_wait.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("cancelWaitForAnswer"),
                        |room| {
                            let can_cancel = room
                                .players
                                .iter()
                                .any(|p| p.id == actor_id && p.is_host && !p.disconnected)
                                || room.answer_setter_id.as_deref() == Some(actor_id.as_str());
                            if !can_cancel {
                                return Err("只有房主或出题人可以取消等待出题");
                            }
                            if !room.waiting_for_answer && room.answer_setter_id.is_none() {
                                return Err("当前没有等待中的出题请求");
                            }

                            let message =
                                if room.answer_setter_id.as_deref() == Some(actor_id.as_str()) {
                                    "出题人已取消出题，等待已取消"
                                } else {
                                    "房主已取消等待出题"
                                };
                            cancel_waiting_for_answer_with_message(room, message)
                                .ok_or("当前没有等待中的出题请求")
                        },
                    )
                    .await;

                let (cancel_payload, players_payload) = match result {
                    Some(Ok(result)) => result,
                    Some(Err(message)) => {
                        emit_error(&socket, "cancelWaitForAnswer", message);
                        return;
                    }
                    None => return,
                };

                emit_to_room(&io_clone, room_id.clone(), "updatePlayers", players_payload);
                emit_to_room(
                    &io_clone,
                    room_id.clone(),
                    "waitForAnswerCanceled",
                    cancel_payload,
                );
                broadcast_lobby_rooms_updated(&io_clone);
            }
        },
    );

    let state_set_ans = Arc::clone(&state);
    let io_set_ans = io.clone();
    let pools_set_ans = Arc::clone(&db_pools);
    socket.on(
        "setAnswer",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_set_ans);
            let io_clone = io_set_ans.clone();
            let pools = Arc::clone(&pools_set_ans);
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let character = match data
                    .get("character")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("missing character"))
                    .and_then(CharacterPayload::from_value)
                {
                    Ok(character) => character,
                    Err(e) => {
                        emit_error(&socket, "setAnswer", &format!("答案角色数据异常: {}", e));
                        return;
                    }
                };
                let answer_stats = (character.id, character_name_for_stats(&character));
                let hints = hints_from_payload(&data);
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("setAnswer"), |room| {
                        let is_setter = room.answer_setter_id.as_deref() == Some(actor_id.as_str());
                        if !is_setter {
                            return Err("只有被指定的出题人可以出题");
                        }
                        if !room.waiting_for_answer {
                            return Err("当前没有等待中的出题请求");
                        }
                        if room.current_game.is_some() {
                            return Err("游戏已经在进行中");
                        }

                        let all_ready = room
                            .players
                            .iter()
                            .all(|p| p.is_host || p.ready || p.disconnected);
                        if !all_ready {
                            return Err("所有玩家必须准备好才能开始游戏");
                        }

                        room.players.retain(|p| !p.disconnected || p.score > 0);
                        let settings = room.settings.clone();

                        let setter_observer_ids =
                            gameplay::setter_teammate_observer_ids(room, &actor_id);
                        gameplay::init_game_state_with_temp_observers(
                            room,
                            character,
                            settings,
                            hints,
                            Some(actor_id.as_str()),
                            Some(&setter_observer_ids),
                        );

                        room.waiting_for_answer = false;
                        room.answer_setter_id = None;

                        if let Some(ref game) = room.current_game {
                            emit_to_room(
                                &io_clone,
                                actor_id.clone(),
                                "guessHistoryUpdate",
                                gameplay::build_guess_history_payload(game),
                            );
                        }

                        gameplay::emit_sync_and_nonstop_state(room, &room_id, &io_clone, true);
                        broadcast_players(
                            &io_clone,
                            &room_id,
                            room,
                            Some(json!({
                                "answerSetterId": Value::Null
                            })),
                        );

                        emit_game_start_to_room_sockets(&io_clone, &room_id, room);
                        emit_to_room(
                            &io_clone,
                            room_id.clone(),
                            "tagBanStateUpdate",
                            json!({ "tagBanState": [] }),
                        );

                        if room
                            .current_game
                            .as_ref()
                            .and_then(|g| g.settings.as_ref())
                            .is_some_and(|s| s.sync_mode)
                        {
                            gameplay::update_sync_progress(room, &room_id, &io_clone);
                        }

                        info!("manual game started in {} by setter", room_id);
                        Ok(())
                    })
                    .await;
                match result {
                    Some(Ok(())) => {
                        let stats_pools = Arc::clone(&pools);
                        let (char_id, char_name) = answer_stats;
                        tokio::spawn(async move {
                            stats::record_answer_character_count(stats_pools, char_id, char_name)
                                .await;
                        });
                    }
                    Some(Err(message)) => emit_error(&socket, "setAnswer", message),
                    None => {}
                }
            }
        },
    );

    // Gameplay events.
    let state_guess = Arc::clone(&state);
    let io_guess = io.clone();
    let pools_guess = Arc::clone(&db_pools);
    socket.on(
        "playerGuess",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_guess);
            let io_clone = io_guess.clone();
            let pools = Arc::clone(&pools_guess);
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let Some(guess_char_id) = data.get("characterId").and_then(|v| v.as_i64()) else {
                    emit_error(&socket, "playerGuess", "猜测数据缺少角色 ID，请重新选择角色");
                    return;
                };

                let Some(room_snapshot) = state.room_snapshot(&room_id).await else {
                    return;
                };
                let Some(game_snapshot) = room_snapshot.current_game.as_ref() else {
                    emit_error(&socket, "playerGuess", "游戏未开始或本轮已经结束，不能提交猜测");
                    return;
                };
                let answer_id_snapshot = game_snapshot.character.id;
                let game_settings = game_snapshot.settings.clone().unwrap_or_default();

                if !room_snapshot.players.iter().any(|p| p.id == actor_id) {
                    emit_error(
                        &socket,
                        "playerGuess",
                        "当前标签页的连接没有绑定到房间玩家，可能是同名标签页重连或刷新导致，请刷新本标签页后重新加入",
                    );
                    return;
                }

                let guess_data = match db::with_archive_db_timed(
                    Arc::clone(&pools),
                    "socket_player_guess_character",
                    Duration::from_secs(2),
                    move |conn| game::character_by_id_with_conn(conn, guess_char_id, &game_settings),
                )
                .await
                {
                    Ok(payload) => match CharacterPayload::from_value(payload) {
                        Ok(character) => character,
                        Err(e) => {
                            emit_error(&socket, "playerGuess", &format!("猜测角色数据异常: {}", e));
                            return;
                        }
                    },
                    Err(e) => {
                        emit_error(&socket, "playerGuess", &format!("加载猜测角色失败: {}", e));
                        return;
                    }
                };
                let guess_stats = (guess_data.id, character_name_for_stats(&guess_data));

                let Some(command_result) = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("playerGuess"), |room| {
                        flush_players_if_due(&io_clone, &room_id, room);
                        room.last_active = Utc::now().timestamp_millis();

                        let Some(answer_character) = room
                            .current_game
                            .as_ref()
                            .map(|game| game.character.clone())
                        else {
                            return PlayerGuessCommandResult::Error(
                                "游戏未开始或本轮已经结束，不能提交猜测",
                            );
                        };
                        if answer_character.id != answer_id_snapshot {
                            return PlayerGuessCommandResult::Error(
                                "本轮状态已变化，请重新提交猜测",
                            );
                        }
                        let game_settings = room
                            .current_game
                            .as_ref()
                            .and_then(|game| game.settings.clone())
                            .unwrap_or_default();
                        let guess_value = guess_data.to_value();
                        let answer_value = answer_character.to_value();
                        let feedback =
                            game::build_feedback(&guess_value, &answer_value, &game_settings);

                        let public_guess = build_public_guess(&guess_data, &feedback);
                        let outcome = match gameplay::apply_guess(
                            room,
                            gameplay::GuessCommand {
                                actor_id: actor_id.clone(),
                                character: guess_data,
                                feedback,
                            },
                        ) {
                            Ok(outcome) => outcome,
                            Err(gameplay::GuessReject::PlayerNotBound) => {
                                return PlayerGuessCommandResult::Error(
                                    "当前标签页的连接没有绑定到房间玩家，可能是同名标签页重连或刷新导致，请刷新本标签页后重新加入",
                                );
                            }
                            Err(gameplay::GuessReject::GameNotRunning) => {
                                return PlayerGuessCommandResult::Error(
                                    "游戏未开始或本轮已经结束，不能提交猜测",
                                );
                            }
                            Err(gameplay::GuessReject::Observer) => {
                                return PlayerGuessCommandResult::Error(
                                    "你当前是旁观者，旁观者不能提交猜测",
                                );
                            }
                            Err(gameplay::GuessReject::AlreadyEnded) => {
                                return PlayerGuessCommandResult::Error(
                                    "本局你已经结束，不能继续猜测",
                                );
                            }
                            Err(gameplay::GuessReject::AttemptExhausted) => {
                                broadcast_players(&io_clone, &room_id, room, None);
                                let _ = gameplay::run_standard_flow(
                                    room, &room_id, &io_clone, false, true,
                                );
                                return PlayerGuessCommandResult::Error(
                                    "本局猜测次数已经用尽，不能继续提交",
                                );
                            }
                            Err(gameplay::GuessReject::GlobalPickDuplicate) => {
                                return PlayerGuessCommandResult::Error(
                                    "【全局BP】该角色已经被其他玩家猜过了",
                                );
                            }
                        };

                        emit_guess_appended_to_allowed(
                            &io_clone,
                            room,
                            &actor_id,
                            json!(outcome.entry.clone()),
                        );

                        // boardcastTeamGuess
                        let mut public_guess_for_team = public_guess.to_value();
                        if let Some(obj) = public_guess_for_team.as_object_mut() {
                            obj.insert(
                                "guessrName".to_string(),
                                Value::String(outcome.player.username.clone()),
                            );
                        }
                        for recipient in room.players.iter() {
                            if recipient.id == actor_id {
                                continue;
                            }
                            let same_team_non_setter = recipient.team.is_some()
                                && outcome.player.team.is_some()
                                && recipient.team == outcome.player.team
                                && !recipient.is_answer_setter;
                            let allowed = same_team_non_setter
                                || recipient.team.as_deref() == Some("0")
                                || recipient.is_answer_setter;
                            if allowed {
                                emit_to_room(
                                    &io_clone,
                                    recipient.id.clone(),
                                    "boardcastTeamGuess",
                                    json!({
                                        "guess": public_guess_for_team,
                                        "playerId": actor_id,
                                        "playerName": outcome.player.username,
                                    }),
                                );
                            }
                        }

                        if outcome.sync_mode {
                            gameplay::update_sync_progress(room, &room_id, &io_clone);
                        }

                        let finalized =
                            gameplay::run_standard_flow(room, &room_id, &io_clone, false, true);
                        if !finalized {
                            broadcast_players(&io_clone, &room_id, room, None);
                        }

                        let answer_reveal = if outcome.is_correct {
                            room.current_game.as_ref().and_then(|game| {
                                room.players.iter().find(|p| p.id == actor_id).and_then(
                                    |player| {
                                        if visible_answer_for(game, Some(player)).is_null() {
                                            None
                                        } else {
                                            Some(game.character.clone())
                                        }
                                    },
                                )
                            })
                        } else {
                            None
                        };

                        PlayerGuessCommandResult::Accepted(Box::new(PlayerGuessAccepted {
                            public_guess,
                            is_correct: outcome.is_correct,
                            is_partial_correct: outcome.is_partial_correct,
                            answer_reveal,
                        }))
                    })
                    .await
                else {
                    return;
                };

                match command_result {
                    PlayerGuessCommandResult::Error(message) => {
                        emit_error(&socket, "playerGuess", message);
                    }
                    PlayerGuessCommandResult::Accepted(accepted) => {
                        let PlayerGuessAccepted {
                            public_guess,
                            is_correct,
                            is_partial_correct,
                            answer_reveal,
                        } = *accepted;
                        let _ = socket.emit(
                            "guessResult",
                            &json!({
                                "guess": public_guess,
                                "isCorrect": is_correct,
                                "isPartialCorrect": is_partial_correct,
                            }),
                        );
                        let stats_pools = Arc::clone(&pools);
                        let (char_id, char_name) = guess_stats;
                        tokio::spawn(async move {
                            stats::record_guess_character_count(stats_pools, char_id, char_name)
                                .await;
                        });
                        if let Some(character) = answer_reveal {
                            let _ = socket.emit(
                                "answerReveal",
                                &json!({
                                    "character": character,
                                }),
                            );
                        }
                    }
                }
            }
        },
    );

    let state_tag = Arc::clone(&state);
    let io_tag = io.clone();
    socket.on(
        "tagBanSharedMetaTags",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_tag);
            let io_clone = io_tag.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tags: Vec<String> = data
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|tags| {
                        tags.iter()
                            .filter_map(|tag| tag.as_str().map(str::trim))
                            .filter(|tag| !tag.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let actor_id = socket.id.to_string();
                let tag_ban_state = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("tagBanSharedMetaTags"),
                        |room| {
                            let player_id = room
                                .players
                                .iter()
                                .find(|p| p.id == actor_id)
                                .map(|p| p.id.clone())?;
                            let game = room.current_game.as_mut()?;
                            let tag_ban = game.settings.as_ref().is_some_and(|s| s.tag_ban);
                            if !tag_ban || tags.is_empty() {
                                return None;
                            }

                            let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);

                            let existing_state_tags: std::collections::HashSet<String> =
                                game.tag_ban_state.iter().map(|e| e.tag.clone()).collect();

                            let target_list = if sync_mode {
                                &mut game.tag_ban_state_pending
                            } else {
                                &mut game.tag_ban_state
                            };

                            let mut changed = false;
                            for tag in tags {
                                if existing_state_tags.contains(&tag) {
                                    continue;
                                }
                                let mut idx = target_list.iter().position(|e| e.tag == tag);
                                if idx.is_none() {
                                    target_list.push(TagBanEntry {
                                        tag,
                                        revealer: Vec::new(),
                                    });
                                    idx = Some(target_list.len() - 1);
                                    changed = true;
                                }
                                let i = idx.unwrap();
                                if target_list[i].revealer.is_empty()
                                    || (sync_mode && !target_list[i].revealer.contains(&player_id))
                                {
                                    target_list[i].revealer.push(player_id.clone());
                                    changed = true;
                                }
                            }

                            if !changed || sync_mode {
                                return None;
                            }
                            Some(game.tag_ban_state.clone())
                        },
                    )
                    .await
                    .flatten();

                if let Some(tag_ban_state) = tag_ban_state {
                    let _ = io_clone
                        .to(room_id)
                        .emit(
                            "tagBanStateUpdate",
                            &json!({
                                "tagBanState": tag_ban_state,
                            }),
                        )
                        .await;
                }
            }
        },
    );

    let state_nonstop = Arc::clone(&state);
    let io_nonstop = io.clone();
    socket.on(
        "nonstopWin",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_nonstop);
            let io_clone = io_nonstop.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(&room_id, RoomCommand::SocketEvent("nonstopWin"), |room| {
                        let mut is_big_win = false;
                        room.last_active = Utc::now().timestamp_millis();
                        if room.current_game.is_none() {
                            return Err("房间不存在或游戏未开始");
                        }
                        let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id)
                        else {
                            return Err("连接中断了");
                        };
                        if room.players[player_idx].temp_observer {
                            return Err("旁观者无法猜测");
                        }
                        if !gameplay::latest_guess_matches_answer(room, &actor_id) {
                            return Err("血战结算必须由服务端确认的正确猜测触发");
                        }

                        let team = room.players[player_idx].team.clone();
                        if let Some(ref t) = team
                            && t != "0"
                        {
                            let teammate_won = room
                                .current_game
                                .as_ref()
                                .map(|g| {
                                    g.nonstop_winners
                                        .iter()
                                        .any(|w| w.team.as_deref() == Some(t.as_str()))
                                })
                                .unwrap_or(false);
                            if teammate_won {
                                return Err("你的队友已经猜对了，你无法继续猜测");
                            }
                        }

                        let already_won = room
                            .current_game
                            .as_ref()
                            .map(|g| g.nonstop_winners.iter().any(|w| w.id == actor_id))
                            .unwrap_or(false);
                        if already_won {
                            return Ok(None);
                        }

                        let raw_guess_count =
                            gameplay::player_attempt_count(&room.players[player_idx]);
                        if !is_big_win && raw_guess_count == 1 {
                            is_big_win = true;
                        }

                        {
                            let p = &mut room.players[player_idx];
                            gameplay::set_player_result(
                                p,
                                if is_big_win {
                                    gameplay::RESULT_BIG_WIN
                                } else {
                                    gameplay::RESULT_WIN
                                },
                            );
                        }

                        if let Some(ref mut game) = room.current_game {
                            game.sync_players_completed.remove(&actor_id);
                        }

                        let is_team_mode = team.is_some() && team.as_deref() != Some("0");
                        if is_team_mode {
                            gameplay::mark_team_victory(room, &room_id, &actor_id, &io_clone);
                        }

                        let sync_mode = room
                            .current_game
                            .as_ref()
                            .and_then(|g| g.settings.as_ref())
                            .is_some_and(|s| s.sync_mode);
                        if sync_mode {
                            // Pre-collect teammate IDs before mutable game borrow
                            let nonstop_teammate_ids: Vec<String> = if is_team_mode {
                                let team_id = team.clone().unwrap_or_default();
                                room.players
                                    .iter()
                                    .filter(|p| {
                                        p.team.as_deref() == Some(team_id.as_str())
                                            && p.id != actor_id
                                            && !p.is_answer_setter
                                            && !p.disconnected
                                    })
                                    .map(|p| p.id.clone())
                                    .collect()
                            } else {
                                vec![]
                            };

                            if let Some(ref mut game) = room.current_game {
                                game.sync_players_completed.insert(actor_id.clone());
                                for id in nonstop_teammate_ids {
                                    game.sync_players_completed.insert(id);
                                }
                            }
                            gameplay::update_sync_progress(room, &room_id, &io_clone);
                        }

                        let initial_total_players = room
                            .current_game
                            .as_ref()
                            .map(|g| g.nonstop_total_players)
                            .unwrap_or(1);
                        let winners_count = room
                            .current_game
                            .as_ref()
                            .map(|g| g.nonstop_winners.len() as i32)
                            .unwrap_or(0);
                        let rank_score = std::cmp::max(1, initial_total_players - winners_count);
                        let total_rounds = room
                            .current_game
                            .as_ref()
                            .and_then(|g| g.settings.as_ref())
                            .map(|s| s.max_attempts as i32)
                            .unwrap_or(10);

                        let score_result = gameplay::calculate_winner_score(
                            gameplay::player_attempt_count(&room.players[player_idx]) as i32,
                            gameplay::player_is_big_winner(&room.players[player_idx]),
                            rank_score,
                            total_rounds,
                        );
                        let score = score_result.total_score;
                        room.players[player_idx].score += score;

                        // Pre-clone player data before mutable game borrow
                        let winner_username = room.players[player_idx].username.clone();
                        let winner_team = room.players[player_idx].team.clone();
                        if let Some(ref mut game) = room.current_game {
                            game.nonstop_winners.push(NonstopWinner {
                                id: actor_id.clone(),
                                username: winner_username,
                                is_big_win,
                                team: winner_team,
                                score,
                                bonuses: NonstopWinnerBonuses {
                                    big_win: score_result.bonuses.big_win,
                                    quick_guess: score_result.bonuses.quick_guess,
                                },
                            });
                        }

                        let finalized =
                            gameplay::run_standard_flow(room, &room_id, &io_clone, false, true);
                        if !finalized {
                            broadcast_players(&io_clone, &room_id, room, None);
                        }
                        Ok(answer_reveal_for(room, &actor_id))
                    })
                    .await;
                match result {
                    Some(Ok(Some(character))) => {
                        let _ = socket.emit(
                            "answerReveal",
                            &json!({
                                "character": character,
                            }),
                        );
                    }
                    Some(Ok(None)) | None => {}
                    Some(Err(message)) => emit_error(&socket, "nonstopWin", message),
                }
            }
        },
    );

    let state_end = Arc::clone(&state);
    let io_end = io.clone();
    socket.on("gameEnd", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_end);
        let io_clone = io_end.clone();
        async move {
            let room_id = data
                .get("roomId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let requested_result = data.get("result").and_then(|v| v.as_str()).unwrap_or("");
            let actor_id = socket.id.to_string();
            let result = state
                .run_room_command(&room_id, RoomCommand::SocketEvent("gameEnd"), |room| {
                    room.last_active = Utc::now().timestamp_millis();
                    let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id)
                    else {
                        return Err("连接中断了");
                    };
                    if room.current_game.is_none() {
                        return Err("游戏未开始或已结束");
                    }
                    let result = if requested_result == "surrender" {
                        "surrender"
                    } else if requested_result == "win" || requested_result == "bigwin" {
                        let guess_id = room
                            .current_game
                            .as_ref()
                            .and_then(|game| {
                                game.guesses.iter().find(|pg| {
                                    pg.username.as_str()
                                        == room.players[player_idx].username.as_str()
                                })
                            })
                            .and_then(|pg| pg.guesses.last())
                            .map(|entry| entry.guess_data.id);
                        let answer_id = room.current_game.as_ref().map(|game| game.character.id);
                        if guess_id.is_some() && guess_id == answer_id {
                            "win"
                        } else {
                            "lose"
                        }
                    } else {
                        "lose"
                    };

                    let raw_guess_count = gameplay::player_attempt_count(&room.players[player_idx]);
                    let mut final_result = result.to_string();
                    if result == "win"
                        && raw_guess_count == 1
                        && !gameplay::player_is_big_winner(&room.players[player_idx])
                    {
                        final_result = "bigwin".to_string();
                    }

                    gameplay::clear_player_result(&mut room.players[player_idx]);
                    let team = room.players[player_idx].team.clone();
                    if let Some(ref t) = team
                        && t != "0"
                        && let Some(ref mut game) = room.current_game
                    {
                        gameplay::clear_team_result(game, t);
                        for p in &mut room.players {
                            if p.team.as_deref() == Some(t.as_str())
                                && !p.is_answer_setter
                                && !p.disconnected
                            {
                                p.round_result = None;
                            }
                        }
                    }

                    let nonstop_mode = room
                        .current_game
                        .as_ref()
                        .and_then(|g| g.settings.as_ref())
                        .is_some_and(|s| s.nonstop_mode);
                    let sync_mode = room
                        .current_game
                        .as_ref()
                        .and_then(|g| g.settings.as_ref())
                        .is_some_and(|s| s.sync_mode);

                    match final_result.as_str() {
                        "surrender" => {
                            gameplay::set_player_result(
                                &mut room.players[player_idx],
                                gameplay::RESULT_SURRENDER,
                            );
                            if let Some(ref t) = team
                                && t != "0"
                                && let Some(ref mut game) = room.current_game
                            {
                                gameplay::set_team_result(game, t, gameplay::RESULT_SURRENDER);
                                for p in &mut room.players {
                                    if p.team.as_deref() == Some(t.as_str())
                                        && !p.is_answer_setter
                                        && !p.disconnected
                                    {
                                        p.round_result =
                                            Some(gameplay::RESULT_SURRENDER.to_string());
                                    }
                                }
                            }
                        }
                        "win" => {
                            gameplay::set_player_result(
                                &mut room.players[player_idx],
                                gameplay::RESULT_WIN,
                            );
                            let winner_username = room.players[player_idx].username.clone();
                            if let Some(ref mut game) = room.current_game
                                && game.first_winner.is_none()
                            {
                                game.first_winner = Some(WinnerMarker {
                                    id: actor_id.clone(),
                                    username: winner_username,
                                    is_big_win: false,
                                    timestamp: Some(Utc::now().timestamp_millis()),
                                });
                            }
                            if !nonstop_mode && team.as_deref().is_some_and(|t| t != "0") {
                                gameplay::mark_team_victory(room, &room_id, &actor_id, &io_clone);
                            }
                        }
                        "bigwin" => {
                            gameplay::set_player_result(
                                &mut room.players[player_idx],
                                gameplay::RESULT_BIG_WIN,
                            );
                            let bigwin_username = room.players[player_idx].username.clone();
                            if let Some(ref mut game) = room.current_game {
                                let should_set = game
                                    .first_winner
                                    .as_ref()
                                    .map(|fw| !fw.is_big_win)
                                    .unwrap_or(true);
                                if game.first_winner.is_none() || should_set {
                                    game.first_winner = Some(WinnerMarker {
                                        id: actor_id.clone(),
                                        username: bigwin_username,
                                        is_big_win: true,
                                        timestamp: Some(Utc::now().timestamp_millis()),
                                    });
                                }
                            }
                            if !nonstop_mode && team.as_deref().is_some_and(|t| t != "0") {
                                gameplay::mark_team_victory(room, &room_id, &actor_id, &io_clone);
                            }
                        }
                        _ => {
                            gameplay::set_player_result(
                                &mut room.players[player_idx],
                                gameplay::RESULT_DEAD,
                            );
                            if let Some(ref t) = team
                                && t != "0"
                                && let Some(ref mut game) = room.current_game
                            {
                                gameplay::set_team_result(game, t, gameplay::RESULT_DEAD);
                                for p in &mut room.players {
                                    if p.team.as_deref() == Some(t.as_str())
                                        && !p.is_answer_setter
                                        && !p.disconnected
                                    {
                                        p.round_result = Some(gameplay::RESULT_DEAD.to_string());
                                    }
                                }
                            }
                        }
                    }

                    if sync_mode {
                        if !nonstop_mode && (final_result == "win" || final_result == "bigwin") {
                            let sync_winner_username = room.players[player_idx].username.clone();
                            if let Some(ref mut game) = room.current_game {
                                game.sync_winner_found = true;
                                game.sync_winner = Some(WinnerMarker {
                                    id: actor_id.clone(),
                                    username: sync_winner_username,
                                    is_big_win: final_result == "bigwin",
                                    timestamp: None,
                                });
                            }
                        }

                        let sync_end_teammate_ids: Vec<String> = if nonstop_mode {
                            team.as_deref()
                                .filter(|t| *t != "0")
                                .map(|t| {
                                    room.players
                                        .iter()
                                        .filter(|p| {
                                            p.team.as_deref() == Some(t)
                                                && p.id != actor_id
                                                && !p.is_answer_setter
                                                && !p.disconnected
                                        })
                                        .map(|p| p.id.clone())
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        } else {
                            vec![]
                        };

                        if let Some(ref mut game) = room.current_game {
                            game.sync_players_completed.insert(actor_id.clone());
                            for id in sync_end_teammate_ids {
                                game.sync_players_completed.insert(id);
                            }
                        }
                        broadcast_players(&io_clone, &room_id, room, None);
                        gameplay::update_sync_progress(room, &room_id, &io_clone);
                    }

                    let finalized =
                        gameplay::run_standard_flow(room, &room_id, &io_clone, false, true);
                    if !finalized {
                        broadcast_players(&io_clone, &room_id, room, None);
                    }
                    Ok(answer_reveal_for(room, &actor_id))
                })
                .await;
            match result {
                Some(Ok(Some(character))) => {
                    let _ = socket.emit(
                        "answerReveal",
                        &json!({
                            "character": character,
                        }),
                    );
                }
                Some(Ok(None)) | None => {}
                Some(Err(message)) => emit_error(&socket, "gameEnd", message),
            }
        }
    });

    let state_ob = Arc::clone(&state);
    let io_ob = io.clone();
    socket.on(
        "enterObserverMode",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_ob);
            let io_clone = io_ob.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let actor_id = socket.id.to_string();
                let result = state
                    .run_room_command(
                        &room_id,
                        RoomCommand::SocketEvent("enterObserverMode"),
                        |room| {
                            room.last_active = Utc::now().timestamp_millis();

                            let Some(player_idx) =
                                room.players.iter().position(|p| p.id == actor_id)
                            else {
                                return Err("连接中断了");
                            };
                            if room.current_game.is_none() {
                                return Err("游戏未开始或已结束");
                            }

                            let has_ended = gameplay::player_has_result(&room.players[player_idx]);
                            let max_attempts = room
                                .current_game
                                .as_ref()
                                .and_then(|g| g.settings.as_ref())
                                .map(|s| s.max_attempts)
                                .unwrap_or(10);
                            let team = room.players[player_idx].team.clone();
                            let is_team_mode = team.is_some() && team.as_deref() != Some("0");

                            let attempt_count = if is_team_mode {
                                let t = team.as_ref().unwrap();
                                room.current_game
                                    .as_ref()
                                    .map(|g| gameplay::team_attempt_count(g, t))
                                    .unwrap_or(0)
                            } else {
                                gameplay::player_attempt_count(&room.players[player_idx])
                            };

                            if !has_ended {
                                let end_result = if attempt_count >= max_attempts {
                                    gameplay::RESULT_DEAD
                                } else {
                                    gameplay::RESULT_SURRENDER
                                };
                                if is_team_mode {
                                    let t = team.as_ref().unwrap().clone();
                                    if let Some(ref mut game) = room.current_game {
                                        gameplay::set_team_result(game, &t, end_result);
                                        for p in &mut room.players {
                                            if p.team.as_deref() == Some(t.as_str())
                                                && !p.is_answer_setter
                                                && !p.disconnected
                                            {
                                                p.round_result = Some(end_result.to_string());
                                            }
                                        }
                                    }
                                } else {
                                    gameplay::set_player_result(
                                        &mut room.players[player_idx],
                                        end_result,
                                    );
                                }
                            }

                            room.players[player_idx].temp_observer = true;
                            let finalized =
                                gameplay::run_standard_flow(room, &room_id, &io_clone, false, true);
                            if !finalized {
                                broadcast_players(&io_clone, &room_id, room, None);
                            }

                            Ok(answer_reveal_for(room, &actor_id))
                        },
                    )
                    .await;
                match result {
                    Some(Ok(Some(character))) => {
                        let _ = socket.emit(
                            "answerReveal",
                            &json!({
                                "character": character,
                            }),
                        );
                    }
                    Some(Ok(None)) | None => {}
                    Some(Err(message)) => emit_error(&socket, "enterObserverMode", message),
                }
            }
        },
    );

    let state_timeout = Arc::clone(&state);
    let io_timeout = io.clone();
    socket.on("timeOut", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_timeout);
        let io_clone = io_timeout.clone();
        async move {
            let room_id = data
                .get("roomId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let actor_id = socket.id.to_string();
            let result = state
                .run_room_command(&room_id, RoomCommand::SocketEvent("timeOut"), |room| {
                    let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id)
                    else {
                        return Err("连接中断了");
                    };
                    if room.current_game.is_none() {
                        return Err("游戏未开始或已结束");
                    }

                    let team = room.players[player_idx].team.clone();
                    let is_team_mode = team.is_some() && team.as_deref() != Some("0");

                    let res = gameplay::handle_player_timeout(room, &actor_id);

                    let reset_timer_targets = if is_team_mode {
                        res.affected_player_ids.clone()
                    } else {
                        Vec::new()
                    };

                    if res.needs_sync_update {
                        gameplay::update_sync_progress(room, &room_id, &io_clone);
                    }

                    let finalized =
                        gameplay::run_standard_flow(room, &room_id, &io_clone, false, true);
                    if !finalized {
                        broadcast_players(&io_clone, &room_id, room, None);
                    }
                    Ok(reset_timer_targets)
                })
                .await;
            match result {
                Some(Ok(reset_timer_targets)) => {
                    for pid in reset_timer_targets {
                        emit_to_room(&io_clone, pid, "resetTimer", json!({}));
                    }
                }
                Some(Err(message)) => emit_error(&socket, "timeOut", message),
                None => {}
            }
        }
    });
}
