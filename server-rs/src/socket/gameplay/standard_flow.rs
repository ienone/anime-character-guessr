use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use socketioxide::SocketIo;

use crate::routes::game::GameSettings;
use crate::socket::state::{
    CharacterPayload, CurrentGame, NonstopWinner, Player, PlayerGuessHistory, Room, TagBanEntry,
};
use crate::socket::{broadcast_lobby_rooms_updated, emit_to_room};

use super::{
    RESULT_BIG_WIN, RESULT_DEAD, RESULT_SURRENDER, RESULT_TEAM_WIN, ScoreBonuses, ScoreResult,
    calculate_nonstop_setter_score, calculate_setter_score, calculate_winner_score,
    player_has_result, player_is_big_winner, player_is_winner, player_result,
};

const SYNC_WAITING_MIN_INTERVAL_MS: i64 = 150;

fn setter_score_reason(
    winner_guess_count: i32,
    has_big_winner: bool,
    big_winner_score: i32,
    total_rounds: i32,
) -> &'static str {
    if has_big_winner {
        return "纯在送分";
    }
    if winner_guess_count > 0 {
        if winner_guess_count <= 3 {
            return "太简单了";
        }
        if (winner_guess_count as f32) > (total_rounds as f32 / 2.0) {
            return "难度适中";
        }
        return "";
    }
    let _ = big_winner_score;
    "没人猜中"
}

fn nonstop_setter_score_reason(
    has_big_winner: bool,
    winners_count: i32,
    total_players_count: i32,
) -> &'static str {
    if has_big_winner {
        return "纯在送分";
    }
    if winners_count == 0 {
        return "无人猜中";
    }
    let total_players = std::cmp::max(1, total_players_count) as f32;
    let win_rate = winners_count as f32 / total_players;
    if win_rate <= 0.25 {
        return "难度偏高";
    }
    if win_rate >= 0.75 {
        return "难度偏低";
    }
    "难度适中"
}

pub fn build_guess_history_payload(game: &crate::socket::state::CurrentGame) -> Value {
    json!({
        "guesses": game.guesses,
    })
}

fn score_result_label(player: &Player) -> &'static str {
    match player_result(player) {
        Some(RESULT_BIG_WIN) => "bigwin",
        Some(super::marks::RESULT_WIN) => "win",
        Some(RESULT_TEAM_WIN) => "teamwin",
        Some(RESULT_DEAD) => "lose",
        Some(RESULT_SURRENDER) => "surrender",
        _ => "",
    }
}

fn build_sync_waiting_key(round: u32, sync_status: &[Value]) -> String {
    let mut normalized: Vec<(String, bool)> = sync_status
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_str()?.to_string();
            let completed = s
                .get("completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some((id, completed))
        })
        .collect();
    normalized.sort_by(|a, b| a.0.cmp(&b.0));
    let tail = normalized
        .into_iter()
        .map(|(id, completed)| format!("{}:{}", id, if completed { 1 } else { 0 }))
        .collect::<Vec<_>>()
        .join("|");
    format!("r:{}|{}", round, tail)
}

fn should_skip_sync_waiting(game: &mut CurrentGame, payload: &Value, force: bool) -> bool {
    let round = payload.get("round").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let sync_status: Vec<Value> = payload
        .get("syncStatus")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let key = build_sync_waiting_key(round, &sync_status);
    let now = Utc::now().timestamp_millis();

    let last_key = game._last_sync_waiting_key.clone();
    let last_at = game._last_sync_waiting_at;

    if !force {
        if let Some(k) = last_key {
            if k == key && now - last_at < SYNC_WAITING_MIN_INTERVAL_MS {
                return true;
            }
        }
    }

    game._last_sync_waiting_key = Some(key);
    game._last_sync_waiting_at = now;
    false
}

pub fn emit_sync_and_nonstop_state(
    room: &mut Room,
    room_id: &str,
    io: &SocketIo,
    force_sync_waiting: bool,
) {
    let Some(game) = room.current_game.as_mut() else {
        return;
    };

    let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);
    let nonstop_mode = game.settings.as_ref().is_some_and(|s| s.nonstop_mode);

    if sync_mode {
        let sync_players: Vec<&Player> = room
            .players
            .iter()
            .filter(|p| {
                !p.is_answer_setter
                    && p.team.as_deref() != Some("0")
                    && !p.disconnected
                    && !player_has_result(p)
            })
            .collect();

        let sync_status: Vec<Value> = sync_players
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "username": p.username,
                    "completed": game.sync_players_completed.contains(&p.id)
                })
            })
            .collect();

        let payload = json!({
            "round": game.sync_round,
            "syncStatus": sync_status,
            "completedCount": sync_status.iter().filter(|s| s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
            "totalCount": sync_status.len(),
        });

        if !should_skip_sync_waiting(game, &payload, force_sync_waiting) {
            emit_to_room(io, room_id.to_string(), "syncWaiting", payload);
        }

        // Frontend currently does not register a `syncGameEnding` handler.
        // Keep `syncWaiting` as the sole signal.
    }

    if nonstop_mode {
        let active_players: Vec<&Player> = room
            .players
            .iter()
            .filter(|p| !p.is_answer_setter && p.team.as_deref() != Some("0") && !p.disconnected)
            .collect();

        let remaining_players: Vec<&Player> = active_players
            .iter()
            .copied()
            .filter(|p| !player_has_result(p))
            .collect();

        let winners = game
            .nonstop_winners
            .iter()
            .enumerate()
            .map(|(idx, w)| {
                json!({
                    "username": w.username,
                    "rank": (idx + 1) as i32,
                    "score": w.score,
                })
            })
            .collect::<Vec<_>>();

        emit_to_room(
            io,
            room_id.to_string(),
            "nonstopProgress",
            json!({
                "winners": winners,
                "remainingCount": remaining_players.len(),
                "totalCount": active_players.len(),
            }),
        );
    }
}

