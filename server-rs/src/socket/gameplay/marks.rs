use crate::socket::state::{CurrentGame, Player};

pub const ATTEMPT_TIMEOUT: &str = "timeout";
pub const ATTEMPT_PARTIAL: &str = "partial";
pub const ATTEMPT_CORRECT: &str = "correct";
pub const ATTEMPT_WRONG: &str = "wrong";

pub const RESULT_WIN: &str = "win";
pub const RESULT_BIG_WIN: &str = "bigWin";
pub const RESULT_DEAD: &str = "dead";
pub const RESULT_SURRENDER: &str = "surrender";
pub const RESULT_TEAM_WIN: &str = "teamWin";

pub fn player_attempt_count(player: &Player) -> usize {
    player.attempt_marks.len()
}

pub fn player_has_result(player: &Player) -> bool {
    player.round_result.is_some()
}

pub fn player_is_winner(player: &Player) -> bool {
    matches!(
        player.round_result.as_deref(),
        Some(RESULT_WIN | RESULT_BIG_WIN | RESULT_TEAM_WIN)
    )
}

pub fn player_is_big_winner(player: &Player) -> bool {
    player.round_result.as_deref() == Some(RESULT_BIG_WIN)
}

pub fn player_result(player: &Player) -> Option<&str> {
    player.round_result.as_deref()
}

pub fn set_player_result(player: &mut Player, result: &str) {
    player.round_result = Some(result.to_string());
}

pub fn clear_player_result(player: &mut Player) {
    player.round_result = None;
}

pub fn push_player_attempt(player: &mut Player, mark: &str) {
    player.attempt_marks.push(mark.to_string());
}

pub fn team_attempt_count(game: &CurrentGame, team_id: &str) -> usize {
    game.team_attempt_marks
        .get(team_id)
        .map(Vec::len)
        .unwrap_or(0)
}

pub fn team_has_result(game: &CurrentGame, team_id: &str) -> bool {
    game.team_round_results.contains_key(team_id)
}

pub fn push_team_attempt(game: &mut CurrentGame, team_id: &str, mark: &str) {
    game.team_attempt_marks
        .entry(team_id.to_string())
        .or_default()
        .push(mark.to_string());
}

pub fn set_team_result(game: &mut CurrentGame, team_id: &str, result: &str) {
    game.team_round_results
        .insert(team_id.to_string(), result.to_string());
}

pub fn clear_team_result(game: &mut CurrentGame, team_id: &str) {
    game.team_round_results.remove(team_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_only_an_attempt_identifier() {
        let mut player = Player {
            id: "p1".to_string(),
            username: "p".to_string(),
            is_host: false,
            score: 0,
            ready: false,
            attempt_marks: vec![],
            round_result: None,
            message: String::new(),
            team: None,
            disconnected: false,
            avatar_id: None,
            avatar_image: None,
            joined_during_game: None,
            temp_observer: false,
            sync_completed_round: None,
            is_answer_setter: false,
        };
        push_player_attempt(&mut player, ATTEMPT_TIMEOUT);
        assert_eq!(player_attempt_count(&player), 1);
        assert!(!player_has_result(&player));
    }
}
