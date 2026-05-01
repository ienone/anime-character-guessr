use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    #[serde(rename = "totalScore")]
    pub total_score: i32,
    pub bonuses: ScoreBonuses,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScoreBonuses {
    #[serde(rename = "bigWin")]
    pub big_win: i32,
    #[serde(rename = "quickGuess")]
    pub quick_guess: i32,
}

pub fn calculate_winner_score(guesses: &str, base_score: i32, total_rounds: i32) -> ScoreResult {
    let is_big_win = guesses.contains('👑');
    let cleaned = super::marks::strip_end_marks(guesses);
    let guess_count = cleaned.chars().count() as i32;

    let mut bonuses = ScoreBonuses::default();

    if is_big_win {
        bonuses.big_win = 3;
    }

    let half_rounds = (total_rounds as f32 / 2.0).ceil() as i32;
    if guess_count <= half_rounds {
        bonuses.quick_guess = 2;
    } else if guess_count < total_rounds {
        bonuses.quick_guess = 1;
    }

    let total_score = base_score + bonuses.big_win + bonuses.quick_guess;

    ScoreResult {
        total_score,
        bonuses,
    }
}

pub fn calculate_setter_score(winner_guesses: &str, winner_guess_count: i32, big_winner_score: i32, total_rounds: i32) -> i32 {
    let has_winner = winner_guess_count > 0;
    let has_big_winner = winner_guesses.contains('👑');

    if !has_winner {
        3
    } else if has_big_winner {
        let penalty = std::cmp::max(1, big_winner_score / 2);
        -penalty
    } else {
        if winner_guess_count == 1 {
            let half_rounds = (total_rounds as f32 / 2.0).ceil() as i32;
            let cleaned = super::marks::strip_end_marks(winner_guesses);
            if (cleaned.chars().count() as i32) <= half_rounds {
                0
            } else {
                1
            }
        } else {
            0
        }
    }
}

pub fn calculate_nonstop_setter_score(has_big_winner: bool, big_winner_score: i32, winners_count: i32, total_players_count: i32) -> i32 {
    let total_players = std::cmp::max(1, total_players_count);
    let player_multiplier = std::cmp::max(1, (total_players as f32 / 2.0).ceil() as i32);

    if has_big_winner {
        let penalty = std::cmp::max(1, big_winner_score / 2);
        return -penalty;
    }

    let win_rate = winners_count as f32 / total_players as f32;
    
    if winners_count == 0 {
        return 3 * player_multiplier;
    } else if win_rate >= 0.9 {
        let penalty = 2 * player_multiplier;
        return -penalty;
    } else if win_rate <= 0.3 {
        return 2 * player_multiplier;
    } else if win_rate <= 0.6 {
        return 1 * player_multiplier;
    }
    
    0
}
