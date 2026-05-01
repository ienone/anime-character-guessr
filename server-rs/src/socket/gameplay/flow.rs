use crate::socket::state::Room;
use tracing::info;

pub struct EnforceResult {
    pub exhausted: bool,
    pub pending_win: bool,
}

pub fn enforce_attempt_limit(room: &mut Room, player_id: &str, is_correct: bool) -> EnforceResult {
    // 1. Get settings and max attempts
    let max_attempts = room.settings.as_ref()
        .and_then(|s| s.get("maxAttempts"))
        .and_then(|v| v.as_i64())
        .unwrap_or(10) as usize;

    let (is_team_mode, team) = {
        let p = room.players.iter().find(|p| p.id == player_id);
        if let Some(p) = p {
            let t = p.team.clone();
            (t.is_some() && t.as_deref() != Some("0"), t)
        } else {
            return EnforceResult { exhausted: false, pending_win: false };
        }
    };

    // 2. Count current attempts
    let source_marks = if is_team_mode {
        let t = team.as_ref().unwrap();
        room.current_game.as_ref()
            .and_then(|g| g.team_guesses.get(t))
            .cloned()
            .unwrap_or_default()
    } else {
        room.players.iter().find(|p| p.id == player_id)
            .map(|p| p.guesses.clone())
            .unwrap_or_default()
    };

    let attempt_count = super::marks::count_attempt_marks(&source_marks);
    
    if attempt_count >= max_attempts {
        if is_correct {
            return EnforceResult { exhausted: false, pending_win: true };
        }
        
        // Exhausted: apply skull mark
        if is_team_mode {
            let t = team.as_ref().unwrap();
            if let Some(ref mut game) = room.current_game {
                let current_guesses = game.team_guesses.get(t).cloned().unwrap_or_default();
                if !super::marks::has_end_mark(&current_guesses) {
                    let new_guesses = format!("{}💀", current_guesses);
                    game.team_guesses.insert(t.clone(), new_guesses.clone());
                    
                    // Sync to all teammates
                    for p in &mut room.players {
                        if p.team.as_deref() == Some(t) && !p.is_answer_setter && !p.disconnected {
                            p.guesses = new_guesses.clone();
                        }
                    }
                }
            }
        } else {
            if let Some(p) = room.players.iter_mut().find(|p| p.id == player_id) {
                if !super::marks::has_end_mark(&p.guesses) {
                    p.guesses = format!("{}💀", p.guesses);
                }
            }
        }
        return EnforceResult { exhausted: true, pending_win: false };
    }

    EnforceResult { exhausted: false, pending_win: false }
}

pub fn check_all_ended(room: &Room) -> bool {
    // Check if all active players (not disconnected, not setter, not spectator) have end marks
    let active_players: Vec<_> = room.players.iter()
        .filter(|p| !p.disconnected && !p.is_answer_setter && p.team.as_deref() != Some("0"))
        .collect();

    if active_players.is_empty() {
        return false;
    }

    active_players.iter().all(|p| super::marks::has_end_mark(&p.guesses))
}
