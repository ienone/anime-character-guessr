use crate::routes::game::GameSettings;
use anyhow::{Result, anyhow};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AvatarId {
    Number(i64),
    Text(String),
}

impl AvatarId {
    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(n) => n.as_i64().map(Self::Number),
            Value::String(s) if !s.trim().is_empty() => Some(Self::Text(s.clone())),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Number(n) => *n == 0,
            Self::Text(s) => s.trim().is_empty(),
        }
    }

    pub fn as_key_string(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Text(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stable_player_id: String,
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
    pub avatar_id: Option<AvatarId>,
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
pub struct CharacterPayload {
    pub id: i64,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl CharacterPayload {
    pub fn from_value(value: Value) -> Result<Self> {
        let Value::Object(mut fields) = value else {
            return Err(anyhow!("character payload must be an object"));
        };
        let id = fields
            .remove("id")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow!("character payload is missing numeric id"))?;
        Ok(Self { id, fields })
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        if key == "id" {
            return None;
        }
        self.fields.get(key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuessEntry {
    pub player_id: String,
    pub player_name: String,
    pub is_correct: bool,
    pub is_partial_correct: bool,
    #[serde(rename = "guessData")]
    pub guess_data: CharacterPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerGuessHistory {
    pub username: String,
    #[serde(default)]
    pub guesses: Vec<GuessEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WinnerMarker {
    pub id: String,
    pub username: String,
    #[serde(rename = "isBigWin")]
    pub is_big_win: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonstopWinnerBonuses {
    #[serde(default, rename = "bigWin")]
    pub big_win: i32,
    #[serde(default, rename = "quickGuess")]
    pub quick_guess: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NonstopWinner {
    pub id: String,
    pub username: String,
    #[serde(rename = "isBigWin")]
    pub is_big_win: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub score: i32,
    #[serde(default)]
    pub bonuses: NonstopWinnerBonuses,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagBanEntry {
    pub tag: String,
    #[serde(default)]
    pub revealer: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentGame {
    pub character: CharacterPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<GameSettings>,
    #[serde(default)]
    pub guesses: Vec<PlayerGuessHistory>,
    #[serde(default)]
    pub team_attempt_marks: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub team_round_results: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<String>>,
    #[serde(default = "default_sync_round")]
    pub sync_round: u32,
    #[serde(default)]
    pub sync_players_completed: HashSet<String>,
    #[serde(default)]
    pub sync_winner_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_winner: Option<WinnerMarker>,
    #[serde(default)]
    pub sync_ready_to_end: bool,
    #[serde(default = "default_rank")]
    pub sync_round_start_rank: u32,
    #[serde(default)]
    pub nonstop_winners: Vec<NonstopWinner>,
    #[serde(default)]
    pub nonstop_total_players: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_winner: Option<WinnerMarker>,
    #[serde(default)]
    pub tag_ban_state: Vec<TagBanEntry>,
    #[serde(default)]
    pub tag_ban_state_pending: Vec<TagBanEntry>,

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
    pub settings: Option<GameSettings>,

    #[serde(skip, default)]
    pub _last_players_broadcast_at: Option<i64>,
    #[serde(skip, default)]
    pub _pending_player_broadcast_extra: Option<Value>,
    #[serde(skip, default)]
    pub _player_broadcast_due_at: Option<i64>,
    #[serde(skip, default)]
    pub _player_broadcast_flush_scheduled: bool,
}

fn default_true() -> bool {
    true
}

use dashmap::DashMap;

const ROOM_COMMAND_CAPACITY: usize = 256;

fn canonical_username(username: &str) -> String {
    username.trim().to_lowercase()
}

pub fn new_player_stable_id(room_id: &str, username: &str) -> String {
    new_player_stable_id_for_session(room_id, username, None)
}

pub fn new_player_stable_id_for_session(
    room_id: &str,
    username: &str,
    player_session_id: Option<&str>,
) -> String {
    if let Some(session_id) = canonical_player_session_id(player_session_id) {
        return format!("{}::session::{}", room_id, session_id);
    }

    format!(
        "{}::legacy::{}::{}",
        room_id,
        canonical_username(username),
        Utc::now().timestamp_millis()
    )
}

fn canonical_player_session_id(player_session_id: Option<&str>) -> Option<String> {
    let session_id = player_session_id?.trim();
    if session_id.len() < 8 || session_id.len() > 128 {
        return None;
    }

    let normalized: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();

    if normalized.len() == session_id.len() {
        Some(normalized)
    } else {
        None
    }
}

pub fn legacy_player_key(room_id: &str, username: &str) -> String {
    format!("{}::legacy-name::{}", room_id, canonical_username(username))
}

pub fn ensure_player_stable_id(room_id: &str, player: &mut Player) -> String {
    if player.stable_player_id.trim().is_empty() {
        player.stable_player_id = new_player_stable_id(room_id, &player.username);
    }
    player.stable_player_id.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_backed_stable_id_ignores_username() {
        let alice = new_player_stable_id_for_session("room-1", "Alice", Some("session_1234"));
        let bob = new_player_stable_id_for_session("room-1", "Bob", Some("session_1234"));

        assert_eq!(alice, "room-1::session::session_1234");
        assert_eq!(alice, bob);
    }

    #[test]
    fn invalid_session_id_uses_legacy_username_fallback() {
        let id = new_player_stable_id_for_session("room-1", " Alice ", Some("bad session"));

        assert!(id.starts_with("room-1::legacy::alice::"));
    }
}

#[derive(Debug)]
pub struct RoomActor {
    room: Arc<Mutex<Room>>,
    command_slots: Arc<Semaphore>,
    queued_commands: AtomicUsize,
    processed_commands: AtomicU64,
    rejected_commands: AtomicU64,
    total_command_micros: AtomicU64,
    max_command_micros: AtomicU64,
}

impl RoomActor {
    pub fn new(room: Room) -> Self {
        Self {
            room: Arc::new(Mutex::new(room)),
            command_slots: Arc::new(Semaphore::new(ROOM_COMMAND_CAPACITY)),
            queued_commands: AtomicUsize::new(0),
            processed_commands: AtomicU64::new(0),
            rejected_commands: AtomicU64::new(0),
            total_command_micros: AtomicU64::new(0),
            max_command_micros: AtomicU64::new(0),
        }
    }

    pub async fn snapshot(&self) -> Room {
        self.room.lock().await.clone()
    }

    pub async fn snapshot_after_removal(&self) -> Room {
        self.room.lock().await.clone()
    }

    fn begin_command(self: &Arc<Self>) -> Option<OwnedSemaphorePermit> {
        let permit = match self.command_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.rejected_commands.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        self.queued_commands.fetch_add(1, Ordering::Relaxed);
        Some(permit)
    }

    pub fn finish_command(&self, elapsed_micros: u64) {
        self.queued_commands.fetch_sub(1, Ordering::Relaxed);
        self.processed_commands.fetch_add(1, Ordering::Relaxed);
        self.total_command_micros
            .fetch_add(elapsed_micros, Ordering::Relaxed);

        let mut current_max = self.max_command_micros.load(Ordering::Relaxed);
        while elapsed_micros > current_max {
            match self.max_command_micros.compare_exchange_weak(
                current_max,
                elapsed_micros,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current_max = observed,
            }
        }
    }

    pub fn command_stats(&self) -> RoomCommandStats {
        RoomCommandStats {
            queued: self.queued_commands.load(Ordering::Relaxed),
            processed: self.processed_commands.load(Ordering::Relaxed),
            rejected: self.rejected_commands.load(Ordering::Relaxed),
            total_micros: self.total_command_micros.load(Ordering::Relaxed),
            max_micros: self.max_command_micros.load(Ordering::Relaxed),
        }
    }

    async fn lock_room(self: &Arc<Self>) -> OwnedMutexGuard<Room> {
        self.room.clone().lock_owned().await
    }
}

struct RoomCommandGuard<'a> {
    actor: Arc<RoomActor>,
    _permit: OwnedSemaphorePermit,
    inner: OwnedMutexGuard<Room>,
    started_at: Instant,
    _marker: std::marker::PhantomData<&'a ()>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RoomCommandStats {
    pub queued: usize,
    pub processed: u64,
    pub rejected: u64,
    pub total_micros: u64,
    pub max_micros: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum RoomCommand {
    SocketEvent(&'static str),
}

impl RoomCommand {
    pub fn label(self) -> &'static str {
        match self {
            RoomCommand::SocketEvent(label) => label,
        }
    }
}

impl Drop for RoomCommandGuard<'_> {
    fn drop(&mut self) {
        let elapsed_micros = self
            .started_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.actor.finish_command(elapsed_micros);
    }
}

impl Deref for RoomCommandGuard<'_> {
    type Target = Room;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for RoomCommandGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[derive(Debug, Default)]
pub struct ServerState {
    pub rooms: DashMap<String, Arc<RoomActor>>,
    pub socket_rooms: DashMap<String, String>,
    pub player_sockets: DashMap<String, String>,
    pub socket_player_keys: DashMap<String, String>,
}

impl ServerState {
    pub fn insert_room(&self, room_id: String, room: Room) {
        self.rooms.insert(room_id, Arc::new(RoomActor::new(room)));
    }

    pub fn contains_room(&self, room_id: &str) -> bool {
        self.rooms.contains_key(room_id)
    }

    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    pub fn room_command_stats_total(&self) -> RoomCommandStats {
        self.rooms
            .iter()
            .map(|entry| entry.value().command_stats())
            .fold(RoomCommandStats::default(), |mut total, stats| {
                total.queued += stats.queued;
                total.processed += stats.processed;
                total.rejected += stats.rejected;
                total.total_micros += stats.total_micros;
                total.max_micros = total.max_micros.max(stats.max_micros);
                total
            })
    }

    pub async fn room_snapshot(&self, room_id: &str) -> Option<Room> {
        let actor = self.rooms.get(room_id)?.value().clone();
        Some(actor.snapshot().await)
    }

    pub async fn room_snapshots(&self) -> Vec<(String, Room)> {
        let actors: Vec<(String, Arc<RoomActor>)> = self
            .rooms
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        let mut snapshots = Vec::with_capacity(actors.len());
        for (room_id, actor) in actors {
            snapshots.push((room_id, actor.snapshot().await));
        }
        snapshots
    }

    async fn lock_room_command(
        &self,
        room_id: &str,
        command: RoomCommand,
    ) -> Option<RoomCommandGuard<'_>> {
        let actor = self.rooms.get(room_id)?.value().clone();
        let permit = actor.begin_command()?;
        let _label = command.label();
        let started_at = Instant::now();
        let inner = actor.lock_room().await;
        Some(RoomCommandGuard {
            actor,
            _permit: permit,
            inner,
            started_at,
            _marker: std::marker::PhantomData,
        })
    }

    pub async fn run_room_command<T, F>(
        &self,
        room_id: &str,
        command: RoomCommand,
        f: F,
    ) -> Option<T>
    where
        F: FnOnce(&mut Room) -> T,
    {
        let mut room = self.lock_room_command(room_id, command).await?;
        Some(f(&mut room))
    }

    pub async fn remove_room(&self, room_id: &str) -> Option<Room> {
        let (_, actor) = self.rooms.remove(room_id)?;
        let room = actor.snapshot_after_removal().await;
        for player in &room.players {
            self.socket_rooms.remove(&player.id);
            self.socket_player_keys.remove(&player.id);
            if !player.stable_player_id.trim().is_empty() {
                self.player_sockets.remove(&player.stable_player_id);
            }
            self.player_sockets
                .remove(&legacy_player_key(room_id, &player.username));
        }
        Some(room)
    }
}
