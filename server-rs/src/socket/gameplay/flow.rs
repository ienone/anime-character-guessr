use crate::socket::state::Room;

pub struct TimeoutResult {
    pub needs_sync_update: bool,
    pub affected_player_ids: Vec<String>,
}

pub struct EnforceResult {
    pub exhausted: bool,
    #[allow(dead_code)]
    pub pending_win: bool,
}

pub fn enforce_attempt_limit(room: &mut Room, player_id: &str, is_correct: bool) -> EnforceResult {
    // Keep aligned with server/utils/gameplay.js:enforceAttemptLimit
    let Some(ref mut game) = room.current_game else {
        return EnforceResult {
            exhausted: false,
            pending_win: false,
        };
    };

    let Some(player) = room.players.iter().find(|p| p.id == player_id) else {
        return EnforceResult {
            exhausted: false,
            pending_win: false,
        };
    };

    // 出题人/旁观者/临时观战者不参与次数判定
    if player.is_answer_setter || player.team.as_deref() == Some("0") || player.temp_observer {
        return EnforceResult {
            exhausted: false,
            pending_win: false,
        };
    }

    let max_attempts = game
        .settings
        .as_ref()
        .and_then(|s| s.get("maxAttempts"))
        .and_then(|v| v.as_i64())
        .unwrap_or(10) as usize;

    let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
    let team = player.team.clone();

    let source_marks = if is_team_mode {
        let t = team.as_ref().unwrap();
        game.team_guesses.get(t).cloned().unwrap_or_default()
    } else {
        player.guesses.clone()
    };

    let attempt_count = super::marks::count_attempt_marks(&source_marks);
    if attempt_count < max_attempts {
        return EnforceResult {
            exhausted: false,
            pending_win: false,
        };
    }

    if super::marks::has_end_mark(&source_marks) {
        return EnforceResult {
            exhausted: true,
            pending_win: false,
        };
    }

    if is_correct {
        return EnforceResult {
            exhausted: true,
            pending_win: true,
        };
    }

    // Exhausted: apply skull mark (💀)
    let sync_mode = game
        .settings
        .as_ref()
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if is_team_mode {
        let t = team.as_ref().unwrap();
        let current = game.team_guesses.get(t).cloned().unwrap_or_default();
        let updated = super::marks::append_end_mark_once(&current, "💀");
        game.team_guesses.insert(t.clone(), updated.clone());

        for p in &mut room.players {
            if p.team.as_deref() == Some(t) && !p.is_answer_setter && !p.disconnected {
                p.guesses = updated.clone();
                if sync_mode {
                    game.sync_players_completed.insert(p.id.clone());
                }
            }
        }
    } else {
        if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
            p.guesses = super::marks::append_end_mark_once(&p.guesses, "💀");
            if sync_mode {
                game.sync_players_completed.insert(p.id.clone());
            }
        }
    }

    EnforceResult {
        exhausted: true,
        pending_win: false,
    }
}

pub fn handle_player_timeout(room: &mut Room, player_id: &str) -> TimeoutResult {
    // Keep aligned with server/utils/gameplay.js:handlePlayerTimeout
    let Some(game) = room.current_game.as_ref() else {
        return TimeoutResult {
            needs_sync_update: false,
            affected_player_ids: vec![],
        };
    };

    let Some(player) = room.players.iter().find(|p| p.id == player_id).cloned() else {
        return TimeoutResult {
            needs_sync_update: false,
            affected_player_ids: vec![],
        };
    };

    if player.is_answer_setter || player.team.as_deref() == Some("0") || player.temp_observer {
        return TimeoutResult {
            needs_sync_update: false,
            affected_player_ids: vec![],
        };
    }

    let timeout_mark = "⏱️";
    let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
    let mut affected_player_ids = Vec::new();

    // 已结束则忽略（避免重复 timeout 污染次数）
    let current_marks = if is_team_mode {
        let t = player.team.as_ref().unwrap();
        game.team_guesses.get(t).cloned().unwrap_or_default()
    } else {
        player.guesses.clone()
    };

    if super::marks::has_end_mark(&current_marks) {
        return TimeoutResult {
            needs_sync_update: false,
            affected_player_ids: vec![],
        };
    }

    let sync_mode = room
        .current_game
        .as_ref()
        .and_then(|g| g.settings.as_ref())
        .and_then(|s| s.get("syncMode"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sync_round = room.current_game.as_ref().map(|g| g.sync_round).unwrap_or(1);

    // Apply timeout mark to guesses (scope mutable borrows tightly)
    if is_team_mode {
        let t = player.team.as_ref().unwrap().clone();
        let updated = {
            let Some(ref mut game) = room.current_game else {
                return TimeoutResult {
                    needs_sync_update: false,
                    affected_player_ids: vec![],
                };
            };
            let existing = game.team_guesses.get(&t).cloned().unwrap_or_default();
            let updated = format!("{}{}", existing, timeout_mark);
            game.team_guesses.insert(t.clone(), updated.clone());
            updated
        };

        for p in &mut room.players {
            if p.team.as_deref() == Some(&t) && !p.is_answer_setter && !p.disconnected {
                p.guesses = updated.clone();
                affected_player_ids.push(p.id.clone());
            }
        }
    } else {
        if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
            p.guesses = format!("{}{}", p.guesses, timeout_mark);
            affected_player_ids.push(p.id.clone());
        }
    }

    // 超时后统一执行次数耗尽判定
    let _enforce = enforce_attempt_limit(room, player_id, false);

    // 同步模式：超时视为本轮完成
    let mut needs_sync_update = false;
    if sync_mode {
        if let Some(ref mut game) = room.current_game {
            game.sync_players_completed.insert(player_id.to_string());
        }
        if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
            p.sync_completed_round = Some(sync_round);
        }
        needs_sync_update = true;
    }

    TimeoutResult {
        needs_sync_update,
        affected_player_ids,
    }
}

#[allow(dead_code)]
pub fn check_all_ended(room: &Room) -> bool {
    // Check if all active players (not disconnected, not setter, not spectator) have end marks
    let active_players: Vec<_> = room.players.iter()
        .filter(|p| !p.disconnected && !p.is_answer_setter && p.team.as_deref() != Some("0") && !p.temp_observer)
        .collect();

    if active_players.is_empty() {
        return false;
    }

    active_players.iter().all(|p| super::marks::has_end_mark(&p.guesses))
}