pub fn apply_setter_observers(room: &mut Room, room_id: &str, setter_id: &str, io: &SocketIo) {
    let Some(team_id) = room
        .players
        .iter()
        .find(|p| p.id == setter_id)
        .and_then(|p| p.team.clone())
    else {
        return;
    };

    if team_id == "0" {
        return;
    }

    for p in &mut room.players {
        if p.team.as_deref() == Some(team_id.as_str())
            && p.id != setter_id
            && !p.is_answer_setter
            && !p.disconnected
        {
            p.temp_observer = true;
        }
    }

    emit_to_room(
        io,
        room_id.to_string(),
        "updatePlayers",
        json!({
            "players": room.players,
            "answerSetterId": room.answer_setter_id,
        }),
    );
}

pub fn revert_setter_observers(room: &mut Room, room_id: &str, io: &SocketIo) {
    let mut changed = false;
    for p in &mut room.players {
        if p.temp_observer {
            p.temp_observer = false;
            changed = true;
        }
    }

    if changed {
        emit_to_room(
            io,
            room_id.to_string(),
            "updatePlayers",
            json!({ "players": room.players }),
        );
    }
}

pub fn mark_team_victory(room: &mut Room, room_id: &str, winner_id: &str, io: &SocketIo) {
    let Some(game) = room.current_game.as_mut() else {
        return;
    };

    let Some(winner) = room.players.iter().find(|p| p.id == winner_id).cloned() else {
        return;
    };

    let Some(team_id) = winner.team.clone() else {
        return;
    };

    if team_id == "0" {
        return;
    }

    super::marks::set_team_result(game, &team_id, RESULT_TEAM_WIN);

    let team_members: Vec<String> = room
        .players
        .iter()
        .filter(|p| {
            p.team.as_deref() == Some(&team_id)
                && p.id != winner_id
                && !p.is_answer_setter
                && !p.disconnected
        })
        .map(|p| p.id.clone())
        .collect();

    for teammate_id in &team_members {
        if let Some(teammate) = room.players.iter_mut().find(|p| p.id == *teammate_id) {
            teammate.round_result = Some(RESULT_TEAM_WIN.to_string());
            teammate.temp_observer = true;
            game.sync_players_completed.remove(&teammate.id);
            emit_to_room(
                io,
                teammate.id.clone(),
                "teamWin",
                json!({
                    "winnerName": winner.username,
                    "message": format!("队友 {} 已猜对！", winner.username),
                }),
            );
        }
    }

    let nonstop_mode = game.settings.as_ref().is_some_and(|s| s.nonstop_mode);
    let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);

    if !nonstop_mode && sync_mode {
        if let Some(winner_mut) = room.players.iter_mut().find(|p| p.id == winner_id) {
            if winner_mut.team.as_deref() != Some("0") {
                winner_mut.temp_observer = true;
            }
        }
    }

    emit_to_room(
        io,
        room_id.to_string(),
        "updatePlayers",
        json!({ "players": room.players }),
    );
}

pub fn init_game_state(
    room: &mut Room,
    character: CharacterPayload,
    settings: Option<GameSettings>,
    hints: Option<Vec<String>>,
    answer_setter_id: Option<&str>,
) {
    let initial_active_players = room
        .players
        .iter()
        .filter(|p| {
            if p.disconnected {
                return false;
            }
            if p.team.as_deref() == Some("0") {
                return false;
            }
            if p.temp_observer {
                return false;
            }
            if answer_setter_id.is_some() && answer_setter_id == Some(p.id.as_str()) {
                return false;
            }
            true
        })
        .count();

    room.current_game = Some(crate::socket::state::CurrentGame {
        character,
        settings,
        guesses: vec![],
        team_attempt_marks: HashMap::new(),
        team_round_results: HashMap::new(),
        hints,
        sync_round: 1,
        sync_players_completed: HashSet::new(),
        sync_winner_found: false,
        sync_winner: None,
        sync_ready_to_end: false,
        sync_round_start_rank: 1,
        nonstop_winners: vec![],
        first_winner: None,
        tag_ban_state: vec![],
        tag_ban_state_pending: vec![],
        nonstop_total_players: initial_active_players as i32,
        _last_sync_waiting_key: None,
        _last_sync_waiting_at: 0,
    });

    let game = room.current_game.as_mut().expect("just set");

    for p in &mut room.players {
        p.attempt_marks.clear();
        p.round_result = None;
        if p.temp_observer {
            p.temp_observer = false;
        }
        p.sync_completed_round = None;
        p.is_answer_setter = answer_setter_id == Some(p.id.as_str());
        if !p.is_answer_setter && p.team.as_deref() != Some("0") {
            game.guesses.push(PlayerGuessHistory {
                username: p.username.clone(),
                guesses: Vec::new(),
            });
        }
    }

    for p in &room.players {
        if let Some(team) = p.team.as_deref() {
            if team != "0" {
                game.team_attempt_marks.entry(team.to_string()).or_default();
            }
        }
    }

    room.last_active = Utc::now().timestamp_millis();
}

