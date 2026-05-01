pub mod state;

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
        socket.on("joinRoom", move |socket: SocketRef, Data::<Value>(data)| {
            let state = Arc::clone(&state_join);
            let io_clone = io_clone.clone(); // Clone for use inside the async block
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

        socket.on("submit_guess", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} submitted guess {:?}", socket.id, data);
            // Handle guess logic...
        });

        socket.on("chat", |socket: SocketRef, Data::<Value>(data), State(_pools): State<Arc<DbPools>>| async move {
            info!("User {} chat {:?}", socket.id, data);
            // Handle chat logic...
        });

        socket.on_disconnect(|socket: SocketRef| async move {
            info!("Client disconnected: {}", socket.id);
            // Cleanup player state
        });
    });
}
