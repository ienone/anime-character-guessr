use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::{json, Value};
use socketioxide::SocketIo;

use crate::socket::state::{CurrentGame, Player, Room};

use super::{calculate_nonstop_setter_score, calculate_setter_score, calculate_winner_score};

const SYNC_WAITING_MIN_INTERVAL_MS: i64 = 150;

fn ends_with_end_mark(guesses: &str, mark: &str) -> bool {
    guesses.ends_with(mark)
}

fn last_end_result(guesses: &str) -> &'static str {
    if ends_with_end_mark(guesses, "🏆") {
        return "teamwin";
    }
    if ends_with_end_mark(guesses, "💀") {
        return "lose";
    }
    if ends_with_end_mark(guesses, "🏳️") {
        return "surrender";
    }
    ""
}

fn setter_score_reason(winner_guesses: &str, winner_guess_count: i32, big_winner_score: i32, total_rounds: i32) -> &'static str {
    if winner_guesses.contains('👑') {
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

fn nonstop_setter_score_reason(has_big_winner: bool, winners_count: i32, total_players_count: i32) -> &'static str {
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
        "teamGuesses": game.team_guesses,
    })
}

fn has_ended_mark(player: &Player) -> bool {
    player.guesses.contains('✌')
        || player.guesses.contains('💀')
        || player.guesses.contains("🏳️")
        || player.guesses.contains('👑')
        || player.guesses.contains('🏆')
}