pub fn update_sync_progress(room: &mut Room, room_id: &str, io: &SocketIo) {
    let Some(game) = room.current_game.as_mut() else {
        return;
    };

    let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);

    if !sync_mode {
        return;
    }

    let is_ended = |p: &Player| player_has_result(p);

    let sync_players: Vec<Player> = room
        .players
        .iter()
        .filter(|p| {
            !p.is_answer_setter && p.team.as_deref() != Some("0") && !p.disconnected && !is_ended(p)
        })
        .cloned()
        .collect();

    if sync_players.is_empty() {
        return;
    }

    for p in &sync_players {
        if p.sync_completed_round == Some(game.sync_round) {
            game.sync_players_completed.insert(p.id.clone());
        }
    }

    let sync_status: Vec<Value> = sync_players
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "username": p.username,
                "completed": game.sync_players_completed.contains(&p.id)
            })
        })
        .collect();

    let all_completed = sync_status.iter().all(|s| {
        s.get("completed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    });

    if all_completed {
        // Merge tagBanStatePending
        let mut pending_ban_broadcast: Option<Vec<TagBanEntry>> = None;

        if game.settings.as_ref().is_some_and(|s| s.tag_ban)
            && !game.tag_ban_state_pending.is_empty()
        {
            let mut existing_tags: HashSet<String> = game
                .tag_ban_state
                .iter()
                .map(|item| item.tag.clone())
                .collect();

            let mut pending_new_entries: Vec<TagBanEntry> = Vec::new();
            for entry in game.tag_ban_state_pending.iter() {
                let tag_name = entry.tag.trim();
                if tag_name.is_empty() || existing_tags.contains(tag_name) {
                    continue;
                }
                existing_tags.insert(tag_name.to_string());
                let mut uniq: HashSet<String> = HashSet::new();
                let revealer_vec = entry
                    .revealer
                    .iter()
                    .cloned()
                    .filter(|s| uniq.insert(s.clone()))
                    .collect::<Vec<_>>();

                pending_new_entries.push(TagBanEntry {
                    tag: tag_name.to_string(),
                    revealer: revealer_vec,
                });
            }

            if !pending_new_entries.is_empty() {
                let mut updated_state = game.tag_ban_state.clone();
                updated_state.extend(pending_new_entries);
                game.tag_ban_state = updated_state.clone();
                pending_ban_broadcast = Some(updated_state);
            }

            game.tag_ban_state_pending.clear();
        }

        if let Some(state) = pending_ban_broadcast.take() {
            emit_to_room(
                io,
                room_id.to_string(),
                "tagBanStateUpdate",
                json!({
                    "tagBanState": state,
                }),
            );
        }

        let nonstop_mode = game.settings.as_ref().is_some_and(|s| s.nonstop_mode);

        if !nonstop_mode && game.sync_winner_found {
            game.sync_ready_to_end = true;
            let payload = json!({
                "round": game.sync_round,
                "syncStatus": sync_status,
                "completedCount": sync_status.len(),
                "totalCount": sync_status.len(),
            });
            if !should_skip_sync_waiting(game, &payload, false) {
                emit_to_room(io, room_id.to_string(), "syncWaiting", payload);
            }

            // Frontend currently has no `syncGameEnding` handler; keep `syncWaiting` as the sole signal.

            // finalize_standard_game needs &mut room; defer until we release the `game` borrow.
            // We'll return early after finalization.
            let _ = game;
            let _ = finalize_standard_game(room, room_id, io, true);
            return;
        }

        game.sync_ready_to_end = false;
        game.sync_round += 1;
        game.sync_players_completed.clear();
        game._last_sync_waiting_key = None;
        game._last_sync_waiting_at = 0;
        for p in &mut room.players {
            p.sync_completed_round = None;
        }

        if nonstop_mode {
            game.sync_round_start_rank = (game.nonstop_winners.len() + 1) as u32;
        }

        // Next round
        let next_sync_players: Vec<&Player> = room
            .players
            .iter()
            .filter(|p| {
                !p.is_answer_setter
                    && p.team.as_deref() != Some("0")
                    && !p.disconnected
                    && !player_has_result(p)
            })
            .collect();

        let next_sync_status: Vec<Value> = next_sync_players
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "username": p.username,
                    "completed": game.sync_players_completed.contains(&p.id)
                })
            })
            .collect();

        emit_to_room(
            io,
            room_id.to_string(),
            "syncRoundStart",
            json!({
                "round": game.sync_round,
            }),
        );

        let next_payload = json!({
            "round": game.sync_round,
            "syncStatus": next_sync_status,
            "completedCount": next_sync_status.iter().filter(|s| s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
            "totalCount": next_sync_status.len(),
        });
        if !should_skip_sync_waiting(game, &next_payload, true) {
            emit_to_room(io, room_id.to_string(), "syncWaiting", next_payload);
        }
    } else {
        let payload = json!({
            "round": game.sync_round,
            "syncStatus": sync_status,
            "completedCount": sync_status.iter().filter(|s| s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
            "totalCount": sync_status.len(),
        });

        if !should_skip_sync_waiting(game, &payload, false) {
            emit_to_room(io, room_id.to_string(), "syncWaiting", payload);
        }

        let nonstop_mode = game.settings.as_ref().is_some_and(|s| s.nonstop_mode);

        if !nonstop_mode && game.sync_winner_found {
            // Frontend currently has no `syncGameEnding` handler; keep `syncWaiting` as the sole signal.
        }
    }
}

