use serde_json::Value;

use crate::socket::state::{CharacterPayload, GuessEntry, Player, PlayerGuessHistory, Room};

#[derive(Debug, Clone)]
pub struct GuessCommand {
    pub actor_id: String,
    pub character: CharacterPayload,
    pub feedback: Value,
}

#[derive(Debug, Clone)]
pub struct GuessOutcome {
    pub player: Player,
    pub entry: GuessEntry,
    pub is_correct: bool,
    pub is_partial_correct: bool,
    pub sync_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuessReject {
    PlayerNotBound,
    GameNotRunning,
    Observer,
    AlreadyEnded,
    AttemptExhausted,
    GlobalPickDuplicate,
}

pub fn apply_guess(room: &mut Room, command: GuessCommand) -> Result<GuessOutcome, GuessReject> {
    let actor_id = command.actor_id;
    let guess_data = command.character;
    let character_id = guess_data.id;
    let player_idx = room
        .players
        .iter()
        .position(|p| p.id == actor_id)
        .ok_or(GuessReject::PlayerNotBound)?;

    if room.current_game.is_none() {
        return Err(GuessReject::GameNotRunning);
    }

    let player = room.players[player_idx].clone();
    if player.team.as_deref() == Some("0") || player.temp_observer {
        return Err(GuessReject::Observer);
    }
    if super::player_has_result(&player) {
        return Err(GuessReject::AlreadyEnded);
    }

    let is_correct = command
        .feedback
        .get("isCorrect")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_partial_correct = command
        .feedback
        .get("isPartialCorrect")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let pre_limit = super::enforce_attempt_limit(room, &actor_id, false);
    if pre_limit.exhausted {
        return Err(GuessReject::AttemptExhausted);
    }

    let (global_pick, sync_mode, nonstop_mode) = room
        .current_game
        .as_ref()
        .map(|game| {
            let settings = game.settings.as_ref();
            (
                settings.is_some_and(|s| s.global_pick),
                settings.is_some_and(|s| s.sync_mode),
                settings.is_some_and(|s| s.nonstop_mode),
            )
        })
        .unwrap_or((false, false, false));

    if global_pick && !sync_mode {
        let already = room
            .current_game
            .as_ref()
            .map(|game| {
                game.guesses.iter().any(|pg| {
                    let other = pg.username != player.username;
                    if !other {
                        return false;
                    }
                    pg.guesses
                        .iter()
                        .any(|guess| guess.guess_data.id == character_id)
                })
            })
            .unwrap_or(false);
        if already && (!nonstop_mode || !is_correct) {
            return Err(GuessReject::GlobalPickDuplicate);
        }
    }

    let entry = GuessEntry {
        player_id: actor_id.clone(),
        player_name: player.username.clone(),
        is_correct,
        is_partial_correct,
        guess_data,
    };

    if let Some(ref mut game) = room.current_game {
        let mut found = false;
        for pg in game.guesses.iter_mut() {
            if pg.username == player.username {
                pg.guesses.push(entry.clone());
                found = true;
                break;
            }
        }
        if !found {
            game.guesses.push(PlayerGuessHistory {
                username: player.username.clone(),
                guesses: vec![entry.clone()],
            });
        }
    }

    let mark = if !is_correct && is_partial_correct {
        super::ATTEMPT_PARTIAL
    } else if is_correct {
        super::ATTEMPT_CORRECT
    } else {
        super::ATTEMPT_WRONG
    };

    let is_team_mode = player.team.is_some() && player.team.as_deref() != Some("0");
    if is_team_mode {
        let team_id = player.team.clone().unwrap_or_default();
        let team_attempts = {
            let game = room.current_game.as_mut().expect("game checked above");
            super::push_team_attempt(game, &team_id, mark);
            game.team_attempt_marks
                .get(&team_id)
                .cloned()
                .unwrap_or_default()
        };

        for p in &mut room.players {
            if p.team.as_deref() == Some(&team_id) && !p.is_answer_setter && !p.disconnected {
                p.attempt_marks = team_attempts.clone();
            }
        }
    } else if let Some(p) = room.players.iter_mut().find(|p| p.id == actor_id) {
        super::push_player_attempt(p, mark);
    }

    if sync_mode && !is_correct {
        let teammate_ids_to_complete: Vec<String> = if is_team_mode {
            let team_id = player.team.clone().unwrap_or_default();
            room.players
                .iter()
                .filter(|p| {
                    p.team.as_deref() == Some(team_id.as_str())
                        && p.id != actor_id
                        && !p.is_answer_setter
                        && !p.disconnected
                })
                .map(|p| p.id.clone())
                .collect()
        } else {
            Vec::new()
        };

        if let Some(ref mut game) = room.current_game {
            game.sync_players_completed.insert(actor_id.clone());
            for id in teammate_ids_to_complete {
                game.sync_players_completed.insert(id);
            }
        }
    }

    if !nonstop_mode {
        let _ = super::enforce_attempt_limit(room, &actor_id, is_correct);
    }

    Ok(GuessOutcome {
        player,
        entry,
        is_correct,
        is_partial_correct,
        sync_mode,
    })
}

pub fn latest_guess_matches_answer(room: &Room, player_id: &str) -> bool {
    let Some(game) = room.current_game.as_ref() else {
        return false;
    };

    game.guesses
        .iter()
        .flat_map(|history| history.guesses.iter())
        .rev()
        .find(|guess| guess.player_id == player_id)
        .is_some_and(|guess| guess.is_correct && guess.guess_data.id == game.character.id)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::routes::game::GameSettings;
    use crate::socket::state::{CharacterPayload, CurrentGame, Player, Room};
    use serde_json::json;

    fn test_player() -> Player {
        Player {
            id: "socket-1".to_string(),
            stable_player_id: "stable-1".to_string(),
            username: "alice".to_string(),
            is_host: false,
            score: 0,
            ready: true,
            attempt_marks: Vec::new(),
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
        }
    }

    fn test_room() -> Room {
        Room {
            host: "host".to_string(),
            is_public: true,
            room_name: String::new(),
            players: vec![test_player()],
            last_active: 0,
            current_game: Some(CurrentGame {
                character: CharacterPayload::from_value(json!({ "id": 1, "name": "answer" }))
                    .unwrap(),
                settings: Some(GameSettings::default()),
                guesses: Vec::new(),
                team_attempt_marks: HashMap::new(),
                team_round_results: HashMap::new(),
                hints: None,
                sync_round: 1,
                sync_players_completed: HashSet::new(),
                sync_winner_found: false,
                sync_winner: None,
                sync_ready_to_end: false,
                sync_round_start_rank: 1,
                nonstop_winners: Vec::new(),
                nonstop_total_players: 0,
                first_winner: None,
                tag_ban_state: Vec::new(),
                tag_ban_state_pending: Vec::new(),
                _last_sync_waiting_key: None,
                _last_sync_waiting_at: 0,
            }),
            answer_setter_id: None,
            waiting_for_answer: false,
            settings: Some(GameSettings::default()),
            _last_players_broadcast_at: None,
            _pending_player_broadcast_extra: None,
            _player_broadcast_due_at: None,
            _player_broadcast_flush_scheduled: false,
        }
    }

    #[test]
    fn latest_guess_must_be_correct_answer_for_nonstop_settlement() {
        let mut room = test_room();
        assert!(!latest_guess_matches_answer(&room, "socket-1"));

        apply_guess(
            &mut room,
            GuessCommand {
                actor_id: "socket-1".to_string(),
                character: CharacterPayload::from_value(json!({ "id": 2, "name": "wrong" }))
                    .unwrap(),
                feedback: json!({ "isCorrect": false, "isPartialCorrect": false }),
            },
        )
        .unwrap();
        assert!(!latest_guess_matches_answer(&room, "socket-1"));

        apply_guess(
            &mut room,
            GuessCommand {
                actor_id: "socket-1".to_string(),
                character: CharacterPayload::from_value(json!({ "id": 1, "name": "answer" }))
                    .unwrap(),
                feedback: json!({ "isCorrect": true, "isPartialCorrect": false }),
            },
        )
        .unwrap();
        assert!(latest_guess_matches_answer(&room, "socket-1"));
    }

    #[test]
    fn records_server_loaded_character_id() {
        let mut room = test_room();
        let outcome = apply_guess(
            &mut room,
            GuessCommand {
                actor_id: "socket-1".to_string(),
                character: CharacterPayload::from_value(json!({ "id": 2, "name": "server guess" }))
                    .unwrap(),
                feedback: json!({ "isCorrect": false, "isPartialCorrect": false }),
            },
        )
        .unwrap();

        assert!(!outcome.is_correct);
        let guesses = &room.current_game.as_ref().unwrap().guesses;
        assert_eq!(guesses.len(), 1);
        assert_eq!(guesses[0].guesses[0].guess_data.id, 2);
    }

    #[test]
    fn records_server_loaded_guess_and_attempt_mark() {
        let mut room = test_room();
        let outcome = apply_guess(
            &mut room,
            GuessCommand {
                actor_id: "socket-1".to_string(),
                character: CharacterPayload::from_value(json!({ "id": 2, "name": "guess" }))
                    .unwrap(),
                feedback: json!({ "isCorrect": false, "isPartialCorrect": false }),
            },
        )
        .unwrap();

        assert!(!outcome.is_correct);
        assert_eq!(
            room.players[0].attempt_marks,
            vec![super::super::ATTEMPT_WRONG.to_string()]
        );
        let guesses = &room.current_game.as_ref().unwrap().guesses;
        assert_eq!(guesses.len(), 1);
        assert_eq!(guesses[0].guesses[0].guess_data.id, 2);
    }
}