fn build_sync_waiting_key(round: u32, sync_status: &[Value]) -> String {
    let mut normalized: Vec<(String, bool)> = sync_status
        .iter()
        .filter_map(|s| {
            let id = s.get("id")?.as_str()?.to_string();
            let completed = s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false);
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

pub fn emit_sync_and_nonstop_state(room: &mut Room, room_id: &str, io: &SocketIo, force_sync_waiting: bool) {
    let Some(game) = room.current_game.as_mut() else {
        return;
    };

    let sync_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let nonstop_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("nonstopMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if sync_mode {
        let sync_players: Vec<&Player> = room
            .players
            .iter()
            .filter(|p| {
                !p.is_answer_setter
                    && p.team.as_deref() != Some("0")
                    && !p.disconnected
                    && !has_ended_mark(p)
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
            let _ = io.to(room_id.to_string()).emit("syncWaiting", &payload);
        }

        if game.sync_winner_found && !nonstop_mode {
            let winner_username = game
                .sync_winner
                .as_ref()
                .and_then(|w| w.get("username"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = io.to(room_id.to_string()).emit(
                "syncGameEnding",
                &json!({
                    "winnerUsername": winner_username,
                    "message": format!("{} 已猜对！等待本轮结束...", winner_username),
                }),
            );
        }
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
            .filter(|p| !has_ended_mark(p))
            .collect();

        let winners = game
            .nonstop_winners
            .iter()
            .enumerate()
            .map(|(idx, w)| {
                let username = w.get("username").cloned().unwrap_or(Value::String(String::new()));
                let score = w.get("score").cloned().unwrap_or(Value::Number(0.into()));
                json!({
                    "username": username,
                    "rank": (idx + 1) as i32,
                    "score": score,
                })
            })
            .collect::<Vec<_>>();

        let _ = io.to(room_id.to_string()).emit(
            "nonstopProgress",
            &json!({
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

    let _ = io.to(room_id.to_string()).emit(
        "updatePlayers",
        &json!({
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
        let _ = io
            .to(room_id.to_string())
            .emit("updatePlayers", &json!({ "players": room.players }));
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

    let entry = game.team_guesses.entry(team_id.clone()).or_default();
    if !entry.contains('🏆') {
        entry.push('🏆');
    }

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
            if !teammate.guesses.contains('🏆') {
                teammate.guesses.push('🏆');
            }
            teammate.temp_observer = true;
            game.sync_players_completed.remove(&teammate.id);
            let _ = io.to(teammate.id.clone()).emit(
                "teamWin",
                &json!({
                    "winnerName": winner.username,
                    "message": format!("队友 {} 已猜对！", winner.username),
                }),
            );
        }
    }

    let nonstop_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("nonstopMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sync_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !nonstop_mode && sync_mode {
        if let Some(winner_mut) = room.players.iter_mut().find(|p| p.id == winner_id) {
            if winner_mut.team.as_deref() != Some("0") {
                winner_mut.temp_observer = true;
            }
        }
    }

    let _ = io
        .to(room_id.to_string())
        .emit("updatePlayers", &json!({ "players": room.players }));
}

pub fn init_game_state(
    room: &mut Room,
    character: Value,
    settings: Option<Value>,
    hints: Option<Value>,
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
        team_guesses: HashMap::new(),
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
        p.guesses.clear();
        if p.temp_observer {
            p.temp_observer = false;
        }
        p.sync_completed_round = None;
        p.is_answer_setter = answer_setter_id == Some(p.id.as_str());
        if !p.is_answer_setter && p.team.as_deref() != Some("0") {
            game.guesses.push(json!({
                "username": p.username,
                "guesses": [],
            }));
        }
    }

    for p in &room.players {
        if let Some(team) = p.team.as_deref() {
            if team != "0" {
                game.team_guesses.entry(team.to_string()).or_insert_with(String::new);
            }
        }
    }

    room.last_active = Utc::now().timestamp_millis();
}

pub fn update_sync_progress(room: &mut Room, room_id: &str, io: &SocketIo) {
    let Some(game) = room.current_game.as_mut() else {
        return;
    };

    let sync_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !sync_mode {
        return;
    }

    let is_ended = |p: &Player| has_ended_mark(p);

    let sync_players: Vec<Player> = room
        .players
        .iter()
        .filter(|p| {
            !p.is_answer_setter
                && p.team.as_deref() != Some("0")
                && !p.disconnected
                && !is_ended(p)
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

    let all_completed = sync_status
        .iter()
        .all(|s| s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false));

    if all_completed {
        // Merge tagBanStatePending
        let mut pending_ban_broadcast: Option<Vec<Value>> = None;

        if game
            .settings
            .as_ref()
            .and_then(|s| s.get("tagBan"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            && !game.tag_ban_state_pending.is_empty()
        {
            let mut existing_tags: HashSet<String> = game
                .tag_ban_state
                .iter()
                .filter_map(|item| item.get("tag").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect();

            let mut pending_new_entries: Vec<Value> = Vec::new();
            for entry in game.tag_ban_state_pending.iter() {
                let Some(tag_name) = entry.get("tag").and_then(|v| v.as_str()) else {
                    continue;
                };
                let tag_name = tag_name.trim();
                if tag_name.is_empty() || existing_tags.contains(tag_name) {
                    continue;
                }
                existing_tags.insert(tag_name.to_string());
                let revealers = entry
                    .get("revealer")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut uniq: HashSet<String> = HashSet::new();
                let revealer_vec = revealers
                    .into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .filter(|s| uniq.insert(s.clone()))
                    .map(Value::String)
                    .collect::<Vec<_>>();

                pending_new_entries.push(json!({
                    "tag": tag_name,
                    "revealer": revealer_vec,
                }));
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
            let _ = io.to(room_id.to_string()).emit(
                "tagBanStateUpdate",
                &json!({
                    "tagBanState": state,
                }),
            );
        }

        let nonstop_mode = game
            .settings
            .as_ref()
            .and_then(|s| s.get("nonstopMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !nonstop_mode && game.sync_winner_found {
            game.sync_ready_to_end = true;
            let payload = json!({
                "round": game.sync_round,
                "syncStatus": sync_status,
                "completedCount": sync_status.len(),
                "totalCount": sync_status.len(),
            });
            if !should_skip_sync_waiting(game, &payload, false) {
                let _ = io.to(room_id.to_string()).emit("syncWaiting", &payload);
            }

            let winner_username = game
                .sync_winner
                .as_ref()
                .and_then(|w| w.get("username"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = io.to(room_id.to_string()).emit(
                "syncGameEnding",
                &json!({
                    "winnerUsername": winner_username,
                    "message": format!("{} 已猜对！等待本轮结束...", winner_username),
                }),
            );

            // finalize_standard_game needs &mut room; defer until we release the `game` borrow.
            // We'll return early after finalization.
            #[allow(dropping_references)]
            drop(game);
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
                    && !has_ended_mark(p)
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

        let _ = io.to(room_id.to_string()).emit(
            "syncRoundStart",
            &json!({
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
            let _ = io.to(room_id.to_string()).emit("syncWaiting", &next_payload);
        }
    } else {
        let payload = json!({
            "round": game.sync_round,
            "syncStatus": sync_status,
            "completedCount": sync_status.iter().filter(|s| s.get("completed").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
            "totalCount": sync_status.len(),
        });

        if !should_skip_sync_waiting(game, &payload, false) {
            let _ = io.to(room_id.to_string()).emit("syncWaiting", &payload);
        }

        let nonstop_mode = game
            .settings
            .as_ref()
            .and_then(|s| s.get("nonstopMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !nonstop_mode && game.sync_winner_found {
            let winner_username = game
                .sync_winner
                .as_ref()
                .and_then(|w| w.get("username"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = io.to(room_id.to_string()).emit(
                "syncGameEnding",
                &json!({
                    "winnerUsername": winner_username,
                    "message": format!("{} 已猜对！等待本轮结束...", winner_username),
                }),
            );
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

    let players_by_id: HashMap<String, &Player> = room.players.iter().map(|p| (p.id.clone(), p)).collect();
    let mut first_partial_index_by_player: HashMap<String, usize> = HashMap::new();

    for player_guesses in guesses {
        let list = player_guesses.get("guesses").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for (idx, g) in list.iter().enumerate() {
            let Some(player_id) = g.get("playerId").and_then(|v| v.as_str()) else {
                continue;
            };
            let is_partial = g
                .get("isPartialCorrect")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let is_correct = g.get("isCorrect").and_then(|v| v.as_bool()).unwrap_or(false);
            if is_partial && !is_correct {
                first_partial_index_by_player.entry(player_id.to_string()).or_insert(idx);
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

fn generate_score_details(players: &[Player], score_changes: &HashMap<String, Value>, setter_info: Option<Value>) -> Vec<Value> {
    let active_players: Vec<&Player> = players.iter().filter(|p| p.team.as_deref() != Some("0")).collect();

    let mut team_map: HashMap<String, Vec<Value>> = HashMap::new();
    let mut no_team_players: Vec<Value> = Vec::new();

    for p in active_players {
        if p.is_answer_setter {
            continue;
        }

        let change = score_changes
            .get(&p.id)
            .cloned()
            .unwrap_or_else(|| json!({ "score": 0, "breakdown": {}, "result": "" }));

        let player_info = json!({
            "id": p.id,
            "username": p.username,
            "team": p.team,
            "score": change.get("score").cloned().unwrap_or(Value::Number(0.into())),
            "breakdown": change.get("breakdown").cloned().unwrap_or_else(|| json!({})),
            "result": change.get("result").cloned().unwrap_or(Value::String(String::new())),
        });

        if let Some(team) = p.team.as_deref() {
            if !team.is_empty() && team != "0" {
                team_map.entry(team.to_string()).or_default().push(player_info);
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
    winner_score_results: &HashMap<String, Value>,
    partial_awardees: &HashSet<String>,
) -> HashMap<String, Value> {
    let mut score_changes: HashMap<String, Value> = HashMap::new();
    let active_players: Vec<&Player> = players
        .iter()
        .filter(|p| !p.is_answer_setter && (p.team.as_deref() != Some("0") || p.temp_observer))
        .collect();

    let winner_id_set: HashSet<String> = actual_winners.iter().map(|w| w.id.clone()).collect();

    for p in active_players {
        if winner_id_set.contains(&p.id) {
            let res = winner_score_results.get(&p.id).cloned().unwrap_or_else(|| json!({"totalScore":0,"bonuses":{}}));
            let bonuses = res.get("bonuses").cloned().unwrap_or_else(|| json!({}));
            score_changes.insert(
                p.id.clone(),
                json!({
                    "score": res.get("totalScore").cloned().unwrap_or(Value::Number(0.into())),
                    "breakdown": {
                        "base": 2,
                        "bigWin": bonuses.get("bigWin").cloned().unwrap_or(Value::Number(0.into())),
                        "quickGuess": bonuses.get("quickGuess").cloned().unwrap_or(Value::Number(0.into())),
                    },
                    "result": if p.guesses.contains('👑') { "bigwin" } else { "win" }
                }),
            );
        } else {
            let has_partial = partial_awardees.contains(&p.id);
            score_changes.insert(
                p.id.clone(),
                json!({
                    "score": if has_partial { 1 } else { 0 },
                    "breakdown": if has_partial { json!({"partial":1}) } else { json!({}) },
                    "result": last_end_result(&p.guesses)
                }),
            );
        }
    }

    score_changes
}

fn build_score_changes_nonstop(
    players: &[Player],
    nonstop_winners: &[Value],
    partial_awardees: &HashSet<String>,
) -> HashMap<String, Value> {
    let mut score_changes: HashMap<String, Value> = HashMap::new();

    let winner_ids: HashSet<String> = nonstop_winners
        .iter()
        .filter_map(|w| w.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

    for (idx, w) in nonstop_winners.iter().enumerate() {
        let Some(wid) = w.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let winner_player = players.iter().find(|p| p.id == wid);
        let is_big_win = winner_player.map(|p| p.guesses.contains('👑')).unwrap_or(false);

        let bonuses = w.get("bonuses").cloned().unwrap_or_else(|| json!({}));
        let big_win_bonus = bonuses
            .get("bigWin")
            .and_then(|v| v.as_i64())
            .unwrap_or(if is_big_win { 12 } else { 0 });
        let quick_guess_bonus = bonuses
            .get("quickGuess")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let score = w.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        let base_score = (score - big_win_bonus - quick_guess_bonus).max(0);

        let mut breakdown = serde_json::Map::new();
        breakdown.insert("rank".to_string(), Value::Number(((idx + 1) as i64).into()));
        breakdown.insert("base".to_string(), Value::Number((base_score as i64).into()));
        if big_win_bonus != 0 {
            breakdown.insert("bigWin".to_string(), Value::Number(big_win_bonus.into()));
        }
        if quick_guess_bonus != 0 {
            breakdown.insert("quickGuess".to_string(), Value::Number(quick_guess_bonus.into()));
        }

        score_changes.insert(
            wid.to_string(),
            json!({
                "score": score,
                "breakdown": Value::Object(breakdown),
                "result": if is_big_win { "bigwin" } else { "win" }
            }),
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
            json!({
                "score": if has_partial { 1 } else { 0 },
                "breakdown": if has_partial { json!({"partial":1}) } else { json!({}) },
                "result": if ends_with_end_mark(&p.guesses, "💀") { "lose" } else if ends_with_end_mark(&p.guesses, "🏳️") { "surrender" } else { "" }
            }),
        );
    }

    score_changes
}

pub fn finalize_nonstop_game(room: &mut Room, room_id: &str, io: &SocketIo) -> bool {
    let nonstop_mode = room
        .current_game
        .as_ref()
        .and_then(|g| g.settings.as_ref())
        .and_then(|s| s.get("nonstopMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
        .filter(|p| !has_ended_mark(p))
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

    let winner_ids: HashSet<String> = game
        .nonstop_winners
        .iter()
        .filter_map(|w| w.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

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
        let Some(wid) = w.get("id").and_then(|v| v.as_str()) else {
            return false;
        };
        room.players
            .iter()
            .find(|p| p.id == wid)
            .map(|p| p.guesses.contains('👑'))
            .unwrap_or(false)
    }) {
        has_big_winner = true;
        big_winner_score = big.get("score").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    }

    let score_changes = build_score_changes_nonstop(&room.players, &game.nonstop_winners, &partial_awardees);

    let score_details = if let Some(setter_i) = answer_setter_idx {
        let setter_id = room.players[setter_i].id.clone();
        let setter_username = room.players[setter_i].username.clone();
        let setter_score = calculate_nonstop_setter_score(has_big_winner, big_winner_score, winners_count, total_players_count);
        room.players[setter_i].score += setter_score;
        let setter_info = Some(json!({
            "type": "setter",
            "username": setter_username,
            "score": setter_score,
            "reason": nonstop_setter_score_reason(has_big_winner, winners_count, total_players_count)
        }));
        let details = generate_score_details(&room.players, &score_changes, setter_info);
        // keep deterministic ordering? Node doesn't; ok.
        // ensure setter_id used? not needed.
        let _ = setter_id;
        details
    } else {
        generate_score_details(&room.players, &score_changes, None)
    };

    let guesses_payload = game.guesses.clone();
    let _ = io.to(room_id.to_string()).emit(
        "gameEnded",
        &json!({
            "guesses": guesses_payload,
            "scoreDetails": score_details,
        }),
    );

    revert_setter_observers(room, room_id, io);
    for p in &mut room.players {
        p.is_answer_setter = false;
    }
    let _ = io.to(room_id.to_string()).emit("resetReadyStatus", &json!({}));
    room.current_game = None;
    let _ = io.to(room_id.to_string()).emit(
        "updatePlayers",
        &json!({
            "players": room.players,
            "isPublic": room.is_public,
            "answerSetterId": Value::Null,
        }),
    );

    true
}

pub fn finalize_standard_game(room: &mut Room, room_id: &str, io: &SocketIo, force: bool) -> bool {
    // Phase 1 (scoped): mutable game borrow for early checks + tagBan merge, extract fields as owned.
    // The block ends before compute_partial_awardees_from_guess_history needs &room.
    let (nonstop_mode, sync_mode, first_winner, total_rounds, sync_ready_to_end, answer_character_id) = {
        let Some(game) = room.current_game.as_mut() else {
            return false;
        };

        let nonstop_mode = game
            .settings.as_ref().and_then(|s| s.get("nonstopMode")).and_then(|v| v.as_bool()).unwrap_or(false);
        if nonstop_mode { return false; }

        let sync_mode = game
            .settings.as_ref().and_then(|s| s.get("syncMode")).and_then(|v| v.as_bool()).unwrap_or(false);

        if sync_mode {
            let pending_list = game.tag_ban_state_pending.clone();
            if !pending_list.is_empty() {
                let mut tag_ban_changed = false;
                for entry in pending_list {
                    let Some(tag) = entry.get("tag").and_then(|v| v.as_str()) else { continue; };
                    let tag = tag.trim();
                    if tag.is_empty() { continue; }
                    let revealer_list: Vec<String> = entry.get("revealer").and_then(|v| v.as_array())
                        .cloned().unwrap_or_default().into_iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                    let mut target_idx: Option<usize> = None;
                    for (i, item) in game.tag_ban_state.iter().enumerate() {
                        if item.get("tag").and_then(|v| v.as_str()) == Some(tag) { target_idx = Some(i); break; }
                    }
                    if target_idx.is_none() {
                        game.tag_ban_state.push(json!({"tag": tag, "revealer": []}));
                        target_idx = Some(game.tag_ban_state.len() - 1);
                        tag_ban_changed = true;
                    }
                    let idx = target_idx.unwrap();
                    let existing: HashSet<String> = game.tag_ban_state[idx].get("revealer")
                        .and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
                    let initial_size = existing.len();
                    let mut merged = existing;
                    for id in revealer_list { merged.insert(id); }
                    if merged.len() != initial_size {
                        let mut merged_list = merged.into_iter().map(Value::String).collect::<Vec<_>>();
                        merged_list.sort_by(|a, b| a.as_str().unwrap_or("").cmp(b.as_str().unwrap_or("")));
                        if let Some(obj) = game.tag_ban_state[idx].as_object_mut() {
                            obj.insert("revealer".to_string(), Value::Array(merged_list));
                        }
                        tag_ban_changed = true;
                    }
                }
                game.tag_ban_state_pending.clear();
                if tag_ban_changed {
                    let _ = io.to(room_id.to_string()).emit("tagBanStateUpdate",
                        &json!({"tagBanState": game.tag_ban_state}));
                }
            }
        }

        let first_winner = game.first_winner.clone();
        let total_rounds = game.settings.as_ref()
            .and_then(|s| s.get("maxAttempts")).and_then(|v| v.as_i64()).unwrap_or(10) as i32;
        let sync_ready_to_end = game.sync_ready_to_end;
        let answer_character_id = game.character.get("id").cloned();
        (nonstop_mode, sync_mode, first_winner, total_rounds, sync_ready_to_end, answer_character_id)
        // mutable game borrow released here
    };

    // Phase 2: no mutable game borrow active — freely use &room
    let active_players: Vec<Player> = room.players.iter()
        .filter(|p| !p.is_answer_setter && (p.team.as_deref() != Some("0") || p.temp_observer))
        .cloned().collect();
    let all_ended = active_players.iter().all(|p| has_ended_mark(p) || p.disconnected);
    let sync_mode_effective = sync_mode && !nonstop_mode;

    let mut actual_winners: Vec<Player> = Vec::new();
    if sync_mode_effective {
        actual_winners = active_players.iter()
            .filter(|p| p.guesses.contains('\u{270c}') || p.guesses.contains('\u{1f451}'))
            .cloned().collect();
    } else {
        let mut bigwinner: Option<Player> = if first_winner.as_ref()
            .and_then(|fw| fw.get("isBigWin").and_then(|v| v.as_bool())).unwrap_or(false)
        {
            let fw_id = first_winner.as_ref().and_then(|fw| fw.get("id").and_then(|v| v.as_str())).unwrap_or("");
            active_players.iter().find(|p| p.id == fw_id).cloned()
                .or_else(|| active_players.iter().find(|p| p.guesses.contains('\u{1f451}')).cloned())
        } else {
            active_players.iter().find(|p| p.guesses.contains('\u{1f451}')).cloned()
        };

        if bigwinner.is_none() {
            if let Some(ref answer_id) = answer_character_id {
                let answer_id_str = answer_id.to_string();
                if let Some(avatar_big_winner) = active_players.iter().find(|p| {
                    (p.guesses.contains('\u{270c}') || p.guesses.contains('\u{1f451}'))
                        && p.avatar_id.as_ref().map(|v| v.to_string()) == Some(answer_id_str.clone())
                }) {
                    let mut aw = avatar_big_winner.clone();
                    if !aw.guesses.contains('\u{1f451}') {
                        aw.guesses = aw.guesses.replace('\u{270c}', "") + "\u{1f451}";
                    }
                    bigwinner = Some(aw);
                }
            }
        }

        let winner: Option<Player> = if bigwinner.is_none()
            && first_winner.as_ref().and_then(|fw| fw.get("id").and_then(|v| v.as_str())).is_some()
            && !first_winner.as_ref().and_then(|fw| fw.get("isBigWin").and_then(|v| v.as_bool())).unwrap_or(false)
        {
            let fw_id = first_winner.as_ref().and_then(|fw| fw.get("id").and_then(|v| v.as_str())).unwrap_or("");
            active_players.iter().find(|p| p.id == fw_id).cloned()
                .or_else(|| active_players.iter().find(|p| p.guesses.contains('\u{270c}')).cloned())
        } else if bigwinner.is_none() {
            active_players.iter().find(|p| p.guesses.contains('\u{270c}')).cloned()
        } else {
            None
        };

        if let Some(w) = bigwinner.or(winner) { actual_winners.push(w); }
    }

    let actual_winner = actual_winners.first().cloned();
    let should_wait = sync_mode_effective && actual_winner.is_some() && !all_ended && !sync_ready_to_end && !force;
    if actual_winner.is_some() && should_wait {
        let _ = io.to(room_id.to_string()).emit("updatePlayers", &json!({"players": room.players}));
        return false;
    }
    if actual_winner.is_none() && !all_ended { return false; }

    let answer_setter_idx = room.players.iter().position(|p| p.is_answer_setter);
    // No mutable game borrow active — safe to pass &room
    let partial_awardees = compute_partial_awardees_from_guess_history(room);

    // Phase 3: scoring (re-acquires mutable borrows of room.players as needed)
    let primary_winner: Option<Player> = if let Some(fw_id) = first_winner.as_ref()
        .and_then(|fw| fw.get("id").and_then(|v| v.as_str()))
    {
        actual_winners.iter().find(|p| p.id == fw_id).cloned().or_else(|| actual_winners.first().cloned())
    } else {
        actual_winners.first().cloned()
    };

    let mut winner_score_results: HashMap<String, Value> = HashMap::new();
    let mut shared_detail_result: Option<Value> = None;
    let mut big_winner_actual_score: i32 = 0;

    if sync_mode_effective {
        if let Some(pw) = primary_winner.clone() {
            let score_result = calculate_winner_score(&pw.guesses, 2, total_rounds);
            let detail_result = calculate_winner_score(&pw.guesses, 0, total_rounds);
            shared_detail_result = Some(json!({"guessCount": detail_result.guess_count}));
            for w in &actual_winners {
                if let Some(p_mut) = room.players.iter_mut().find(|p| p.id == w.id) {
                    p_mut.score += score_result.total_score;
                }
                winner_score_results.insert(w.id.clone(), json!({
                    "totalScore": score_result.total_score,
                    "guessCount": detail_result.guess_count,
                    "bonuses": score_result.bonuses,
                }));
            }
            if pw.guesses.contains('\u{1f451}') { big_winner_actual_score = score_result.total_score; }
        }
    } else {
        for w in &actual_winners {
            let score_result = calculate_winner_score(&w.guesses, 2, total_rounds);
            if let Some(p_mut) = room.players.iter_mut().find(|p| p.id == w.id) {
                p_mut.score += score_result.total_score;
            }
            winner_score_results.insert(w.id.clone(), json!({
                "totalScore": score_result.total_score,
                "guessCount": score_result.guess_count,
                "bonuses": score_result.bonuses,
            }));
        }
        if let Some(pw) = primary_winner.clone().or_else(|| actual_winners.first().cloned()) {
            let detail = calculate_winner_score(&pw.guesses, 0, total_rounds);
            shared_detail_result = Some(json!({"guessCount": detail.guess_count}));
        }
        for w in actual_winners.iter().filter(|p| p.guesses.contains('\u{1f451}')) {
            let res = calculate_winner_score(&w.guesses, 2, total_rounds);
            big_winner_actual_score = big_winner_actual_score.max(res.total_score);
        }
    }

    let winner_id_set: HashSet<String> = actual_winners.iter().map(|w| w.id.clone()).collect();
    for p in &mut room.players {
        if p.is_answer_setter || p.team.as_deref() == Some("0") || winner_id_set.contains(&p.id) { continue; }
        if partial_awardees.contains(&p.id) { p.score += 1; }
    }

    let winner_guess_count = shared_detail_result.as_ref()
        .and_then(|d| d.get("guessCount")).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let score_changes = build_score_changes_standard(&room.players, &actual_winners, &winner_score_results, &partial_awardees);

    // Read guesses_payload via immutable ref (no mutable game borrow needed)
    let guesses_payload = room.current_game.as_ref().map(|g| g.guesses.clone()).unwrap_or_default();

    let score_details = if let Some(setter_i) = answer_setter_idx {
        let primary_guesses = primary_winner.as_ref().map(|p| p.guesses.clone()).unwrap_or_default();
        let setter_score = calculate_setter_score(&primary_guesses, winner_guess_count, big_winner_actual_score, total_rounds);
        let setter_username = room.players[setter_i].username.clone();
        room.players[setter_i].score += setter_score;
        let setter_info = Some(json!({
            "type": "setter", "username": setter_username, "score": setter_score,
            "reason": setter_score_reason(&primary_guesses, winner_guess_count, big_winner_actual_score, total_rounds)
        }));
        generate_score_details(&room.players, &score_changes, setter_info)
    } else {
        generate_score_details(&room.players, &score_changes, None)
    };

    let _ = io.to(room_id.to_string()).emit("gameEnded", &json!({
        "guesses": guesses_payload, "scoreDetails": score_details,
    }));

    revert_setter_observers(room, room_id, io);
    for p in &mut room.players { p.is_answer_setter = false; }
    for p in &mut room.players {
        if p.joined_during_game == Some(true) {
            p.team = None; p.joined_during_game = Some(false); p.ready = false;
        }
    }
    room.current_game = None;
    let _ = io.to(room_id.to_string()).emit("updatePlayers", &json!({
        "players": room.players, "isPublic": room.is_public, "answerSetterId": Value::Null,
    }));
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

    let sync_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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
        .and_then(|s| s.get("nonstopMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if nonstop_mode {
        return finalize_nonstop_game(room, room_id, io);
    }

    finalize_standard_game(room, room_id, io, force_finalize)
}
