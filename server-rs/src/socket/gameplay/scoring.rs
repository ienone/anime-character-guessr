use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    #[serde(rename = "totalScore")]
    pub total_score: i32,
    #[serde(rename = "guessCount")]
    pub guess_count: i32,
    #[serde(rename = "isBigWin")]
    pub is_big_win: bool,
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
    // Winner score scales with guess count and supports crown-marked big wins.
    let is_big_win = guesses.contains('👑');
    let cleaned = super::marks::strip_end_marks(guesses);
    let guess_count = cleaned.chars().count() as i32;

    let mut total_score = base_score;
    let mut bonuses = ScoreBonuses::default();

    if is_big_win {
        bonuses.big_win = 12;
        total_score += bonuses.big_win;
    }

    if !is_big_win {
        if (2..=3).contains(&guess_count) {
            bonuses.quick_guess = 2;
        } else {
            let half_rounds = ((total_rounds as f32) / 2.0).ceil() as i32;
            if guess_count >= 4 && guess_count <= half_rounds {
                bonuses.quick_guess = 1;
            }
        }
        total_score += bonuses.quick_guess;
    }

    ScoreResult {
        total_score,
        guess_count,
        is_big_win,
        bonuses,
    }
}

pub fn calculate_setter_score(
    winner_guesses: &str,
    winner_guess_count: i32,
    big_winner_score: i32,
    total_rounds: i32,
) -> i32 {
    // Setter score depends on winners, skips, and per-round base score.
    let has_winner = winner_guess_count > 0;
    let has_big_winner = winner_guesses.contains('👑');

    if has_big_winner {
        let penalty = std::cmp::max(1, big_winner_score / 2);
        return -penalty;
    }

    if has_winner {
        if winner_guess_count <= 3 {
            return -1;
        }
        if (winner_guess_count as f32) > (total_rounds as f32 / 2.0) {
            return 1;
        }
        return 0;
    }

    -1
}

pub fn calculate_nonstop_setter_score(
    has_big_winner: bool,
    big_winner_score: i32,
    winners_count: i32,
    total_players_count: i32,
) -> i32 {
    // Nonstop setter score scales by winner count and total active players.
    let total_players = std::cmp::max(1, total_players_count);
    let player_multiplier = std::cmp::max(1, ((total_players as f32) / 2.0).ceil() as i32);

    if has_big_winner {
        let penalty = std::cmp::max(1, big_winner_score / 2);
        return -penalty;
    }

    if winners_count == 0 {
        return -2 * player_multiplier;
    }

    let win_rate = winners_count as f32 / total_players as f32;
    let base_score = if win_rate <= 0.25 {
        1
    } else if win_rate >= 0.75 {
        1
    } else {
        2
    };

    base_score * player_multiplier
}
