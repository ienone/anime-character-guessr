use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use socketioxide::SocketIo;
use std::sync::Arc;

use crate::socket::state::{Room, ServerState};
use crate::socket::{MAX_ROOM_PLAYERS, broadcast_lobby_rooms_updated, emit_to_room};

#[derive(Clone)]
pub struct RoomState {
    pub state: Arc<ServerState>,
    pub io: SocketIo,
}

fn active_player_count(players: &[crate::socket::state::Player]) -> usize {
    players.iter().filter(|p| !p.disconnected).count()
}

pub fn room_routes(state: Arc<ServerState>, io: SocketIo) -> Router {
    let room_state = RoomState { state, io };
    Router::new()
        .route("/quick-join", get(quick_join))
        .route("/room-count", get(room_count))
        .route("/list-rooms", get(list_rooms))
        .route("/room-info/{id}", get(room_info))
        .route("/clean-rooms", get(clean_rooms))
        .route("/close-room/{id}", get(close_room_get))
        .route("/close-room/{id}", post(close_room_post))
        .with_state(room_state)
}

#[derive(Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

#[derive(Deserialize)]
struct AdminQuery {
    token: Option<String>,
}

fn configured_admin_token() -> Option<String> {
    std::env::var("ROOM_ADMIN_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn is_admin_request(headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let Some(expected) = configured_admin_token() else {
        return false;
    };

    let header_token_matches = headers
        .get("x-admin-token")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected);
    let bearer_token_matches = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == expected);
    let query_token_matches = query_token.is_some_and(|value| value == expected);

    header_token_matches || bearer_token_matches || query_token_matches
}

fn admin_forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "admin token required" })),
    )
        .into_response()
}

/// GET /quick-join?lang=en
async fn quick_join(State(rs): State<RoomState>, Query(q): Query<LangQuery>) -> impl IntoResponse {
    let public_rooms: Vec<String> = rs
        .state
        .room_snapshots()
        .await
        .into_iter()
        .filter(|(_, room)| room.is_public && active_player_count(&room.players) < MAX_ROOM_PLAYERS)
        .map(|(id, _)| id)
        .collect();

    if public_rooms.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "没有可用的公开房间" })),
        )
            .into_response();
    }

    use rand::seq::IndexedRandom;
    let room_id: Option<String> = {
        let mut rng = rand::rng();
        public_rooms.choose(&mut rng).cloned()
    }; // rng dropped here

    let room_id = match room_id {
        Some(id) => id,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "没有可用的公开房间" })),
            )
                .into_response();
        }
    };

    let base_url =
        std::env::var("CLIENT_URL").unwrap_or_else(|_| "http://localhost:5173".to_string());
    let client_url = if q.lang.as_deref() == Some("en") {
        "https://vertikarl.github.io/anime-character-guessr-english/#".to_string()
    } else {
        base_url
    };
    let url = format!("{}/multiplayer/{}", client_url, room_id);
    Json(json!({ "url": url })).into_response()
}

/// GET /room-count
async fn room_count(State(rs): State<RoomState>) -> impl IntoResponse {
    let command_stats = rs.state.room_command_stats_total();
    Json(json!({
        "count": rs.state.room_count(),
        "queuedRoomCommands": command_stats.queued,
        "processedRoomCommands": command_stats.processed,
        "rejectedRoomCommands": command_stats.rejected,
        "totalRoomCommandMicros": command_stats.total_micros,
        "maxRoomCommandMicros": command_stats.max_micros,
    }))
}

/// GET /list-rooms
async fn list_rooms(State(rs): State<RoomState>) -> impl IntoResponse {
    let rooms: Vec<Value> = rs
        .state
        .room_snapshots()
        .await
        .into_iter()
        .map(|(id, room)| public_room_payload(id, &room))
        .collect();
    Json(rooms)
}