pub fn compute_partial_awardees_from_guess_history(room: &Room) -> HashSet<String> {
    let mut awardees = HashSet::new();
    let Some(game) = room.current_game.as_ref() else {
        return awardees;
    };

    let guesses = &game.guesses;
    if guesses.is_empty() {
        return awardees;
    }

    let players_by_id: HashMap<String, &Player> =
        room.players.iter().map(|p| (p.id.clone(), p)).collect();
    let mut first_partial_index_by_player: HashMap<String, usize> = HashMap::new();

    for player_guesses in guesses {
        for (idx, guess) in player_guesses.guesses.iter().enumerate() {
            if guess.is_partial_correct && !guess.is_correct {
                first_partial_index_by_player
                    .entry(guess.player_id.clone())
                    .or_insert(idx);
            }
        }
    }

    #[derive(Clone)]
    struct Best {
        player_id: String,
        idx: usize,
        username: String,
    }

    let mut best_by_group: HashMap<String, Best> = HashMap::new();

    for (player_id, idx) in first_partial_index_by_player {
        let Some(p) = players_by_id.get(&player_id) else {
            continue;
        };
        if p.is_answer_setter {
            continue;
        }
        if p.team.as_deref() == Some("0") {
            continue;
        }

        let group_key = if let Some(team) = p.team.as_deref() {
            if !team.is_empty() && team != "0" {
                format!("team:{}", team)
            } else {
                format!("solo:{}", player_id)
            }
        } else {
            format!("solo:{}", player_id)
        };

        let username = p.username.clone();
        let candidate = Best {
            player_id: player_id.clone(),
            idx,
            username: username.clone(),
        };

        match best_by_group.get(&group_key) {
            None => {
                best_by_group.insert(group_key, candidate);
            }
            Some(current) => {
                if idx < current.idx || (idx == current.idx && username < current.username) {
                    best_by_group.insert(group_key, candidate);
                }
            }
        }
    }

    for best in best_by_group.values() {
        awardees.insert(best.player_id.clone());
    }

    awardees
}

