use crate::socket::state::Room;

pub struct TimeoutResult {
    pub needs_sync_update: bool,
    pub affected_player_ids: Vec<String>,
}

pub struct EnforceResult {
    pub exhausted: bool,
}

pub fn enforce_attempt_limit(room: &mut Room, player_id: &str, is_correct: bool) -> EnforceResult {
    // Enforce per-player guess limits and eliminate players after exhausting attempts.
    let Some(ref mut game) = room.current_game else {
        return EnforceResult { exhausted: false };
    };

    let Some(player) = room.players.iter().find(|p| p.id == player_id) else {
        return EnforceResult { exhausted: false };
    };

    // 出题人/旁观者/临时观战者不参与次数判定
    if player.is_answer_setter || player.team.as_deref() == Some("0") || player.temp_observer {
        return EnforceResult { exhausted: false };
    }

    let max_attempts = game
        .settings
        .as_ref()
        .map(|settings| settings.max_attempts)
        .unwrap_or(10);

    let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
    let team = player.team.clone();

    let attempt_count = if is_team_mode {
        let t = team.as_ref().unwrap();
        super::marks::team_attempt_count(game, t)
    } else {
        super::marks::player_attempt_count(player)
    };

    if attempt_count < max_attempts {
        return EnforceResult { exhausted: false };
    }

    if is_team_mode
        .then(|| team.as_ref().unwrap())
        .is_some_and(|t| super::marks::team_has_result(game, t))
        || (!is_team_mode && super::marks::player_has_result(player))
    {
        return EnforceResult { exhausted: true };
    }

    if is_correct {
        return EnforceResult { exhausted: true };
    }

    // Exhausted: apply structured death result.
    let sync_mode = game
        .settings
        .as_ref()
        .is_some_and(|settings| settings.sync_mode);

    if is_team_mode {
        let t = team.as_ref().unwrap();
        super::marks::set_team_result(game, t, super::marks::RESULT_DEAD);

        for p in &mut room.players {
            if p.team.as_deref() == Some(t) && !p.is_answer_setter && !p.disconnected {
                p.round_result = Some(super::marks::RESULT_DEAD.to_string());
                if sync_mode {
                    game.sync_players_completed.insert(p.id.clone());
                }
            }
        }
    } else {
        if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
            super::marks::set_player_result(p, super::marks::RESULT_DEAD);
            if sync_mode {
                game.sync_players_completed.insert(p.id.clone());
            }
        }
    }

    EnforceResult { exhausted: true }
}

pub fn handle_player_timeout(room: &mut Room, player_id: &str) -> TimeoutResult {
    // Eliminate timed-out players and finish the round when no active players remain.
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

    let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
    let mut affected_player_ids = Vec::new();

    let has_ended = if is_team_mode {
        let t = player.team.as_ref().unwrap();
        super::marks::team_has_result(game, t)
    } else {
        super::marks::player_has_result(&player)
    };
    if has_ended {
        return TimeoutResult {
            needs_sync_update: false,
            affected_player_ids: vec![],
        };
    }

    let sync_mode = room
        .current_game
        .as_ref()
        .and_then(|g| g.settings.as_ref())
        .is_some_and(|settings| settings.sync_mode);
    let sync_round = room
        .current_game
        .as_ref()
        .map(|g| g.sync_round)
        .unwrap_or(1);

    // Apply timeout mark to guesses (scope mutable borrows tightly)
    if is_team_mode {
        let t = player.team.as_ref().unwrap().clone();
        let team_attempts = {
            let Some(ref mut game) = room.current_game else {
                return TimeoutResult {
                    needs_sync_update: false,
                    affected_player_ids: vec![],
                };
            };
            super::marks::push_team_attempt(game, &t, super::marks::ATTEMPT_TIMEOUT);
            game.team_attempt_marks.get(&t).cloned().unwrap_or_default()
        };

        for p in &mut room.players {
            if p.team.as_deref() == Some(&t) && !p.is_answer_setter && !p.disconnected {
                p.attempt_marks = team_attempts.clone();
                affected_player_ids.push(p.id.clone());
            }
        }
    } else {
        if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
            super::marks::push_player_attempt(p, super::marks::ATTEMPT_TIMEOUT);
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
