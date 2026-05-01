pub mod state;
pub mod gameplay;

use serde_json::{json, Value};
use socketioxide::extract::{Data, SocketRef, State};
use socketioxide::SocketIo;
use std::sync::Arc;
use tracing::{info, warn};
use crate::db::DbPools;
use state::{ServerState, Room, Player};
use chrono::Utc;

pub fn register_handlers(io: SocketIo) {
    // Create global room state
    let server_state = Arc::new(ServerState::default());

    // We attach the DB pools as state for the socket layer
    io.clone().ns("/", move |socket: SocketRef, State(_pools): State<Arc<DbPools>>| async move {
        let state = Arc::clone(&server_state);
        let io_clone = io.clone();

        // Handle connection
        info!("Client connected: {}", socket.id);
        
        let state_create = Arc::clone(&state);
        socket.on("createRoom", move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_create);
            async move {
                let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let username = data.get("username").and_then(|v| v.as_str()).unwrap_or("").to_string();
                
                if username.trim().is_empty() {
                    socket.emit("error", &json!({ "message": "createRoom: 用户名呢" })).ok();
                    return;
                }
                
                if state.rooms.contains_key(&room_id) {
                    socket.emit("error", &json!({ "message": "createRoom: 房间已存在" })).ok();
                    return;
                }
                
                if state.rooms.len() >= 259 {
                    socket.emit("error", &json!({ "message": "createRoom: 服务器已满，请稍后再试" })).ok();
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
                    guesses: String::new(),
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
                };

                state.rooms.insert(room_id.clone(), room.clone());
                let _ = socket.join(room_id.clone());
                
                // Broadcast
                let payload = json!({
                    "players": room.players,
                    "isPublic": room.is_public,
                    "answerSetterId": room.answer_setter_id,
                });
                let _ = socket.to(room_id.clone()).emit("updatePlayers", &payload);
                let _ = socket.emit("updatePlayers", &payload);
                let _ = socket.emit("roomNameUpdated", &json!({ "roomName": "" }));
                
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
                    socket.emit("error", &json!({ "message": "joinRoom: 用户名呢" })).ok();
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
                        guesses: String::new(),
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
                    };

                    state.rooms.insert(room_id.clone(), room.clone());
                    let _ = socket.join(room_id.clone());
                    
                    let payload = json!({
                        "players": room.players,
                        "isPublic": room.is_public,
                        "answerSetterId": room.answer_setter_id,
                    });
                    let _ = socket.to(room_id.clone()).emit("updatePlayers", &payload);
                    let _ = socket.emit("updatePlayers", &payload);
                    let _ = socket.emit("roomNameUpdated", &json!({ "roomName": "" }));
                    
                    info!("room {} created by {} via joinRoom fallback", room_id, username);
                    return;
                }

                // Room exists, acquire write lock
                let mut room = state.rooms.get_mut(&room_id).unwrap();
                room.last_active = Utc::now().timestamp_millis();

                if room.current_game.is_some() {
                    info!("[join observer] room {} in progress", room_id);
                }

                let username_lower = username.to_lowercase();
                let existing_idx = room.players.iter().position(|p| p.username.to_lowercase() == username_lower);

                if let Some(idx) = existing_idx {
                    // Helper to check if a Value representing an avatar is conceptually empty
                    let is_empty_avatar = |val: &Option<Value>| -> bool {
                        val.is_none() || val.as_ref().map_or(true, |v| v.is_null() || (v.is_string() && v.as_str().unwrap().is_empty()) || (v.is_number() && v.as_i64() == Some(0)))
                    };
                    
                    // Helper to safely get avatar as string
                    let avatar_to_string = |val: &Option<Value>| -> String {
                        val.as_ref().map(|v| if v.is_string() { v.as_str().unwrap().to_string() } else { v.to_string() }).unwrap_or_default()
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
                                socket.emit("error", &json!({ "message": "joinRoom: 头像信息不一致，无法重连" })).ok();
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
                        let payload = json!({
                            "players": room.players,
                            "isPublic": room.is_public,
                            "answerSetterId": room.answer_setter_id,
                        });
                        let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload);
                        let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));

                        // Emit snapshot
                        if let Some(ref game) = room.current_game {
                            let is_setter = room.players[idx].is_answer_setter;
                            let _ = socket.emit("gameStart", &json!({
                                "character": game.character,
                                "settings": game.settings,
                                "players": room.players,
                                "isPublic": room.is_public,
                                "hints": game.hints,
                                "isAnswerSetter": is_setter,
                            }));
                            let _ = socket.emit("guessHistoryUpdate", &json!({
                                "guesses": game.guesses,
                                "teamGuesses": game.team_guesses,
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
                            let payload = json!({
                                "players": room.players,
                                "isPublic": room.is_public,
                                "answerSetterId": room.answer_setter_id,
                            });
                            let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload);
                            let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));

                            if let Some(ref game) = room.current_game {
                                let is_setter = room.players[idx].is_answer_setter;
                                let _ = socket.emit("gameStart", &json!({
                                    "character": game.character,
                                    "settings": game.settings,
                                    "players": room.players,
                                    "isPublic": room.is_public,
                                    "hints": game.hints,
                                    "isAnswerSetter": is_setter,
                                }));
                                let _ = socket.emit("guessHistoryUpdate", &json!({
                                    "guesses": game.guesses,
                                    "teamGuesses": game.team_guesses,
                                }));
                                let _ = socket.emit("tagBanStateUpdate", &json!({
                                    "tagBanState": game.tag_ban_state,
                                }));
                            }
                            info!("{} rebound to room {} from stale socket", username, room_id);
                            return;
                        }
                        
                        socket.emit("error", &json!({ "message": "joinRoom: 换个名字吧" })).ok();
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
                        socket.emit("error", &json!({ "message": "joinRoom: 头像已被选用" })).ok();
                        return;
                    }
                }

                let new_player = Player {
                    id: socket.id.to_string(),
                    username: username.clone(),
                    is_host: false,
                    score: 0,
                    ready: false,
                    guesses: String::new(),
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

                let payload = json!({
                    "players": room.players,
                    "isPublic": room.is_public,
                    "answerSetterId": room.answer_setter_id,
                });
                let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload);
                let _ = socket.emit("roomNameUpdated", &json!({ "roomName": room.room_name }));

                if let Some(ref game) = room.current_game {
                    let _ = socket.emit("gameStart", &json!({
                        "character": game.character,
                        "settings": game.settings,
                        "players": room.players,
                        "isPublic": room.is_public,
                        "hints": game.hints,
                        "isAnswerSetter": false,
                    }));
                    let _ = socket.emit("guessHistoryUpdate", &json!({
                        "guesses": game.guesses,
                        "teamGuesses": game.team_guesses,
                    }));
                    let _ = socket.emit("tagBanStateUpdate", &json!({
                        "tagBanState": game.tag_ban_state,
                    }));
                }

                info!("{} joined room {}", username, room_id);
            }
        });

        // Register room configuration and player state events
        register_room_handlers(socket.clone(), Arc::clone(&state), io_clone.clone());

        socket.on("submit_guess", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} submitted guess {:?}", socket.id, data);
            // Handle guess logic...
        });

        socket.on("chat", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} chat {:?}", socket.id, data);
            // Handle chat logic...
        });

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
                            }));
                        } else {
                            // No one else left, wait for cleanup or remove
                            room.players[idx].disconnected = true;
                        }
                    } else {
                        room.players[idx].disconnected = true;
                        if room.answer_setter_id.as_deref() == Some(socket.id.to_string().as_str()) {
                            room.answer_setter_id = None;
                            room.waiting_for_answer = false;
                            let _ = io_clone.to(room_id.clone()).emit("waitForAnswerCanceled", &json!({ "message": format!("指定的出题人 {} 已离开，等待被取消", room.players[idx].username) }));
                        }
                    }
                    
                    if let Some(ref mut game) = room.current_game {
                        game.sync_players_completed.remove(&socket.id.to_string());
                    }

                    let payload = json!({
                        "players": room.players,
                        "isPublic": room.is_public,
                        "answerSetterId": room.answer_setter_id,
                    });
                    let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload);
                    break;
                }
            }
        });
    });
}