fn compute_partial_awardees_from_guess_history_guesses(
    guesses: &[PlayerGuessHistory],
) -> HashSet<String> {
    let mut first_partial_index_by_player: HashMap<String, usize> = HashMap::new();

    for player_guesses in guesses {
        for (idx, guess) in player_guesses.guesses.iter().enumerate() {
            if guess.is_partial_correct && !guess.is_correct {
                first_partial_index_by_player
                    .entry(guess.player_id.clone())
                    .or_insert(idx);
            }
        }
    }

    let mut awardees = HashSet::new();
    if first_partial_index_by_player.is_empty() {
        return awardees;
    }

    // Without the full Room context, fall back to awarding all earliest partial-correct guessers.
    // (The main caller only uses this for +1 partial bonus and already filters observers/setters upstream.)
    let best_idx = first_partial_index_by_player
        .values()
        .copied()
        .min()
        .unwrap_or(usize::MAX);
    for (pid, idx) in first_partial_index_by_player {
        if idx == best_idx {
            awardees.insert(pid);
        }
    }

    awardees
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoreBreakdown {
    #[serde(skip_serializing_if = "Option::is_none")]
    rank: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base: Option<i32>,
    #[serde(rename = "bigWin", skip_serializing_if = "Option::is_none")]
    big_win: Option<i32>,
    #[serde(rename = "quickGuess", skip_serializing_if = "Option::is_none")]
    quick_guess: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partial: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScoreChange {
    score: i32,
    breakdown: ScoreBreakdown,
    result: String,
}

impl Default for ScoreChange {
    fn default() -> Self {
        Self {
            score: 0,
            breakdown: ScoreBreakdown::default(),
            result: String::new(),
        }
    }
}

fn score_change(score: i32, breakdown: ScoreBreakdown, result: impl Into<String>) -> ScoreChange {
    ScoreChange {
        score,
        breakdown,
        result: result.into(),
    }
}

fn generate_score_details(
    players: &[Player],
    score_changes: &HashMap<String, ScoreChange>,
    setter_info: Option<Value>,
) -> Vec<Value> {
    let active_players: Vec<&Player> = players
        .iter()
        .filter(|p| p.team.as_deref() != Some("0"))
        .collect();

    let mut team_map: HashMap<String, Vec<Value>> = HashMap::new();
    let mut no_team_players: Vec<Value> = Vec::new();

    for p in active_players {
        if p.is_answer_setter {
            continue;
        }

        let change = score_changes.get(&p.id).cloned().unwrap_or_default();

        let player_info = json!({
            "id": p.id,
            "username": p.username,
            "team": p.team,
            "score": change.score,
            "breakdown": change.breakdown,
            "result": change.result,
        });

        if let Some(team) = p.team.as_deref() {
            if !team.is_empty() && team != "0" {
                team_map
                    .entry(team.to_string())
                    .or_default()
                    .push(player_info);
            } else {
                no_team_players.push(player_info);
            }
        } else {
            no_team_players.push(player_info);
        }
    }

    let mut details: Vec<Value> = Vec::new();

    for (team_id, members) in team_map.into_iter() {
        if members.len() > 1 {
            let team_score = members
                .iter()
                .filter_map(|m| m.get("score").and_then(|v| v.as_i64()))
                .sum::<i64>();
            details.push(json!({
                "type": "team",
                "teamId": team_id,
                "teamScore": team_score,
                "members": members,
            }));
        } else if let Some(first) = members.into_iter().next() {
            no_team_players.push(first);
        }
    }

    for p in no_team_players {
        let mut obj = p.as_object().cloned().unwrap_or_default();
        obj.insert("type".to_string(), Value::String("player".to_string()));
        details.push(Value::Object(obj));
    }

    if let Some(setter) = setter_info {
        details.push(setter);
    }

    details
}

fn build_score_changes_standard(
    players: &[Player],
    actual_winners: &[Player],
    winner_score_results: &HashMap<String, ScoreResult>,
    partial_awardees: &HashSet<String>,
) -> HashMap<String, ScoreChange> {
    let mut score_changes: HashMap<String, ScoreChange> = HashMap::new();
    let active_players: Vec<&Player> = players
        .iter()
        .filter(|p| !p.is_answer_setter && (p.team.as_deref() != Some("0") || p.temp_observer))
        .collect();

    let winner_id_set: HashSet<String> = actual_winners.iter().map(|w| w.id.clone()).collect();

    for p in active_players {
        if winner_id_set.contains(&p.id) {
            let res = winner_score_results.get(&p.id);
            let default_bonuses = ScoreBonuses::default();
            let bonuses = res.map(|r| &r.bonuses).unwrap_or(&default_bonuses);
            score_changes.insert(
                p.id.clone(),
                score_change(
                    res.map(|r| r.total_score).unwrap_or(0),
                    ScoreBreakdown {
                        base: Some(2),
                        big_win: Some(bonuses.big_win),
                        quick_guess: Some(bonuses.quick_guess),
                        ..ScoreBreakdown::default()
                    },
                    score_result_label(p),
                ),
            );
        } else {
            let has_partial = partial_awardees.contains(&p.id);
            score_changes.insert(
                p.id.clone(),
                score_change(
                    if has_partial { 1 } else { 0 },
                    ScoreBreakdown {
                        partial: has_partial.then_some(1),
                        ..ScoreBreakdown::default()
                    },
                    score_result_label(p),
                ),
            );
        }
    }

    score_changes
}

fn build_score_changes_nonstop(
    players: &[Player],
    nonstop_winners: &[NonstopWinner],
    partial_awardees: &HashSet<String>,
) -> HashMap<String, ScoreChange> {
    let mut score_changes: HashMap<String, ScoreChange> = HashMap::new();

    let winner_ids: HashSet<String> = nonstop_winners.iter().map(|w| w.id.clone()).collect();

    for (idx, w) in nonstop_winners.iter().enumerate() {
        let winner_player = players.iter().find(|p| p.id == w.id);
        let is_big_win = winner_player.map(player_is_big_winner).unwrap_or(false);

        let big_win_bonus = if w.bonuses.big_win == 0 && is_big_win {
            12
        } else {
            w.bonuses.big_win
        };
        let quick_guess_bonus = w.bonuses.quick_guess;
        let score = w.score;
        let base_score = (score - big_win_bonus - quick_guess_bonus).max(0);

        score_changes.insert(
            w.id.clone(),
            score_change(
                score,
                ScoreBreakdown {
                    rank: Some((idx + 1) as i32),
                    base: Some(base_score),
                    big_win: (big_win_bonus != 0).then_some(big_win_bonus),
                    quick_guess: (quick_guess_bonus != 0).then_some(quick_guess_bonus),
                    ..ScoreBreakdown::default()
                },
                if is_big_win { "bigwin" } else { "win" },
            ),
        );
    }

    let active_players: Vec<&Player> = players
        .iter()
        .filter(|p| !p.is_answer_setter && (p.team.as_deref() != Some("0") || p.temp_observer))
        .collect();

    for p in active_players {
        if winner_ids.contains(&p.id) {
            continue;
        }
        let has_partial = partial_awardees.contains(&p.id);
        score_changes.insert(
            p.id.clone(),
            score_change(
                if has_partial { 1 } else { 0 },
                ScoreBreakdown {
                    partial: has_partial.then_some(1),
                    ..ScoreBreakdown::default()
                },
                score_result_label(p),
            ),
        );
    }

    score_changes
}

pub fn finalize_nonstop_game(room: &mut Room, room_id: &str, io: &SocketIo) -> bool {
    let nonstop_mode = room
        .current_game
        .as_ref()
        .and_then(|g| g.settings.as_ref())
        .is_some_and(|s| s.nonstop_mode);

    if !nonstop_mode {
        return false;
    }

    let active_players: Vec<&Player> = room
        .players
        .iter()
        .filter(|p| !p.is_answer_setter && p.team.as_deref() != Some("0") && !p.disconnected)
        .collect();

    let remaining_players: Vec<&Player> = active_players
        .iter()
        .copied()
        .filter(|p| !player_has_result(p))
        .collect();

    if !remaining_players.is_empty() {
        return false;
    }

    let partial_awardees = compute_partial_awardees_from_guess_history(room);

    let Some(game) = room.current_game.as_mut() else {
        return false;
    };

    let answer_setter_idx = room.players.iter().position(|p| p.is_answer_setter);

    let winners_count = game.nonstop_winners.len() as i32;
    let total_players_count = active_players.len() as i32;

    let winner_ids: HashSet<String> = game.nonstop_winners.iter().map(|w| w.id.clone()).collect();

    for p in &mut room.players {
        if p.is_answer_setter {
            continue;
        }
        if p.team.as_deref() == Some("0") {
            continue;
        }
        if winner_ids.contains(&p.id) {
            continue;
        }
        if partial_awardees.contains(&p.id) {
            p.score += 1;
        }
    }

    let mut has_big_winner = false;
    let mut big_winner_score = 0;
    if let Some(big) = game.nonstop_winners.iter().find(|w| {
        room.players
            .iter()
            .find(|p| p.id == w.id)
            .map(player_is_big_winner)
            .unwrap_or(false)
    }) {
        has_big_winner = true;
        big_winner_score = big.score;
    }

    let score_changes =
        build_score_changes_nonstop(&room.players, &game.nonstop_winners, &partial_awardees);

    let score_details = if let Some(setter_i) = answer_setter_idx {
        let setter_id = room.players[setter_i].id.clone();
        let setter_username = room.players[setter_i].username.clone();
        let setter_score = calculate_nonstop_setter_score(
            has_big_winner,
            big_winner_score,
            winners_count,
            total_players_count,
        );
        room.players[setter_i].score += setter_score;
        let setter_info = Some(json!({
            "type": "setter",
            "username": setter_username,
            "score": setter_score,
            "reason": nonstop_setter_score_reason(has_big_winner, winners_count, total_players_count)
        }));
        let details = generate_score_details(&room.players, &score_changes, setter_info);
        // Keep deterministic ordering for stable score details.
        // ensure setter_id used? not needed.
        let _ = setter_id;
        details
    } else {
        generate_score_details(&room.players, &score_changes, None)
    };

    let guesses_payload = game.guesses.clone();
    emit_to_room(
        io,
        room_id.to_string(),
        "gameEnded",
        json!({
            "guesses": guesses_payload,
            "scoreDetails": score_details,
            "answerCharacter": game.character,
        }),
    );

    revert_setter_observers(room, room_id, io);
    for p in &mut room.players {
        p.is_answer_setter = false;
    }
    emit_to_room(io, room_id.to_string(), "resetReadyStatus", json!({}));
    room.current_game = None;
    emit_to_room(
        io,
        room_id.to_string(),
        "updatePlayers",
        json!({
            "players": room.players,
            "isPublic": room.is_public,
            "answerSetterId": Value::Null,
        }),
    );
    broadcast_lobby_rooms_updated(io);

    true
}

pub fn finalize_standard_game(room: &mut Room, room_id: &str, io: &SocketIo, force: bool) -> bool {
    let Some(game) = room.current_game.as_mut() else {
        return false;
    };

    let nonstop_mode = game.settings.as_ref().is_some_and(|s| s.nonstop_mode);
    if nonstop_mode {
        return false;
    }

    let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);

    // merge pending tagBan into state in syncMode
    if sync_mode {
        let pending_list = game.tag_ban_state_pending.clone();
        if !pending_list.is_empty() {
            let mut tag_ban_changed = false;
            for entry in pending_list {
                let tag = entry.tag.trim();
                if tag.is_empty() {
                    continue;
                }

                let revealer_list: Vec<String> = entry.revealer;

                let mut target_idx: Option<usize> = None;
                for (i, item) in game.tag_ban_state.iter().enumerate() {
                    if item.tag == tag {
                        target_idx = Some(i);
                        break;
                    }
                }

                if target_idx.is_none() {
                    game.tag_ban_state.push(TagBanEntry {
                        tag: tag.to_string(),
                        revealer: Vec::new(),
                    });
                    target_idx = Some(game.tag_ban_state.len() - 1);
                    tag_ban_changed = true;
                }

                let idx = target_idx.unwrap();
                let existing: HashSet<String> =
                    game.tag_ban_state[idx].revealer.iter().cloned().collect();
                let initial_size = existing.len();

                let mut merged = existing;
                for id in revealer_list {
                    merged.insert(id);
                }

                if merged.len() != initial_size {
                    let mut merged_list = merged.into_iter().collect::<Vec<_>>();
                    merged_list.sort();
                    game.tag_ban_state[idx].revealer = merged_list;
                    tag_ban_changed = true;
                }
            }

            game.tag_ban_state_pending.clear();

            if tag_ban_changed {
                emit_to_room(
                    io,
                    room_id.to_string(),
                    "tagBanStateUpdate",
                    json!({
                        "tagBanState": game.tag_ban_state,
                    }),
                );
            }
        }
    }

    let active_players: Vec<Player> = room
        .players
        .iter()
        .filter(|p| !p.is_answer_setter && (p.team.as_deref() != Some("0") || p.temp_observer))
        .cloned()
        .collect();

    let all_ended = active_players
        .iter()
        .all(|p| player_has_result(p) || p.disconnected);

    let first_winner = game.first_winner.clone();
    let sync_mode_effective = sync_mode && !nonstop_mode;

    let mut actual_winners: Vec<Player> = Vec::new();

    if sync_mode_effective {
        actual_winners = active_players
            .iter()
            .filter(|p| player_is_winner(p))
            .cloned()
            .collect();
    } else {
        let answer_id = Some(json!(game.character.id));

        // bigwinner priority
        let mut bigwinner: Option<Player> = if first_winner.as_ref().is_some_and(|fw| fw.is_big_win)
        {
            let fw_id = first_winner.as_ref().map(|fw| fw.id.as_str()).unwrap_or("");
            active_players
                .iter()
                .find(|p| p.id == fw_id)
                .cloned()
                .or_else(|| {
                    active_players
                        .iter()
                        .find(|p| player_is_big_winner(p))
                        .cloned()
                })
        } else {
            active_players
                .iter()
                .find(|p| player_is_big_winner(p))
                .cloned()
        };

        if bigwinner.is_none() {
            if let Some(answer_id) = answer_id {
                let answer_id_str = answer_id.to_string();
                if let Some(avatar_big_winner) = active_players.iter().find(|p| {
                    player_is_winner(p)
                        && p.avatar_id.as_ref().map(|v| v.as_key_string())
                            == Some(answer_id_str.clone())
                }) {
                    let mut aw = avatar_big_winner.clone();
                    aw.round_result = Some(RESULT_BIG_WIN.to_string());
                    bigwinner = Some(aw);
                }
            }
        }

        let winner: Option<Player> = if bigwinner.is_none()
            && first_winner.as_ref().map(|fw| fw.id.as_str()).is_some()
            && !first_winner.as_ref().is_some_and(|fw| fw.is_big_win)
        {
            let fw_id = first_winner.as_ref().map(|fw| fw.id.as_str()).unwrap_or("");
            active_players
                .iter()
                .find(|p| p.id == fw_id)
                .cloned()
                .or_else(|| active_players.iter().find(|p| player_is_winner(p)).cloned())
        } else if bigwinner.is_none() {
            active_players.iter().find(|p| player_is_winner(p)).cloned()
        } else {
            None
        };

        let actual_winner = bigwinner.or(winner);
        if let Some(w) = actual_winner {
            actual_winners.push(w);
        }
    }

    let actual_winner = actual_winners.first().cloned();
    let total_rounds = game
        .settings
        .as_ref()
        .map(|s| s.max_attempts as i32)
        .unwrap_or(10);

    let should_wait_for_sync_round = sync_mode_effective
        && actual_winner.is_some()
        && !all_ended
        && !game.sync_ready_to_end
        && !force;

    if actual_winner.is_some() && should_wait_for_sync_round {
        emit_to_room(
            io,
            room_id.to_string(),
            "updatePlayers",
            json!({
                "players": room.players,
            }),
        );
        return false;
    }

    if actual_winner.is_none() && !all_ended {
        return false;
    }

    let answer_setter_idx = room.players.iter().position(|p| p.is_answer_setter);

    // Avoid borrow conflict: `compute_partial_awardees_from_guess_history` only needs guess history.
    let guesses_snapshot = game.guesses.clone();
    let partial_awardees = compute_partial_awardees_from_guess_history_guesses(&guesses_snapshot);

    // compute winners score
    let mut winner_score_results: HashMap<String, ScoreResult> = HashMap::new();
    let primary_winner: Option<Player> =
        if let Some(first_winner_id) = first_winner.as_ref().map(|fw| fw.id.as_str()) {
            actual_winners
                .iter()
                .find(|p| p.id == first_winner_id)
                .cloned()
                .or_else(|| actual_winners.first().cloned())
        } else {
            actual_winners.first().cloned()
        };

    // sync mode shared scoring
    let mut shared_detail_result: Option<Value> = None;
    let mut big_winner_actual_score: i32 = 0;

    if sync_mode_effective {
        if let Some(pw) = primary_winner.clone() {
            let score_result = calculate_winner_score(
                super::marks::player_attempt_count(&pw) as i32,
                player_is_big_winner(&pw),
                2,
                total_rounds,
            );
            let detail_result = calculate_winner_score(
                super::marks::player_attempt_count(&pw) as i32,
                player_is_big_winner(&pw),
                0,
                total_rounds,
            );
            shared_detail_result = Some(json!({"guessCount": detail_result.guess_count}));

            for w in &actual_winners {
                if let Some(p_mut) = room.players.iter_mut().find(|p| p.id == w.id) {
                    p_mut.score += score_result.total_score;
                }
                let mut winner_score_result = score_result.clone();
                winner_score_result.guess_count = detail_result.guess_count;
                winner_score_results.insert(w.id.clone(), winner_score_result);
            }

            if player_is_big_winner(&pw) {
                big_winner_actual_score = score_result.total_score;
            }
        }
    } else {
        for w in &actual_winners {
            let score_result = calculate_winner_score(
                super::marks::player_attempt_count(w) as i32,
                player_is_big_winner(w),
                2,
                total_rounds,
            );
            if let Some(p_mut) = room.players.iter_mut().find(|p| p.id == w.id) {
                p_mut.score += score_result.total_score;
            }
            winner_score_results.insert(w.id.clone(), score_result);
        }

        if let Some(pw) = primary_winner
            .clone()
            .or_else(|| actual_winners.first().cloned())
        {
            let detail = calculate_winner_score(
                super::marks::player_attempt_count(&pw) as i32,
                player_is_big_winner(&pw),
                0,
                total_rounds,
            );
            shared_detail_result = Some(json!({"guessCount": detail.guess_count}));
        }

        for w in actual_winners.iter().filter(|p| player_is_big_winner(p)) {
            let res = calculate_winner_score(
                super::marks::player_attempt_count(w) as i32,
                true,
                2,
                total_rounds,
            );
            big_winner_actual_score = big_winner_actual_score.max(res.total_score);
        }
    }

    let winner_id_set: HashSet<String> = actual_winners.iter().map(|w| w.id.clone()).collect();
    for p in &mut room.players {
        if p.is_answer_setter {
            continue;
        }
        if p.team.as_deref() == Some("0") {
            continue;
        }
        if winner_id_set.contains(&p.id) {
            continue;
        }
        if partial_awardees.contains(&p.id) {
            p.score += 1;
        }
    }

    let winner_guess_count = shared_detail_result
        .as_ref()
        .and_then(|d| d.get("guessCount"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    // score changes
    let score_changes = build_score_changes_standard(
        &room.players,
        &actual_winners,
        &winner_score_results,
        &partial_awardees,
    );

    let guesses_payload = guesses_snapshot;

    let score_details = if let Some(setter_i) = answer_setter_idx {
        let has_big_winner = primary_winner
            .as_ref()
            .map(player_is_big_winner)
            .unwrap_or(false);
        let setter_score = calculate_setter_score(
            winner_guess_count,
            has_big_winner,
            big_winner_actual_score,
            total_rounds,
        );
        let setter_username = room.players[setter_i].username.clone();
        room.players[setter_i].score += setter_score;

        let setter_info = Some(json!({
            "type": "setter",
            "username": setter_username,
            "score": setter_score,
                "reason": setter_score_reason(winner_guess_count, has_big_winner, big_winner_actual_score, total_rounds)
        }));

        generate_score_details(&room.players, &score_changes, setter_info)
    } else {
        generate_score_details(&room.players, &score_changes, None)
    };

    emit_to_room(
        io,
        room_id.to_string(),
        "gameEnded",
        json!({
            "guesses": guesses_payload,
            "scoreDetails": score_details,
            "answerCharacter": game.character,
        }),
    );

    revert_setter_observers(room, room_id, io);

    for p in &mut room.players {
        p.is_answer_setter = false;
    }

    for p in &mut room.players {
        if p.joined_during_game == Some(true) {
            p.team = None;
            p.joined_during_game = Some(false);
            p.ready = false;
        }
    }

    room.current_game = None;
    emit_to_room(
        io,
        room_id.to_string(),
        "updatePlayers",
        json!({
            "players": room.players,
            "isPublic": room.is_public,
            "answerSetterId": Value::Null,
        }),
    );
    broadcast_lobby_rooms_updated(io);

    true
}

pub fn run_standard_flow(
    room: &mut Room,
    room_id: &str,
    io: &SocketIo,
    force_finalize: bool,
    broadcast_state: bool,
) -> bool {
    let Some(game) = room.current_game.as_ref() else {
        return false;
    };

    let sync_mode = game.settings.as_ref().is_some_and(|s| s.sync_mode);

    if sync_mode {
        update_sync_progress(room, room_id, io);
    }

    if broadcast_state {
        emit_sync_and_nonstop_state(room, room_id, io, false);
    }

    let nonstop_mode = room
        .current_game
        .as_ref()
        .and_then(|g| g.settings.as_ref())
        .is_some_and(|s| s.nonstop_mode);

    if nonstop_mode {
        return finalize_nonstop_game(room, room_id, io);
    }

    finalize_standard_game(room, room_id, io, force_finalize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_player(id: &str, username: &str, result: Option<&str>) -> Player {
        Player {
            id: id.to_string(),
            stable_player_id: format!("stable-{}", id),
            username: username.to_string(),
            is_host: false,
            score: 0,
            ready: true,
            attempt_marks: Vec::new(),
            round_result: result.map(str::to_string),
            message: String::new(),
            team: None,
            disconnected: false,
            avatar_id: None,
            avatar_image: None,
            joined_during_game: None,
            temp_observer: false,
            sync_completed_round: None,
            is_answer_setter: false,
        }
    }

    #[test]
    fn standard_score_details_preserve_winner_breakdown_shape() {
        let winner = test_player("p1", "alice", Some(RESULT_BIG_WIN));
        let loser = test_player("p2", "bob", Some(RESULT_DEAD));
        let players = vec![winner.clone(), loser];
        let mut winner_scores = HashMap::new();
        winner_scores.insert(
            winner.id.clone(),
            ScoreResult {
                total_score: 14,
                guess_count: 1,
                is_big_win: true,
                bonuses: ScoreBonuses {
                    big_win: 12,
                    quick_guess: 0,
                },
            },
        );

        let score_changes =
            build_score_changes_standard(&players, &[winner], &winner_scores, &HashSet::new());
        let details = generate_score_details(&players, &score_changes, None);
        let alice = details
            .iter()
            .find(|d| d.get("username").and_then(Value::as_str) == Some("alice"))
            .expect("winner details should be emitted");

        assert_eq!(alice.get("score").and_then(Value::as_i64), Some(14));
        assert_eq!(alice.get("result").and_then(Value::as_str), Some("bigwin"));
        assert_eq!(
            alice.pointer("/breakdown/base").and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            alice.pointer("/breakdown/bigWin").and_then(Value::as_i64),
            Some(12)
        );
        assert_eq!(
            alice
                .pointer("/breakdown/quickGuess")
                .and_then(Value::as_i64),
            Some(0)
        );
    }

    #[test]
    fn nonstop_score_details_preserve_ranked_breakdown_shape() {
        let winner = test_player("p1", "alice", Some(crate::socket::gameplay::RESULT_WIN));
        let players = vec![winner.clone()];
        let winners = vec![NonstopWinner {
            id: winner.id.clone(),
            username: winner.username.clone(),
            is_big_win: false,
            team: None,
            score: 4,
            bonuses: crate::socket::state::NonstopWinnerBonuses {
                big_win: 0,
                quick_guess: 2,
            },
        }];

        let score_changes = build_score_changes_nonstop(&players, &winners, &HashSet::new());
        let details = generate_score_details(&players, &score_changes, None);
        let alice = details
            .iter()
            .find(|d| d.get("username").and_then(Value::as_str) == Some("alice"))
            .expect("winner details should be emitted");

        assert_eq!(alice.get("score").and_then(Value::as_i64), Some(4));
        assert_eq!(alice.get("result").and_then(Value::as_str), Some("win"));
        assert_eq!(
            alice.pointer("/breakdown/rank").and_then(Value::as_i64),
            Some(1)
        );
        assert_eq!(
            alice.pointer("/breakdown/base").and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            alice
                .pointer("/breakdown/quickGuess")
                .and_then(Value::as_i64),
            Some(2)
        );
    }
}
