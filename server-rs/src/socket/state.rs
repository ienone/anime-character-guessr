use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub is_host: bool,
    #[serde(default)]
    pub score: i32,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub attempt_marks: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_result: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default)]
    pub disconnected: bool,

    // Optional fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<Value>, // Can be number or string in JS
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joined_during_game: Option<bool>,

    // Internal server states mapped to frontend
    #[serde(default, rename = "_tempObserver")]
    pub temp_observer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_completed_round: Option<u32>,
    #[serde(default)]
    pub is_answer_setter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentGame {
    pub character: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    #[serde(default)]
    pub guesses: Vec<Value>,
    #[serde(default)]
    pub team_attempt_marks: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub team_round_results: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Value>,
    #[serde(default = "default_sync_round")]
    pub sync_round: u32,
    #[serde(default)]
    pub sync_players_completed: HashSet<String>,
    #[serde(default)]
    pub sync_winner_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_winner: Option<Value>,
    #[serde(default)]
    pub sync_ready_to_end: bool,
    #[serde(default = "default_rank")]
    pub sync_round_start_rank: u32,
    #[serde(default)]
    pub nonstop_winners: Vec<Value>,
    #[serde(default)]
    pub nonstop_total_players: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_winner: Option<Value>,
    #[serde(default)]
    pub tag_ban_state: Vec<Value>,
    #[serde(default)]
    pub tag_ban_state_pending: Vec<Value>,

    // Internal timing states
    #[serde(skip, default)]
    pub _last_sync_waiting_key: Option<String>,
    #[serde(skip, default)]
    pub _last_sync_waiting_at: i64,
}

fn default_sync_round() -> u32 {
    1
}
fn default_rank() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub host: String,
    #[serde(default = "default_true")]
    pub is_public: bool,
    #[serde(default)]
    pub room_name: String,
    #[serde(default)]
    pub players: Vec<Player>,
    #[serde(default)]
    pub last_active: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_game: Option<CurrentGame>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_setter_id: Option<String>,
    #[serde(default)]
    pub waiting_for_answer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,

    #[serde(skip, default)]
    pub _last_players_broadcast_at: Option<i64>,
    #[serde(skip, default)]
    pub _pending_player_broadcast_extra: Option<Value>,
    #[serde(skip, default)]
    pub _player_broadcast_due_at: Option<i64>,
}

fn default_true() -> bool {
    true
}

use dashmap::DashMap;

#[derive(Debug, Default)]
pub struct ServerState {
    pub rooms: DashMap<String, Room>,
}
