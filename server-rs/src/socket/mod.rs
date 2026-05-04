pub mod gameplay;
pub mod state;

use crate::db::DbPools;
use crate::routes::game::{self, GameSettings};
use chrono::Utc;
use serde_json::{Value, json};
use socketioxide::SocketIo;
use socketioxide::extract::{Data, SocketRef, State};
use state::{Player, Room, ServerState};
use std::sync::Arc;
use tracing::{info, warn};

fn emit_error(socket: &SocketRef, event: &str, message: &str) {
    let _ = socket.emit(
        "error",
        &json!({
            "event": event,
            "message": format!("{}: {}", event, message),
        }),
    );
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

fn broadcast_players(io: &SocketIo, room_id: &str, room: &mut Room, extra: Option<Value>) {
    // Coalesce short bursts of updatePlayers broadcasts for better performance and
    // smoother UI by coalescing updatePlayers broadcasts.
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

    if !force_immediate {
        if let Some(last) = room._last_players_broadcast_at {
            if now - last < COOLDOWN_MS {
                room._pending_player_broadcast_extra =
                    merge_extra(room._pending_player_broadcast_extra.take(), extra_no_force);
                // Mark as due; will be flushed at the next event boundary.
                let due = last + COOLDOWN_MS;
                room._player_broadcast_due_at = Some(
                    room._player_broadcast_due_at
                        .map(|prev| prev.min(due))
                        .unwrap_or(due),
                );
                return;
            }
        }
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

    let io = io.clone();
    let room_id = room_id.to_string();
    tokio::spawn(async move {
        let _ = io
            .to(room_id)
            .emit("updatePlayers", &Value::Object(payload))
            .await;
    });
}

fn flush_players_if_due(io: &SocketIo, room_id: &str, room: &mut Room) {
    let Some(due_at) = room._player_broadcast_due_at else {
        return;
    };
    let now = Utc::now().timestamp_millis();
    if now < due_at {
        return;
    }
    // Clear due flag first to avoid recursion loops.
    room._player_broadcast_due_at = None;
    // Flush pending extras even if no new extra was requested.
    broadcast_players(io, room_id, room, None);
}

fn broadcast_players_force(io: &SocketIo, room_id: &str, room: &mut Room) {
    broadcast_players(
        io,
        room_id,
        room,
        Some(json!({
            "forceImmediate": true
        })),
    );
}

pub(crate) fn broadcast_lobby_rooms_updated(io: &SocketIo) {
    let io = io.clone();
    tokio::spawn(async move {
        let _ = io.emit("roomsUpdated", &json!({})).await;
    });
}

pub(crate) fn emit_to_room(
    io: &SocketIo,
    target: impl Into<String>,
    event: &'static str,
    payload: Value,
) {
    let io = io.clone();
    let target = target.into();
    tokio::spawn(async move {
        let _ = io.to(target).emit(event, &payload).await;
    });
}

fn emit_guess_history_update_to_allowed(
    io: &SocketIo,
    _room_id: &str,
    room: &Room,
    actor_id: &str,
) {
    let Some(game) = room.current_game.as_ref() else {
        return;
    };

    let actor = match room.players.iter().find(|p| p.id == actor_id) {
        Some(p) => p,
        None => return,
    };

    let payload = json!({
        "guesses": game.guesses,
    });

    for target in &room.players {
        let allowed = target.id == actor.id
            || target.is_answer_setter
            || target.team.as_deref() == Some("0")
            || target.temp_observer
            || (target.team.is_some() && actor.team.is_some() && target.team == actor.team);
        if allowed {
            let io = io.clone();
            let target_id = target.id.clone();
            let payload = payload.clone();
            tokio::spawn(async move {
                let _ = io.to(target_id).emit("guessHistoryUpdate", &payload).await;
            });
        }
    }
}

fn broadcast_guess_history_update(io: &SocketIo, room_id: &str, room: &Room) {
    let Some(game) = room.current_game.as_ref() else {
        return;
    };
    let payload = gameplay::build_guess_history_payload(game);
    let io = io.clone();
    let room_id = room_id.to_string();
    tokio::spawn(async move {
        let _ = io.to(room_id).emit("guessHistoryUpdate", &payload).await;
    });
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
        game.character.clone()
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

fn emit_answer_reveal(socket: &SocketRef, room: &Room, player_id: &str) {
    let Some(game) = room.current_game.as_ref() else {
        return;
    };
    let Some(player) = room.players.iter().find(|p| p.id == player_id) else {
        return;
    };
    if visible_answer_for(game, Some(player)).is_null() {
        return;
    }
    let _ = socket.emit(
        "answerReveal",
        &json!({
            "character": game.character,
        }),
    );
}

fn build_public_guess(guess_data: Value, feedback: &Value) -> Value {
    json!({
        "id": guess_data.get("id").cloned().unwrap_or(Value::Null),
        "icon": guess_data.get("image").cloned().unwrap_or(Value::Null),
        "name": guess_data.get("name").cloned().unwrap_or(Value::Null),
        "nameCn": guess_data.get("nameCn").cloned().unwrap_or(Value::Null),
        "nameEn": guess_data.get("nameEn").cloned().unwrap_or(Value::Null),
        "gender": guess_data.get("gender").cloned().unwrap_or(Value::Null),
        "genderFeedback": feedback.pointer("/gender/feedback").cloned().unwrap_or(json!("no")),
        "latestAppearance": guess_data.get("latestAppearance").cloned().unwrap_or(Value::Null),
        "latestAppearanceFeedback": feedback.pointer("/latestAppearance/feedback").cloned().unwrap_or(json!("?")),
        "earliestAppearance": guess_data.get("earliestAppearance").cloned().unwrap_or(Value::Null),
        "earliestAppearanceFeedback": feedback.pointer("/earliestAppearance/feedback").cloned().unwrap_or(json!("?")),
        "highestRating": guess_data.get("highestRating").cloned().unwrap_or(Value::Null),
        "ratingFeedback": feedback.pointer("/rating/feedback").cloned().unwrap_or(json!("?")),
        "appearancesCount": guess_data.get("appearances").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        "appearancesCountFeedback": feedback.pointer("/appearancesCount/feedback").cloned().unwrap_or(json!("?")),
        "popularity": guess_data.get("popularity").cloned().unwrap_or(Value::Null),
        "popularityFeedback": feedback.pointer("/popularity/feedback").cloned().unwrap_or(json!("?")),
        "appearanceIds": guess_data.get("appearanceIds").cloned().unwrap_or(json!([])),
        "sharedAppearances": feedback.get("shared_appearances").cloned().unwrap_or(json!({ "first": "", "count": 0 })),
        "metaTags": feedback.pointer("/metaTags/guess").cloned().unwrap_or(json!([])),
        "sharedMetaTags": feedback.pointer("/metaTags/shared").cloned().unwrap_or(json!([])),
        "isAnswer": feedback.get("isCorrect").and_then(|v| v.as_bool()).unwrap_or(false),
    })
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

                if state.rooms.contains_key(&room_id) {
                    emit_error(&socket, "createRoom", "房间已存在，请换一个房间号或直接加入该房间");
                    return;
                }

                if state.rooms.len() >= 259 {
                    emit_error(&socket, "createRoom", "服务器房间数量已满，请稍后再试");
                    return;
                }

                let avatar_id = data.get("avatarId").cloned();
                let avatar_image = data.get("avatarImage").and_then(|v| v.as_str()).map(|s| s.to_string());

                let player = Player {
                    id: socket.id.to_string(),
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
                };

                state.rooms.insert(room_id.clone(), room.clone());
                let _ = socket.join(room_id.clone());

                // Broadcast
                let payload = json!({
                    "players": room.players,
                    "isPublic": room.is_public,
                    "answerSetterId": room.answer_setter_id,
                });
                let _ = socket.to(room_id.clone()).emit("updatePlayers", &payload).await;
                let _ = socket.emit("updatePlayers", &payload);
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

                let incoming_avatar_id = data.get("avatarId").cloned();
                let incoming_avatar_image = data.get("avatarImage").and_then(|v| v.as_str()).map(|s| s.to_string());

                // 1. Room doesn't exist -> Fallback to createRoom logic
                if !state.rooms.contains_key(&room_id) {
                    let player = Player {
                        id: socket.id.to_string(),
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
                    };

                    state.rooms.insert(room_id.clone(), room.clone());
                    let _ = socket.join(room_id.clone());

                    let payload = json!({
                        "players": room.players,
                        "isPublic": room.is_public,
                        "answerSetterId": room.answer_setter_id,
                    });
                    let _ = socket.to(room_id.clone()).emit("updatePlayers", &payload).await;
                    let _ = socket.emit("updatePlayers", &payload);
                    let _ = socket.emit("roomNameUpdated", &json!({ "roomName": "" }));
                    broadcast_lobby_rooms_updated(&io_clone);

                    info!("room {} created by {} via joinRoom fallback", room_id, username);
                    return;
                }

                // Room exists, acquire write lock
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                room.last_active = Utc::now().timestamp_millis();

                if room.current_game.is_some() {
                    info!("[join observer] room {} in progress", room_id);
                }

                let username_lower = username.to_lowercase();
                let existing_idx = room.players.iter().position(|p| p.username.to_lowercase() == username_lower);

                if let Some(idx) = existing_idx {
                    if room.players[idx].id == socket.id.to_string() {
                        let _ = socket.join(room_id.clone());
                        broadcast_players_force(&io_clone, &room_id, &mut room);
                        let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));

                        if let Some(ref game) = room.current_game {
                            emit_game_start_snapshot(&socket, &room, Some(idx));
                            let _ = socket.emit("guessHistoryUpdate", &json!({
                                "guesses": game.guesses,
                            }));
                            let _ = socket.emit("tagBanStateUpdate", &json!({
                                "tagBanState": game.tag_ban_state,
                            }));
                        }

                        info!("{} rejoined room {} idempotently", username, room_id);
                        return;
                    }

                    // Helper to check if a Value representing an avatar is conceptually empty
                    let is_empty_avatar = |val: &Option<Value>| -> bool {
                        val.is_none() || val.as_ref().map_or(true, |v| v.is_null() || (v.is_string() && v.as_str().unwrap_or("").is_empty()) || (v.is_number() && v.as_i64() == Some(0)))
                    };

                    // Helper to safely get avatar as string
                    let avatar_to_string = |val: &Option<Value>| -> String {
                        val.as_ref().map(|v| if v.is_string() { v.as_str().unwrap_or("").to_string() } else { v.to_string() }).unwrap_or_default()
                    };

                    // 2. Reconnection Logic
                    if room.players[idx].disconnected {
                        let prev_avatar_id = room.players[idx].avatar_id.clone();

                        let is_incoming_empty = is_empty_avatar(&incoming_avatar_id);
                        let is_prev_empty = is_empty_avatar(&prev_avatar_id);

                        // Reject ONLY IF both are explicitly provided and they mismatch.
                        // If prev was empty, we ACCEPT the new incoming avatar (user drew it after reconnecting).
                        if !is_incoming_empty && !is_prev_empty {
                            let prev_str = avatar_to_string(&prev_avatar_id);
                            let incoming_str = avatar_to_string(&incoming_avatar_id);
                            if prev_str != incoming_str {
                                warn!("avatar mismatch for {} during reconnect: expected {} got {}", username, prev_str, incoming_str);
                                emit_error(&socket, "joinRoom", "头像信息和原玩家不一致，无法作为同一玩家重连；请换一个名字加入");
                                return;
                            }
                        }

                        let previous_socket_id = room.players[idx].id.clone();
                        room.players[idx].id = socket.id.to_string();
                        room.players[idx].disconnected = false;

                        // If incoming has a new avatar, update it (useful if prev was empty and they just drew one)
                        if !is_incoming_empty {
                            room.players[idx].avatar_id = incoming_avatar_id.clone();
                            if incoming_avatar_image.is_some() {
                                room.players[idx].avatar_image = incoming_avatar_image.clone();
                            }
                        }

                        // Update revealer IDs in tagBanState if game is active
                        if let Some(ref mut game) = room.current_game {
                            let old_id_val = json!(previous_socket_id);
                            let new_id_val = json!(socket.id.to_string());

                            let replace_revealers = |states: &mut Vec<Value>| {
                                for entry in states.iter_mut() {
                                    if let Some(obj) = entry.as_object_mut() {
                                        if let Some(revealers) = obj.get_mut("revealer").and_then(|v| v.as_array_mut()) {
                                            for rev in revealers.iter_mut() {
                                                if rev == &old_id_val {
                                                    *rev = new_id_val.clone();
                                                }
                                            }
                                        }
                                    }
                                }
                            };

                            replace_revealers(&mut game.tag_ban_state);
                            replace_revealers(&mut game.tag_ban_state_pending);
                        }

                        let _ = socket.join(room_id.clone());
                        broadcast_players_force(&io_clone, &room_id, &mut room);
                        let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));
                        broadcast_lobby_rooms_updated(&io_clone);

                        // Emit snapshot
                        if let Some(ref game) = room.current_game {
                            emit_game_start_snapshot(&socket, &room, Some(idx));
                            let _ = socket.emit("guessHistoryUpdate", &json!({
                                "guesses": game.guesses,
                            }));
                            let _ = socket.emit("tagBanStateUpdate", &json!({
                                "tagBanState": game.tag_ban_state,
                            }));
                            // Skipping broadcastState for brevity, will implement full state sync later
                        }

                        info!("{} reconnected to room {}", username, room_id);
                        return;
                    } else {
                        // Check if socket is stale
                        let sockets = io_clone.within(room_id.clone()).sockets();
                        let is_stale = !sockets.iter().any(|s| s.id.to_string() == room.players[idx].id);

                        if is_stale {
                            warn!("stale socket detected for {}; forcing reconnect bind", username);

                            let previous_socket_id = room.players[idx].id.clone();
                            room.players[idx].id = socket.id.to_string();
                            room.players[idx].disconnected = false;

                            let is_incoming_empty = is_empty_avatar(&incoming_avatar_id);
                            if !is_incoming_empty {
                                room.players[idx].avatar_id = incoming_avatar_id.clone();
                                if incoming_avatar_image.is_some() {
                                    room.players[idx].avatar_image = incoming_avatar_image.clone();
                                }
                            }

                            if let Some(ref mut game) = room.current_game {
                                let old_id_val = json!(previous_socket_id);
                                let new_id_val = json!(socket.id.to_string());

                                let replace_revealers = |states: &mut Vec<Value>| {
                                    for entry in states.iter_mut() {
                                        if let Some(obj) = entry.as_object_mut() {
                                            if let Some(revealers) = obj.get_mut("revealer").and_then(|v| v.as_array_mut()) {
                                                for rev in revealers.iter_mut() {
                                                    if rev == &old_id_val {
                                                        *rev = new_id_val.clone();
                                                    }
                                                }
                                            }
                                        }
                                    }
                                };
                                replace_revealers(&mut game.tag_ban_state);
                                replace_revealers(&mut game.tag_ban_state_pending);
                            }

                            let _ = socket.join(room_id.clone());
                            broadcast_players_force(&io_clone, &room_id, &mut room);
                            let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));
                            broadcast_lobby_rooms_updated(&io_clone);

                            if let Some(ref game) = room.current_game {
                                emit_game_start_snapshot(&socket, &room, Some(idx));
                                let _ = socket.emit("guessHistoryUpdate", &json!({
                                    "guesses": game.guesses,
                                }));
                                let _ = socket.emit("tagBanStateUpdate", &json!({
                                    "tagBanState": game.tag_ban_state,
                                }));
                            }
                            info!("{} rebound to room {} from stale socket", username, room_id);
                            return;
                        }

                        emit_error(&socket, "joinRoom", "这个名字已经在房间里，请换一个名字；如果这是你自己的旧标签页，请关闭旧标签页或刷新当前页");
                        return;
                    }
                }

                // 3. New Player joining existing room
                if let Some(ref inc_id) = incoming_avatar_id {
                    let inc_str = inc_id.to_string();
                    let is_taken = room.players.iter().any(|p| {
                        !p.disconnected &&
                        p.avatar_id.is_some() &&
                        p.avatar_id.as_ref().unwrap().to_string() != "0" &&
                        p.avatar_id.as_ref().unwrap().to_string() == inc_str
                    });
                    if is_taken {
                        emit_error(&socket, "joinRoom", "头像已被房间内其他玩家使用，请重新抽取头像或换一个名字");
                        return;
                    }
                }

                let new_player = Player {
                    id: socket.id.to_string(),
                    username: username.clone(),
                    is_host: false,
                    score: 0,
                    ready: false,
                    attempt_marks: Vec::new(),
                    round_result: None,
                    message: String::new(),
                    team: if room.current_game.is_some() { Some("0".to_string()) } else { None },
                    joined_during_game: Some(room.current_game.is_some()),
                    disconnected: false,
                    avatar_id: incoming_avatar_id,
                    avatar_image: incoming_avatar_image,
                    temp_observer: false,
                    sync_completed_round: None,
                    is_answer_setter: false,
                };

                room.players.push(new_player);
                let _ = socket.join(room_id.clone());

                broadcast_players_force(&io_clone, &room_id, &mut room);
                let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));
                broadcast_lobby_rooms_updated(&io_clone);

                if let Some(ref game) = room.current_game {
                    let new_idx = room.players.len().saturating_sub(1);
                    emit_game_start_snapshot(&socket, &room, Some(new_idx));
                    let _ = socket.emit("guessHistoryUpdate", &json!({
                        "guesses": game.guesses,
                    }));
                    let _ = socket.emit("tagBanStateUpdate", &json!({
                        "tagBanState": game.tag_ban_state,
                    }));
                }

                info!("{} joined room {}", username, room_id);
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

            // Find room containing the disconnected socket and mark disconnected
            for mut room_entry in state.rooms.iter_mut() {
                let room_id = room_entry.key().clone();
                let room = room_entry.value_mut();

                if let Some(idx) = room.players.iter().position(|p| p.id == socket.id.to_string()) {
                    if room.host == socket.id.to_string() {
                        if let Some(new_host) = room.players.iter().find(|p| !p.disconnected && p.id != socket.id.to_string()) {
                            room.host = new_host.id.clone();
                            let new_host_id = new_host.id.clone();
                            let new_host_name = new_host.username.clone();
                            let old_host_name = room.players[idx].username.clone();

                            for p in &mut room.players {
                                if p.id == new_host_id {
                                    p.is_host = true;
                                    p.ready = false;
                                }
                            }
                            room.players[idx].is_host = false;
                            room.players[idx].disconnected = true;

                            let _ = io_clone.to(room_id.clone()).emit("hostTransferred", &json!({
                                "oldHostName": old_host_name,
                                "newHostId": new_host_id,
                                "newHostName": new_host_name
                            })).await;
                        } else {
                            // No one else left, wait for cleanup or remove
                            room.players[idx].disconnected = true;
                        }
                    } else {
                        room.players[idx].disconnected = true;
                        if room.answer_setter_id.as_deref() == Some(socket.id.to_string().as_str()) {
                            room.answer_setter_id = None;
                            room.waiting_for_answer = false;
                            let _ = io_clone.to(room_id.clone()).emit("waitForAnswerCanceled", &json!({ "message": format!("指定的出题人 {} 已离开，等待被取消", room.players[idx].username) })).await;
                        }
                    }

                    if let Some(ref mut game) = room.current_game {
                        game.sync_players_completed.remove(&socket.id.to_string());
                    }

                    broadcast_players_force(&io_clone, &room_id, room);
                    broadcast_lobby_rooms_updated(&io_clone);
                    break;
                }
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => {
                        emit_error(
                            &socket,
                            "playerGuess",
                            "房间不存在或已经被清理，请回到多人页面重新加入",
                        );
                        return;
                    }
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                let Some(player_idx) = room
                    .players
                    .iter()
                    .position(|p| p.id == socket.id.to_string())
                else {
                    let _ = socket.emit("error", &json!({ "message": "连接中断了" }));
                    return;
                };
                if room.players[player_idx].is_host {
                    let _ = socket.emit("error", &json!({ "message": "房主不需要准备" }));
                    return;
                }
                if room.current_game.is_some() {
                    let _ =
                        socket.emit("error", &json!({ "message": "游戏进行中不能更改准备状态" }));
                    return;
                }
                room.players[player_idx].ready = !room.players[player_idx].ready;
                broadcast_players_force(&io_clone, &room_id, &mut room);
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                if !room
                    .players
                    .iter()
                    .any(|p| p.id == socket.id.to_string() && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以更改设置" }));
                    return;
                }
                if let Some(settings) = data.get("settings") {
                    room.settings = Some(settings.clone());
                    room.last_active = Utc::now().timestamp_millis();
                    let _ = io_clone
                        .to(room_id)
                        .emit("updateGameSettings", &json!({ "settings": settings }))
                        .await;
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
                if let Some(room) = state.rooms.get(room_id) {
                    if let Some(ref settings) = room.settings {
                        let _ = socket.emit("updateGameSettings", &json!({ "settings": settings }));
                    }
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                if !room
                    .players
                    .iter()
                    .any(|p| p.id == socket.id.to_string() && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以更改房间状态" }));
                    return;
                }
                room.is_public = !room.is_public;
                broadcast_players_force(&io_clone, &room_id, &mut room);
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                if !room
                    .players
                    .iter()
                    .any(|p| p.id == socket.id.to_string() && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以修改房名" }));
                    return;
                }
                room.room_name = room_name.clone();
                let _ = io_clone
                    .to(room_id)
                    .emit("roomNameUpdated", &json!({ "roomName": room_name }))
                    .await;
                broadcast_lobby_rooms_updated(&io_clone);
            }
        },
    );

    let state_manual = Arc::clone(&state);
    let io_manual = io.clone();
    socket.on("enterManualMode", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_manual);
        let io_clone = io_manual.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            flush_players_if_due(&io_clone, &room_id, &mut room);
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以进入出题模式" }));
                return;
            }
            for p in &mut room.players {
                if !p.is_host { p.ready = true; }
            }
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload).await;
        }
    });

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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                if let Some(p) = room
                    .players
                    .iter_mut()
                    .find(|p| p.id == socket.id.to_string())
                {
                    p.message = message;
                    broadcast_players_force(&io_clone, &room_id, &mut room);
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);

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

                if let Some(p) = room
                    .players
                    .iter_mut()
                    .find(|p| p.id == socket.id.to_string())
                {
                    p.team = team_parsed;
                    broadcast_players_force(&io_clone, &room_id, &mut room);
                }
            }
        },
    );

    let state_kick = Arc::clone(&state);
    let io_kick = io.clone();
    socket.on("kickPlayer", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_kick);
        let io_clone = io_kick.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let player_id = data.get("playerId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以踢出玩家" })); return;
            }
            if player_id == socket.id.to_string() {
                let _ = socket.emit("error", &json!({ "message": "无法踢出自己" })); return;
            }

            let Some(idx) = room.players.iter().position(|p| p.id == player_id) else {
                let _ = socket.emit("error", &json!({ "message": "找不到要踢出的玩家" })); return;
            };

            let player_to_kick = room.players.remove(idx);

            if room.answer_setter_id.as_deref() == Some(player_to_kick.id.as_str()) {
                room.answer_setter_id = None;
                room.waiting_for_answer = false;
                let _ = io_clone.to(room_id.clone()).emit("waitForAnswerCanceled", &json!({ "message": format!("指定的出题人 {} 已被踢出，等待已取消", player_to_kick.username) })).await;
            }

            let _ = io_clone.to(player_id.clone()).emit("playerKicked", &json!({ "playerId": player_id, "username": player_to_kick.username })).await;
            let _ = socket.to(room_id.clone()).emit("playerKicked", &json!({ "playerId": player_id, "username": player_to_kick.username })).await;

            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload).await;

            if let Some(ref mut game) = room.current_game {
                game.sync_players_completed.remove(&player_id);
            }
            let sockets = io_clone.within(room_id).sockets();
            if let Some(kicked) = sockets.iter().find(|s| s.id.to_string() == player_id) {
                let _ = kicked.clone().disconnect();
            }
        }
    });

    let state_transfer = Arc::clone(&state);
    let io_transfer = io.clone();
    socket.on("transferHost", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_transfer);
        let io_clone = io_transfer.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let new_host_id = data.get("newHostId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };

            let current_host_name = {
                let current = match room.players.iter().find(|p| p.id == socket.id.to_string()) {
                    Some(p) => p,
                    None => return,
                };
                if !current.is_host {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以转移权限" })); return;
                }
                current.username.clone()
            };

            if let Some(new_host) = room.players.iter().find(|p| p.id == new_host_id) {
                if new_host.disconnected {
                    let _ = socket.emit("error", &json!({ "message": "无法将房主转移给该玩家" })); return;
                }
            } else {
                let _ = socket.emit("error", &json!({ "message": "无法将房主转移给该玩家" })); return;
            }

            room.host = new_host_id.clone();
            let mut new_host_name = String::new();
            for p in &mut room.players {
                p.is_host = p.id == new_host_id;
                if p.is_host {
                    p.ready = false;
                    new_host_name = p.username.clone();
                } else if p.id == socket.id.to_string() {
                    p.ready = true;
                }
            }

            let _ = io_clone.to(room_id.clone()).emit("hostTransferred", &json!({
                "oldHostName": current_host_name,
                "newHostId": new_host_id,
                "newHostName": new_host_name
            })).await;
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload).await;
        }
    });

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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };

                if !room
                    .players
                    .iter()
                    .any(|p| p.id == socket.id.to_string() && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以选择出题人" }));
                    return;
                }

                let setter_name = match room.players.iter().find(|p| p.id == setter_id) {
                    Some(p) => p.username.clone(),
                    None => {
                        let _ = socket.emit("error", &json!({ "message": "找不到选中的玩家" }));
                        return;
                    }
                };

                gameplay::revert_setter_observers(&mut room, &room_id, &io_clone);
                room.answer_setter_id = Some(setter_id.clone());
                room.waiting_for_answer = true;

                gameplay::apply_setter_observers(&mut room, &room_id, &setter_id, &io_clone);

                let _ = io_clone
                    .to(room_id.clone())
                    .emit(
                        "waitForAnswer",
                        &json!({
                            "answerSetterId": setter_id,
                            "setterUsername": setter_name
                        }),
                    )
                    .await;
                broadcast_players(
                    &io_clone,
                    &room_id,
                    &mut room,
                    Some(json!({
                        "answerSetterId": setter_id
                    })),
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };

                if !room
                    .players
                    .iter()
                    .any(|p| p.id == socket.id.to_string() && p.is_host)
                {
                    let _ = socket.emit("error", &json!({ "message": "只有房主可以开始游戏" }));
                    return;
                }
                if room.current_game.is_some() {
                    let _ = socket.emit("error", &json!({ "message": "游戏已经在进行中" }));
                    return;
                }

                let all_ready = room
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

                // Remove disconnected players with 0 score
                room.players.retain(|p| !p.disconnected || p.score > 0);

                // gameStart is the non-manual start path: clear any pending setter state
                gameplay::revert_setter_observers(&mut room, &room_id, &io_clone);
                room.answer_setter_id = None;
                room.waiting_for_answer = false;

                let settings = data.get("settings").cloned();
                let settings_value = settings.clone().unwrap_or_else(|| json!({}));
                let game_settings = GameSettings::from_json(&settings_value);
                let character = match game::random_character(&pools, &game_settings) {
                    Ok((_, payload)) => payload,
                    Err(e) => {
                        emit_error(&socket, "gameStart", &format!("随机角色失败: {}", e));
                        return;
                    }
                };

                gameplay::init_game_state(
                    &mut room,
                    character.clone(),
                    settings.clone(),
                    None,
                    None,
                );

                emit_game_start_to_room_sockets(&io_clone, &room_id, &room);
                let _ = io_clone
                    .to(room_id.clone())
                    .emit("tagBanStateUpdate", &json!({ "tagBanState": [] }))
                    .await;

                // Initial sync/nonstop progress (syncWaiting / nonstopProgress)
                gameplay::emit_sync_and_nonstop_state(&mut room, &room_id, &io_clone, true);

                info!("game started in {}", room_id);

                // Ensure updatePlayers gets answerSetterId=null immediately after gameStart.
                broadcast_players(
                    &io_clone,
                    &room_id,
                    &mut room,
                    Some(json!({
                        "answerSetterId": Value::Null
                    })),
                );
                broadcast_lobby_rooms_updated(&io_clone);
            }
        },
    );

    let state_set_ans = Arc::clone(&state);
    let io_set_ans = io.clone();
    socket.on(
        "setAnswer",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_set_ans);
            let io_clone = io_set_ans.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };

                let is_setter =
                    room.answer_setter_id.as_deref() == Some(socket.id.to_string().as_str());
                if !is_setter {
                    emit_error(&socket, "setAnswer", "只有被指定的出题人可以出题");
                    return;
                }
                if room.current_game.is_some() {
                    emit_error(&socket, "setAnswer", "游戏已经在进行中");
                    return;
                }

                let all_ready = room
                    .players
                    .iter()
                    .all(|p| p.is_host || p.ready || p.disconnected);
                if !all_ready {
                    emit_error(&socket, "setAnswer", "所有玩家必须准备好才能开始游戏");
                    return;
                }

                room.players.retain(|p| !p.disconnected || p.score > 0);

                let character = data.get("character").cloned().unwrap_or(Value::Null);
                let hints = data.get("hints").cloned();
                let settings = room.settings.clone();

                gameplay::apply_setter_observers(
                    &mut room,
                    &room_id,
                    socket.id.to_string().as_str(),
                    &io_clone,
                );
                gameplay::init_game_state(
                    &mut room,
                    character.clone(),
                    settings.clone(),
                    hints.clone(),
                    Some(socket.id.to_string().as_str()),
                );

                room.waiting_for_answer = false;
                room.answer_setter_id = None;

                if let Some(ref game) = room.current_game {
                    let _ = socket.emit(
                        "guessHistoryUpdate",
                        &gameplay::build_guess_history_payload(game),
                    );
                }

                gameplay::emit_sync_and_nonstop_state(&mut room, &room_id, &io_clone, true);
                broadcast_players(
                    &io_clone,
                    &room_id,
                    &mut room,
                    Some(json!({
                        "answerSetterId": Value::Null
                    })),
                );

                emit_game_start_to_room_sockets(&io_clone, &room_id, &room);
                let _ = io_clone
                    .to(room_id.clone())
                    .emit("tagBanStateUpdate", &json!({ "tagBanState": [] }))
                    .await;

                if room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("syncMode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    gameplay::update_sync_progress(&mut room, &room_id, &io_clone);
                }

                info!("manual game started in {} by setter", room_id);
            }
        },
    );

    // Gameplay events.
    let state_guess = Arc::clone(&state);
    let io_guess = io.clone();
    socket.on(
        "playerGuess",
        move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_guess);
            let io_clone = io_guess.clone();
            async move {
                let room_id = data
                    .get("roomId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                flush_players_if_due(&io_clone, &room_id, &mut room);
                room.last_active = Utc::now().timestamp_millis();

                let actor_id = socket.id.to_string();
                let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id) else {
                    emit_error(
                        &socket,
                        "playerGuess",
                        "当前标签页的连接没有绑定到房间玩家，可能是同名标签页重连或刷新导致，请刷新本标签页后重新加入",
                    );
                    return;
                };
                if room.current_game.is_none() {
                    emit_error(&socket, "playerGuess", "游戏未开始或本轮已经结束，不能提交猜测");
                    return;
                }

                let player = room.players[player_idx].clone();
                if player.team.as_deref() == Some("0") || player.temp_observer {
                    emit_error(&socket, "playerGuess", "你当前是旁观者，旁观者不能提交猜测");
                    return;
                }
                if gameplay::player_has_result(&player) {
                    emit_error(&socket, "playerGuess", "本局你已经结束，不能继续猜测");
                    return;
                }

                let guess_data = data.get("guessData").cloned().unwrap_or(Value::Null);
                let guess_id_ok = guess_data
                    .get("id")
                    .and_then(|v| if v.is_null() { None } else { Some(v) })
                    .is_some();
                if !guess_id_ok {
                    emit_error(&socket, "playerGuess", "猜测数据缺少角色 ID，请重新选择角色");
                    return;
                }

                let answer = room
                    .current_game
                    .as_ref()
                    .map(|game| game.character.clone())
                    .unwrap_or(Value::Null);
                let settings_value = room
                    .current_game
                    .as_ref()
                    .and_then(|game| game.settings.clone())
                    .unwrap_or_else(|| json!({}));
                let feedback = game::build_feedback(
                    &guess_data,
                    &answer,
                    &GameSettings::from_json(&settings_value),
                );
                let is_correct = feedback
                    .get("isCorrect")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let is_partial_correct = feedback
                    .get("isPartialCorrect")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Pre-check attempt limit before writing this guess
                let pre_limit = gameplay::enforce_attempt_limit(&mut room, &actor_id, false);
                if pre_limit.exhausted {
                    broadcast_players(&io_clone, &room_id, &mut room, None);
                    broadcast_guess_history_update(&io_clone, &room_id, &room);
                    let _ =
                        gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
                    emit_error(&socket, "playerGuess", "本局猜测次数已经用尽，不能继续提交");
                    return;
                }

                // Global BP
                let global_pick = room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("globalPick"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let sync_mode = room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("syncMode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let nonstop_mode = room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("nonstopMode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if global_pick && !sync_mode {
                    let character_id = guess_data.get("id").cloned().unwrap_or(Value::Null);
                    let already = room
                        .current_game
                        .as_ref()
                        .and_then(|g| Some(&g.guesses))
                        .map(|guesses| {
                            guesses.iter().any(|pg| {
                                let other =
                                    pg.get("username").and_then(|v| v.as_str()).unwrap_or("")
                                        != player.username;
                                if !other {
                                    return false;
                                }
                                let list = pg.get("guesses").and_then(|v| v.as_array());
                                list.is_some_and(|arr| {
                                    arr.iter().any(|g| {
                                        g.get("guessData").and_then(|gd| gd.get("id"))
                                            == Some(&character_id)
                                    })
                                })
                            })
                        })
                        .unwrap_or(false);
                    if already && (!nonstop_mode || !is_correct) {
                        emit_error(
                            &socket,
                            "playerGuess",
                            "【全局BP】该角色已经被其他玩家猜过了",
                        );
                        return;
                    }
                }

                let public_guess = build_public_guess(guess_data.clone(), &feedback);
                let _ = socket.emit(
                    "guessResult",
                    &json!({
                        "guess": public_guess,
                        "isCorrect": is_correct,
                        "isPartialCorrect": is_partial_correct,
                    }),
                );

                // Write guess history
                if let Some(ref mut game) = room.current_game {
                    let entry = json!({
                        "playerId": actor_id,
                        "playerName": player.username,
                        "isCorrect": is_correct,
                        "isPartialCorrect": is_partial_correct,
                        "guessData": guess_data,
                    });

                    let mut found = false;
                    for pg in game.guesses.iter_mut() {
                        if pg.get("username").and_then(|v| v.as_str())
                            == Some(player.username.as_str())
                        {
                            if let Some(list) = pg.get_mut("guesses").and_then(|v| v.as_array_mut())
                            {
                                list.push(entry.clone());
                            }
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        game.guesses.push(json!({
                            "username": player.username,
                            "guesses": [entry],
                        }));
                    }
                }

                emit_guess_history_update_to_allowed(&io_clone, &room_id, &room, &actor_id);

                // boardcastTeamGuess
                let mut public_guess_for_team = public_guess.clone();
                if let Some(obj) = public_guess_for_team.as_object_mut() {
                    obj.insert(
                        "guessrName".to_string(),
                        Value::String(player.username.clone()),
                    );
                }
                for recipient in room.players.iter() {
                    if recipient.id == actor_id {
                        continue;
                    }
                    let same_team_non_setter = recipient.team.is_some()
                        && player.team.is_some()
                        && recipient.team == player.team
                        && !recipient.is_answer_setter;
                    let allowed = same_team_non_setter
                        || recipient.team.as_deref() == Some("0")
                        || recipient.is_answer_setter;
                    if allowed {
                        let _ = io_clone
                            .to(recipient.id.clone())
                            .emit(
                                "boardcastTeamGuess",
                                &json!({
                                    "guess": public_guess_for_team,
                                    "playerId": actor_id,
                                    "playerName": player.username,
                                }),
                            )
                            .await;
                    }
                }

                // Apply mark to guesses (player/team)
                let mark = if !is_correct && is_partial_correct {
                    gameplay::ATTEMPT_PARTIAL
                } else if is_correct {
                    gameplay::ATTEMPT_CORRECT
                } else {
                    gameplay::ATTEMPT_WRONG
                };

                let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
                if is_team_mode {
                    let team_id = player.team.clone().unwrap();
                    let team_attempts = {
                        let game = room.current_game.as_mut().unwrap();
                        gameplay::push_team_attempt(game, &team_id, mark);
                        game
                            .team_attempt_marks
                            .get(&team_id)
                            .cloned()
                            .unwrap_or_default()
                    };

                    for p in &mut room.players {
                        if p.team.as_deref() == Some(&team_id)
                            && !p.is_answer_setter
                            && !p.disconnected
                        {
                            p.attempt_marks = team_attempts.clone();
                        }
                    }
                } else {
                    if let Some(p) = room.players.iter_mut().find(|p| p.id == actor_id) {
                        gameplay::push_player_attempt(p, mark);
                    }
                }

                // Sync mode progress
                if sync_mode {
                    if !is_correct {
                        // Pre-collect teammate IDs before mutable game borrow
                        let teammate_ids_to_complete: Vec<String> = if is_team_mode {
                            let team_id = player.team.clone().unwrap_or_default();
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
                            for id in teammate_ids_to_complete {
                                game.sync_players_completed.insert(id);
                            }
                        }
                    }
                    gameplay::update_sync_progress(&mut room, &room_id, &io_clone);
                }

                // Post-check attempt limit (non-nonstop)
                if !nonstop_mode {
                    let _ = gameplay::enforce_attempt_limit(&mut room, &actor_id, is_correct);
                }

                let finalized =
                    gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
                if !finalized {
                    broadcast_players(&io_clone, &room_id, &mut room, None);
                }
                if is_correct {
                    emit_answer_reveal(&socket, &room, &actor_id);
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
                let tags = data
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                let actor_id = socket.id.to_string();
                let Some(player) = room.players.iter().find(|p| p.id == actor_id).cloned() else {
                    return;
                };
                let Some(ref mut game) = room.current_game else {
                    return;
                };
                let tag_ban = game
                    .settings
                    .as_ref()
                    .and_then(|s| s.get("tagBan"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !tag_ban {
                    return;
                }
                if tags.is_empty() {
                    return;
                }

                let sync_mode = game
                    .settings
                    .as_ref()
                    .and_then(|s| s.get("syncMode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let existing_state_tags: std::collections::HashSet<String> = game
                    .tag_ban_state
                    .iter()
                    .filter_map(|e| e.get("tag").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .collect();

                let target_list = if sync_mode {
                    &mut game.tag_ban_state_pending
                } else {
                    &mut game.tag_ban_state
                };

                let mut changed = false;
                for tag in tags {
                    let Some(tag_name) = tag.as_str() else {
                        continue;
                    };
                    if existing_state_tags.contains(tag_name) {
                        continue;
                    }
                    let mut idx = target_list
                        .iter()
                        .position(|e| e.get("tag").and_then(|v| v.as_str()) == Some(tag_name));
                    if idx.is_none() {
                        target_list.push(json!({"tag": tag_name, "revealer": []}));
                        idx = Some(target_list.len() - 1);
                        changed = true;
                    }
                    let i = idx.unwrap();
                    let existing = target_list[i]
                        .get("revealer")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if existing.is_empty() {
                        if let Some(obj) = target_list[i].as_object_mut() {
                            obj.insert(
                                "revealer".to_string(),
                                Value::Array(vec![Value::String(player.id.clone())]),
                            );
                        }
                        changed = true;
                    } else if sync_mode {
                        let has = existing
                            .iter()
                            .any(|v| v.as_str() == Some(player.id.as_str()));
                        if !has {
                            let mut new_list = existing;
                            new_list.push(Value::String(player.id.clone()));
                            if let Some(obj) = target_list[i].as_object_mut() {
                                obj.insert("revealer".to_string(), Value::Array(new_list));
                            }
                        }
                    }
                }

                if !changed || sync_mode {
                    return;
                }
                let _ = io_clone
                    .to(room_id)
                    .emit(
                        "tagBanStateUpdate",
                        &json!({
                            "tagBanState": game.tag_ban_state,
                        }),
                    )
                    .await;
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
                let mut is_big_win = false;
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                room.last_active = Utc::now().timestamp_millis();
                let actor_id = socket.id.to_string();
                if room.current_game.is_none() {
                    emit_error(&socket, "nonstopWin", "房间不存在或游戏未开始");
                    return;
                }
                let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id) else {
                    emit_error(&socket, "nonstopWin", "连接中断了");
                    return;
                };
                if room.players[player_idx].temp_observer {
                    emit_error(&socket, "nonstopWin", "旁观者无法猜测");
                    return;
                }

                let team = room.players[player_idx].team.clone();
                if let Some(ref t) = team {
                    if t != "0" {
                        let teammate_won = room
                            .current_game
                            .as_ref()
                            .map(|g| {
                                g.nonstop_winners.iter().any(|w| {
                                    let Some(wid) = w.get("id").and_then(|v| v.as_str()) else {
                                        return false;
                                    };
                                    room.players
                                        .iter()
                                        .find(|p| p.id == wid)
                                        .and_then(|p| p.team.clone())
                                        .as_deref()
                                        == Some(t.as_str())
                                })
                            })
                            .unwrap_or(false);
                        if teammate_won {
                            emit_error(&socket, "nonstopWin", "你的队友已经猜对了，你无法继续猜测");
                            return;
                        }
                    }
                }

                let already_won = room
                    .current_game
                    .as_ref()
                    .map(|g| {
                        g.nonstop_winners.iter().any(|w| {
                            w.get("id").and_then(|v| v.as_str()) == Some(actor_id.as_str())
                        })
                    })
                    .unwrap_or(false);
                if already_won {
                    return;
                }

                let raw_guess_count = gameplay::player_attempt_count(&room.players[player_idx]);
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
                    gameplay::mark_team_victory(&mut room, &room_id, &actor_id, &io_clone);
                }

                let sync_mode = room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("syncMode"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
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
                    gameplay::update_sync_progress(&mut room, &room_id, &io_clone);
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
                    .and_then(|s| s.get("maxAttempts"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(10) as i32;

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
                    game.nonstop_winners.push(json!({
                        "id": actor_id,
                        "username": winner_username,
                        "isBigWin": is_big_win,
                        "team": winner_team,
                        "score": score,
                        "bonuses": score_result.bonuses,
                    }));
                }

                let finalized =
                    gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
                if !finalized {
                    broadcast_players(&io_clone, &room_id, &mut room, None);
                }
                emit_answer_reveal(&socket, &room, &actor_id);
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
            let mut room = match state.rooms.get_mut(&room_id) {
                Some(r) => r,
                None => return,
            };
            room.last_active = Utc::now().timestamp_millis();
            let actor_id = socket.id.to_string();
            let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id) else {
                emit_error(&socket, "gameEnd", "连接中断了");
                return;
            };
            if room.current_game.is_none() {
                emit_error(&socket, "gameEnd", "游戏未开始或已结束");
                return;
            }
            let result = if requested_result == "surrender" {
                "surrender"
            } else if requested_result == "win" || requested_result == "bigwin" {
                let guess_id = room
                    .current_game
                    .as_ref()
                    .and_then(|game| {
                        game.guesses.iter().find(|pg| {
                            pg.get("username").and_then(|v| v.as_str())
                                == Some(room.players[player_idx].username.as_str())
                        })
                    })
                    .and_then(|pg| pg.get("guesses").and_then(|v| v.as_array()))
                    .and_then(|arr| arr.last())
                    .and_then(|entry| entry.get("guessData"))
                    .and_then(|guess| guess.get("id"))
                    .cloned();
                let answer_id = room
                    .current_game
                    .as_ref()
                    .and_then(|game| game.character.get("id"))
                    .cloned();
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
            if let Some(ref t) = team {
                if t != "0" {
                    if let Some(ref mut game) = room.current_game {
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
                }
            }

            let nonstop_mode = room
                .current_game
                .as_ref()
                .and_then(|g| g.settings.as_ref())
                .and_then(|s| s.get("nonstopMode"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let sync_mode = room
                .current_game
                .as_ref()
                .and_then(|g| g.settings.as_ref())
                .and_then(|s| s.get("syncMode"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            match final_result.as_str() {
                "surrender" => {
                    gameplay::set_player_result(
                        &mut room.players[player_idx],
                        gameplay::RESULT_SURRENDER,
                    );
                    if let Some(ref t) = team {
                        if t != "0" {
                            if let Some(ref mut game) = room.current_game {
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
                    }
                }
                "win" => {
                    gameplay::set_player_result(
                        &mut room.players[player_idx],
                        gameplay::RESULT_WIN,
                    );
                    let winner_username = room.players[player_idx].username.clone();
                    if let Some(ref mut game) = room.current_game {
                        if game.first_winner.is_none() {
                            game.first_winner = Some(json!({
                                "id": actor_id,
                                "username": winner_username,
                                "isBigWin": false,
                                "timestamp": Utc::now().timestamp_millis(),
                            }));
                        }
                    }
                    if !nonstop_mode {
                        if team.as_deref().is_some_and(|t| t != "0") {
                            gameplay::mark_team_victory(&mut room, &room_id, &actor_id, &io_clone);
                        }
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
                            .and_then(|fw| fw.get("isBigWin").and_then(|v| v.as_bool()))
                            .map(|is_big| !is_big)
                            .unwrap_or(true);
                        if game.first_winner.is_none() || should_set {
                            game.first_winner = Some(json!({
                                "id": actor_id,
                                "username": bigwin_username,
                                "isBigWin": true,
                                "timestamp": Utc::now().timestamp_millis(),
                            }));
                        }
                    }
                    if !nonstop_mode {
                        if team.as_deref().is_some_and(|t| t != "0") {
                            gameplay::mark_team_victory(&mut room, &room_id, &actor_id, &io_clone);
                        }
                    }
                }
                _ => {
                    gameplay::set_player_result(
                        &mut room.players[player_idx],
                        gameplay::RESULT_DEAD,
                    );
                    if let Some(ref t) = team {
                        if t != "0" {
                            if let Some(ref mut game) = room.current_game {
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
                }
            }

            if sync_mode {
                if !nonstop_mode && (final_result == "win" || final_result == "bigwin") {
                    let sync_winner_username = room.players[player_idx].username.clone();
                    if let Some(ref mut game) = room.current_game {
                        game.sync_winner_found = true;
                        game.sync_winner = Some(json!({
                            "id": actor_id,
                            "username": sync_winner_username,
                            "isBigWin": final_result == "bigwin",
                        }));
                    }
                }

                // Pre-collect teammate IDs before mutable game borrow
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
                broadcast_players(&io_clone, &room_id, &mut room, None);
                gameplay::update_sync_progress(&mut room, &room_id, &io_clone);
            }

            let finalized =
                gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
            if !finalized {
                broadcast_players(&io_clone, &room_id, &mut room, None);
            }
            emit_answer_reveal(&socket, &room, &actor_id);
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
                let mut room = match state.rooms.get_mut(&room_id) {
                    Some(r) => r,
                    None => return,
                };
                room.last_active = Utc::now().timestamp_millis();

                let actor_id = socket.id.to_string();
                let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id) else {
                    emit_error(&socket, "enterObserverMode", "连接中断了");
                    return;
                };
                if room.current_game.is_none() {
                    emit_error(&socket, "enterObserverMode", "游戏未开始或已结束");
                    return;
                }

                let has_ended = gameplay::player_has_result(&room.players[player_idx]);
                let max_attempts = room
                    .current_game
                    .as_ref()
                    .and_then(|g| g.settings.as_ref())
                    .and_then(|s| s.get("maxAttempts"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(10) as usize;
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
                        gameplay::set_player_result(&mut room.players[player_idx], end_result);
                    }
                }

                room.players[player_idx].temp_observer = true;
                let finalized =
                    gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
                if !finalized {
                    broadcast_players(&io_clone, &room_id, &mut room, None);
                }
                emit_answer_reveal(&socket, &room, &actor_id);
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
            let mut room = match state.rooms.get_mut(&room_id) {
                Some(r) => r,
                None => return,
            };

            let actor_id = socket.id.to_string();
            let Some(player_idx) = room.players.iter().position(|p| p.id == actor_id) else {
                emit_error(&socket, "timeOut", "连接中断了");
                return;
            };
            if room.current_game.is_none() {
                emit_error(&socket, "timeOut", "游戏未开始或已结束");
                return;
            }

            let team = room.players[player_idx].team.clone();
            let is_team_mode = team.is_some() && team.as_deref() != Some("0");

            let res = gameplay::handle_player_timeout(&mut room, &actor_id);

            if is_team_mode {
                for pid in &res.affected_player_ids {
                    let _ = io_clone
                        .to(pid.clone())
                        .emit("resetTimer", &json!({}))
                        .await;
                }
            }

            if res.needs_sync_update {
                gameplay::update_sync_progress(&mut room, &room_id, &io_clone);
            }

            broadcast_guess_history_update(&io_clone, &room_id, &room);

            let finalized =
                gameplay::run_standard_flow(&mut room, &room_id, &io_clone, false, true);
            if !finalized {
                broadcast_players(&io_clone, &room_id, &mut room, None);
            }
        }
    });
}