fn public_room_payload(id: String, room: &Room) -> Value {
    let host_player = room
        .players
        .iter()
        .find(|p| p.is_host)
        .or_else(|| room.players.iter().find(|p| p.id == room.host));
    let host_name = host_player.map(|p| p.username.as_str()).unwrap_or("");
    let display_room_name = if room.room_name.trim().is_empty() {
        format!("{}的房间", host_name)
    } else {
        room.room_name.clone()
    };
    json!({
        "id": id,
        "isPublic": room.is_public,
        "playerCount": active_player_count(&room.players),
        "maxPlayers": MAX_ROOM_PLAYERS,
        "players": room.players.iter().filter(|p| !p.disconnected).map(|p| p.username.clone()).collect::<Vec<_>>(),
        "isGameStarted": room.current_game.is_some(),
        "roomName": room.room_name,
        "displayRoomName": display_room_name,
        "hostName": host_name,
        "waitingForAnswer": room.waiting_for_answer,
    })
}

/// GET /room-info/{id}
async fn room_info(State(rs): State<RoomState>, Path(id): Path<String>) -> impl IntoResponse {
    match rs.state.room_snapshot(&id).await {
        Some(room) => Json(public_room_payload(id, &room)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Room not found" })),
        )
            .into_response(),
    }
}

/// GET /clean-rooms — manual trigger (admin)
async fn clean_rooms(
    State(rs): State<RoomState>,
    Query(q): Query<AdminQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_request(&headers, q.token.as_deref()) {
        return admin_forbidden();
    }

    let now = Utc::now().timestamp_millis();
    let stale_ms: i64 = 5 * 60 * 1000;

    let stale_ids: Vec<String> = rs
        .state
        .room_snapshots()
        .await
        .into_iter()
        .filter(|(_, room)| room.current_game.is_none() && (now - room.last_active) > stale_ms)
        .map(|(id, _)| id)
        .collect();

    let count = stale_ids.len();
    for room_id in stale_ids {
        rs.state.remove_room(&room_id).await;
        emit_to_room(
            &rs.io,
            room_id.clone(),
            "roomClosed",
            json!({ "message": "房间因长时间无活动已关闭" }),
        );
    }
    if count > 0 {
        broadcast_lobby_rooms_updated(&rs.io);
    }
    Json(json!({ "message": format!("已清理{}个房间", count), "cleaned": count })).into_response()
}

#[derive(Deserialize)]
struct CloseReason {
    reason: Option<String>,
}

/// GET /close-room/{id}
async fn close_room_get(
    State(rs): State<RoomState>,
    Path(room_id): Path<String>,
    Query(q): Query<AdminQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_admin_request(&headers, q.token.as_deref()) {
        return admin_forbidden();
    }

    let player_count = match rs.state.room_snapshot(&room_id).await {
        Some(r) => active_player_count(&r.players),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "房间不存在" })),
            )
                .into_response();
        }
    };
    let message = "房间被管理关闭，如有疑问请添加首页QQ群".to_string();
    rs.state.remove_room(&room_id).await;
    emit_to_room(
        &rs.io,
        room_id.clone(),
        "roomClosed",
        json!({ "message": message }),
    );
    broadcast_lobby_rooms_updated(&rs.io);
    Json(json!({
        "message": "房间已关闭",
        "roomId": room_id,
        "playerCount": player_count,
        "closeMessage": message,
    }))
    .into_response()
}

/// POST /close-room/{id}  { reason?: string }
async fn close_room_post(
    State(rs): State<RoomState>,
    Path(room_id): Path<String>,
    Query(q): Query<AdminQuery>,
    headers: HeaderMap,
    Json(body): Json<CloseReason>,
) -> impl IntoResponse {
    if !is_admin_request(&headers, q.token.as_deref()) {
        return admin_forbidden();
    }

    let player_count = match rs.state.room_snapshot(&room_id).await {
        Some(r) => active_player_count(&r.players),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "房间不存在" })),
            )
                .into_response();
        }
    };
    let message = match body.reason.as_deref() {
        Some(r) if !r.trim().is_empty() => {
            format!("房间因{}被关闭，如有疑问请添加首页QQ群", r.trim())
        }
        _ => "房间被管理关闭，如有疑问请添加首页QQ群".to_string(),
    };
    rs.state.remove_room(&room_id).await;
    emit_to_room(
        &rs.io,
        room_id.clone(),
        "roomClosed",
        json!({ "message": message }),
    );
    broadcast_lobby_rooms_updated(&rs.io);
    Json(json!({
        "message": "房间已关闭",
        "roomId": room_id,
        "playerCount": player_count,
        "closeMessage": message,
    }))
    .into_response()
}