fn register_room_handlers(socket: SocketRef, state: Arc<ServerState>, io: SocketIo) {
    let state_ready = Arc::clone(&state);
    let io_ready = io.clone();
    socket.on("toggleReady", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_ready);
        let io_clone = io_ready.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            let Some(player_idx) = room.players.iter().position(|p| p.id == socket.id.to_string()) else {
                let _ = socket.emit("error", &json!({ "message": "连接中断了" }));
                return;
            };
            if room.players[player_idx].is_host {
                let _ = socket.emit("error", &json!({ "message": "房主不需要准备" }));
                return;
            }
            if room.current_game.is_some() {
                let _ = socket.emit("error", &json!({ "message": "游戏进行中不能更改准备状态" }));
                return;
            }
            room.players[player_idx].ready = !room.players[player_idx].ready;
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
        }
    });

    let state_set = Arc::clone(&state);
    let io_set = io.clone();
    socket.on("updateGameSettings", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_set);
        let io_clone = io_set.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以更改设置" }));
                return;
            }
            if let Some(settings) = data.get("settings") {
                room.settings = Some(settings.clone());
                room.last_active = Utc::now().timestamp_millis();
                let _ = io_clone.to(room_id).emit("updateGameSettings", &json!({ "settings": settings }));
            }
        }
    });

    let state_req = Arc::clone(&state);
    socket.on("requestGameSettings", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_req);
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(room) = state.rooms.get(room_id) {
                if let Some(ref settings) = room.settings {
                    let _ = socket.emit("updateGameSettings", &json!({ "settings": settings }));
                }
            }
        }
    });

    let state_vis = Arc::clone(&state);
    let io_vis = io.clone();
    socket.on("toggleRoomVisibility", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_vis);
        let io_clone = io_vis.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以更改房间状态" }));
                return;
            }
            room.is_public = !room.is_public;
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
        }
    });

    let state_name = Arc::clone(&state);
    let io_name = io.clone();
    socket.on("updateRoomName", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_name);
        let io_clone = io_name.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let room_name = data.get("roomName").and_then(|v| v.as_str()).unwrap_or("").trim().chars().take(30).collect::<String>();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以修改房名" }));
                return;
            }
            room.room_name = room_name.clone();
            let _ = io_clone.to(room_id).emit("roomNameUpdated", &json!({ "roomName": room_name }));
        }
    });

    let state_manual = Arc::clone(&state);
    let io_manual = io.clone();
    socket.on("enterManualMode", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_manual);
        let io_clone = io_manual.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以进入出题模式" }));
                return;
            }
            for p in &mut room.players {
                if !p.is_host { p.ready = true; }
            }
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
        }
    });

    let state_msg = Arc::clone(&state);
    let io_msg = io.clone();
    socket.on("updatePlayerMessage", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_msg);
        let io_clone = io_msg.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            if let Some(p) = room.players.iter_mut().find(|p| p.id == socket.id.to_string()) {
                p.message = message;
                let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
                let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
            }
        }
    });

    let state_team = Arc::clone(&state);
    let io_team = io.clone();
    socket.on("updatePlayerTeam", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_team);
        let io_clone = io_team.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let team = data.get("team").and_then(|v| v.as_str()).map(|s| s.to_string());
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            
            // Allow empty string to reset team, else '0'-'8'
            let team_parsed = match team.as_deref() {
                Some("") | None => None,
                Some(t) if t.len() == 1 && t.chars().next().unwrap().is_ascii_digit() && t <= "8" => Some(t.to_string()),
                _ => { let _ = socket.emit("error", &json!({ "message": "Invalid team value" })); return; }
            };

            if let Some(p) = room.players.iter_mut().find(|p| p.id == socket.id.to_string()) {
                p.team = team_parsed;
                let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
                let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
            }
        }
    });

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
                let _ = io_clone.to(room_id.clone()).emit("waitForAnswerCanceled", &json!({ "message": format!("指定的出题人 {} 已被踢出，等待已取消", player_to_kick.username) }));
            }

            let _ = io_clone.to(player_id.clone()).emit("playerKicked", &json!({ "playerId": player_id, "username": player_to_kick.username }));
            let _ = socket.to(room_id.clone()).emit("playerKicked", &json!({ "playerId": player_id, "username": player_to_kick.username }));
            
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id.clone()).emit("updatePlayers", &payload);

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
                }
            }

            let _ = io_clone.to(room_id.clone()).emit("hostTransferred", &json!({
                "oldHostName": current_host_name,
                "newHostId": new_host_id,
                "newHostName": new_host_name
            }));
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
        }
    });

    let state_setter = Arc::clone(&state);
    let io_setter = io.clone();
    socket.on("setAnswerSetter", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_setter);
        let io_clone = io_setter.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let setter_id = data.get("setterId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以选择出题人" })); return;
            }
            
            let setter_name = match room.players.iter().find(|p| p.id == setter_id) {
                Some(p) => p.username.clone(),
                None => { let _ = socket.emit("error", &json!({ "message": "找不到选中的玩家" })); return; }
            };

            room.answer_setter_id = Some(setter_id.clone());
            room.waiting_for_answer = true;

            let _ = io_clone.to(room_id.clone()).emit("waitForAnswer", &json!({
                "answerSetterId": setter_id,
                "setterUsername": setter_name
            }));
            let payload = json!({ "players": room.players, "isPublic": room.is_public, "answerSetterId": room.answer_setter_id });
            let _ = io_clone.to(room_id).emit("updatePlayers", &payload);
        }
    });

    let state_start = Arc::clone(&state);
    let io_start = io.clone();
    socket.on("gameStart", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_start);
        let io_clone = io_start.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            
            if !room.players.iter().any(|p| p.id == socket.id.to_string() && p.is_host) {
                let _ = socket.emit("error", &json!({ "message": "只有房主可以开始游戏" }));
                return;
            }
            if room.current_game.is_some() {
                let _ = socket.emit("error", &json!({ "message": "游戏已经在进行中" }));
                return;
            }
            
            let all_ready = room.players.iter().all(|p| p.is_host || p.ready || p.disconnected);
            if !all_ready {
                let _ = socket.emit("error", &json!({ "message": "所有玩家必须准备好才能开始游戏" }));
                return;
            }

            // Remove disconnected players with 0 score
            room.players.retain(|p| !p.disconnected || p.score > 0);

            let character = data.get("character").cloned().unwrap_or(Value::Null);
            let settings = data.get("settings").cloned();
            
            // Init Game State
            room.current_game = Some(state::CurrentGame {
                character: character.clone(),
                settings: settings.clone(),
                guesses: vec![],
                team_guesses: std::collections::HashMap::new(),
                hints: None,
                sync_round: 1,
                sync_players_completed: std::collections::HashSet::new(),
                sync_winner_found: false,
                sync_winner: None,
                sync_ready_to_end: false,
                sync_round_start_rank: 1,
                nonstop_winners: vec![],
                nonstop_total_players: 0,
                first_winner: None,
                tag_ban_state: vec![],
                tag_ban_state_pending: vec![],
                _last_sync_waiting_key: None,
                _last_sync_waiting_at: 0,
            });

            let mut nonstop_total = 0;
            let mut teams_seen = std::collections::HashSet::new();
            let answer_setter_id = room.answer_setter_id.clone();

            for p in &mut room.players {
                p.guesses = String::new();
                p.message = String::new();
                p.ready = false;
                p.is_answer_setter = answer_setter_id.as_deref() == Some(&p.id);
                p.temp_observer = false;
                p.sync_completed_round = None;
                p.joined_during_game = None;
                
                if !p.disconnected && !p.is_answer_setter && p.team.as_deref() != Some("0") {
                    if let Some(ref t) = p.team {
                        if teams_seen.insert(t.clone()) { nonstop_total += 1; }
                    } else {
                        nonstop_total += 1;
                    }
                }
            }

            if let Some(ref mut game) = room.current_game {
                game.nonstop_total_players = std::cmp::max(1, nonstop_total);
            }

            room.last_active = Utc::now().timestamp_millis();

            let _ = io_clone.to(room_id.clone()).emit("gameStart", &json!({
                "character": character,
                "settings": settings,
                "players": room.players,
                "isPublic": room.is_public,
                "isGameStarted": true,
            }));
            let _ = io_clone.to(room_id.clone()).emit("tagBanStateUpdate", &json!({ "tagBanState": [] }));
            
            info!("game started in {}", room_id);
        }
    });

    let state_set_ans = Arc::clone(&state);
    let io_set_ans = io.clone();
    socket.on("setAnswer", move |socket: SocketRef, Data::<Value>(data)| {
        let state = Arc::clone(&state_set_ans);
        let io_clone = io_set_ans.clone();
        async move {
            let room_id = data.get("roomId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mut room = match state.rooms.get_mut(&room_id) { Some(r) => r, None => return };
            
            let is_setter = room.answer_setter_id.as_deref() == Some(socket.id.to_string().as_str());
            if !is_setter {
                let _ = socket.emit("error", &json!({ "message": "只有被指定的出题人可以出题" })); return;
            }
            if room.current_game.is_some() {
                let _ = socket.emit("error", &json!({ "message": "游戏已经在进行中" })); return;
            }
            
            let all_ready = room.players.iter().all(|p| p.is_host || p.ready || p.disconnected);
            if !all_ready {
                let _ = socket.emit("error", &json!({ "message": "所有玩家必须准备好才能开始游戏" })); return;
            }

            room.players.retain(|p| !p.disconnected || p.score > 0);

            let character = data.get("character").cloned().unwrap_or(Value::Null);
            let hints = data.get("hints").cloned();
            let settings = room.settings.clone();

            room.current_game = Some(state::CurrentGame {
                character: character.clone(),
                settings: settings.clone(),
                guesses: vec![],
                team_guesses: std::collections::HashMap::new(),
                hints: hints.clone(),
                sync_round: 1,
                sync_players_completed: std::collections::HashSet::new(),
                sync_winner_found: false,
                sync_winner: None,
                sync_ready_to_end: false,
                sync_round_start_rank: 1,
                nonstop_winners: vec![],
                nonstop_total_players: 0,
                first_winner: None,
                tag_ban_state: vec![],
                tag_ban_state_pending: vec![],
                _last_sync_waiting_key: None,
                _last_sync_waiting_at: 0,
            });

            let mut nonstop_total = 0;
            let mut teams_seen = std::collections::HashSet::new();
            let answer_setter_id = room.answer_setter_id.clone();

            for p in &mut room.players {
                p.guesses = String::new();
                p.message = String::new();
                p.ready = false;
                p.is_answer_setter = answer_setter_id.as_deref() == Some(&p.id);
                p.temp_observer = false;
                p.sync_completed_round = None;
                p.joined_during_game = None;
                
                if !p.disconnected && !p.is_answer_setter && p.team.as_deref() != Some("0") {
                    if let Some(ref t) = p.team {
                        if teams_seen.insert(t.clone()) { nonstop_total += 1; }
                    } else {
                        nonstop_total += 1;
                    }
                }
            }

            if let Some(ref mut game) = room.current_game {
                game.nonstop_total_players = std::cmp::max(1, nonstop_total);
            }

            room.waiting_for_answer = false;
            room.last_active = Utc::now().timestamp_millis();

            let _ = io_clone.to(room_id.clone()).emit("gameStart", &json!({
                "character": character,
                "settings": settings,
                "players": room.players,
                "isPublic": room.is_public,
                "isGameStarted": true,
                "hints": hints,
            }));
            
            let _ = io_clone.to(room_id.clone()).emit("tagBanStateUpdate", &json!({ "tagBanState": [] }));
            info!("manual game started in {} by setter", room_id);
        }
    });
}
