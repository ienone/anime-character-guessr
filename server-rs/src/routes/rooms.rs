use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use socketioxide::SocketIo;
use std::sync::Arc;
use chrono::Utc;

use crate::socket::state::ServerState;

#[derive(Clone)]
pub struct RoomState {
    pub state: Arc<ServerState>,
    pub io: SocketIo,
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

/// GET /quick-join?lang=en
async fn quick_join(
    State(rs): State<RoomState>,
    Query(q): Query<LangQuery>,
) -> impl IntoResponse {
    let public_rooms: Vec<String> = rs.state.rooms
        .iter()
        .filter(|e| e.value().is_public)
        .map(|e| e.key().clone())
        .collect();

    if public_rooms.is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "没有可用的公开房间" }))).into_response();
    }

    use rand::seq::IndexedRandom;
    let room_id: Option<String> = {
        let mut rng = rand::rng();
        public_rooms.choose(&mut rng).cloned()
    }; // rng dropped here

    let room_id = match room_id {
        Some(id) => id,
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "没有可用的公开房间" }))).into_response(),
    };

    let base_url = std::env::var("CLIENT_URL").unwrap_or_else(|_| "http://localhost:5173".to_string());
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
    Json(json!({ "count": rs.state.rooms.len() }))
}

/// GET /list-rooms
async fn list_rooms(State(rs): State<RoomState>) -> impl IntoResponse {
    let rooms: Vec<Value> = rs.state.rooms.iter().map(|entry| {
        let id = entry.key().clone();
        let room = entry.value();
        let host_player = room.players.iter().find(|p| p.is_host)
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
            "playerCount": room.players.len(),
            "players": room.players.iter().map(|p| p.username.clone()).collect::<Vec<_>>(),
            "isGameStarted": room.current_game.is_some(),
            "roomName": room.room_name,
            "displayRoomName": display_room_name,
            "hostName": host_name,
        })
    }).collect();
    Json(rooms)
}

/// GET /room-info/{id}
async fn room_info(
    State(rs): State<RoomState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match rs.state.rooms.get(&id) {
        Some(room) => Json(room.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "Room not found" }))).into_response(),
    }
}

/// GET /clean-rooms — manual trigger (admin)
async fn clean_rooms(State(rs): State<RoomState>) -> impl IntoResponse {
    let now = Utc::now().timestamp_millis();
    let stale_ms: i64 = 5 * 60 * 1000;

    let stale_ids: Vec<String> = rs.state.rooms.iter()
        .filter(|e| e.value().current_game.is_none() && (now - e.value().last_active) > stale_ms)
        .map(|e| e.key().clone())
        .collect();

    let count = stale_ids.len();
    for room_id in stale_ids {
        rs.state.rooms.remove(&room_id);
        let _ = rs.io.to(room_id.clone()).emit("roomClosed", &json!({ "message": "房间因长时间无活动已关闭" }));
    }
    Json(json!({ "message": format!("已清理{}个房间", count), "cleaned": count }))
}

#[derive(Deserialize)]
struct CloseReason {
    reason: Option<String>,
}

/// GET /close-room/{id}
async fn close_room_get(
    State(rs): State<RoomState>,
    Path(room_id): Path<String>,
) -> impl IntoResponse {
    let player_count = match rs.state.rooms.get(&room_id) {
        Some(r) => r.players.len(),
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "房间不存在" }))).into_response(),
    };
    let message = "房间被管理关闭，如有疑问请添加首页QQ群".to_string();
    rs.state.rooms.remove(&room_id);
    let _ = rs.io.to(room_id.clone()).emit("roomClosed", &json!({ "message": message }));
    Json(json!({
        "message": "房间已关闭",
        "roomId": room_id,
        "playerCount": player_count,
        "closeMessage": message,
    })).into_response()
}

/// POST /close-room/{id}  { reason?: string }
async fn close_room_post(
    State(rs): State<RoomState>,
    Path(room_id): Path<String>,
    Json(body): Json<CloseReason>,
) -> impl IntoResponse {
    let player_count = match rs.state.rooms.get(&room_id) {
        Some(r) => r.players.len(),
        None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "房间不存在" }))).into_response(),
    };
    let message = match body.reason.as_deref() {
        Some(r) if !r.trim().is_empty() =>
            format!("房间因{}被关闭，如有疑问请添加首页QQ群", r.trim()),
        _ => "房间被管理关闭，如有疑问请添加首页QQ群".to_string(),
    };
    rs.state.rooms.remove(&room_id);
    let _ = rs.io.to(room_id.clone()).emit("roomClosed", &json!({ "message": message }));
    Json(json!({
        "message": "房间已关闭",
        "roomId": room_id,
        "playerCount": player_count,
        "closeMessage": message,
    })).into_response()
}
