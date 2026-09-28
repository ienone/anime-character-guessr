## Multiplayer Socket Manual Test Checklist (Rust server)

Run `server-rs` and open `client` in two browser windows (or incognito) to simulate 2 players.

### Basic room lifecycle
- Create room (player A): expect `updatePlayers`, `roomNameUpdated`.
- Join room (player B): both clients receive `updatePlayers` with 2 players.
- Toggle ready (player B): both clients see `ready` updated.
- Update room name (host): both clients see `roomNameUpdated`.
- Toggle visibility (host): both clients see `isPublic` updated in `updatePlayers`.

### Start a normal game
- Host presses start: all clients receive `gameStart` and then `tagBanStateUpdate`.
- Make a wrong guess: team/observers should see incremental `guessAppended` and `boardcastTeamGuess` per rules; `guessHistoryUpdate` is still used for snapshots/resync.
- Make a correct guess: trigger `gameEnd` client->server and verify `gameEnded` is broadcast.

### Reconnect snapshot
- While game is running, close player B tab and re-open/join with same username:
  - Expect immediate `updatePlayers` + `roomNameUpdated`
  - If game running: expect `gameStart` + `guessHistoryUpdate` + `tagBanStateUpdate` snapshot on join.

### Manual answer mode
- Host sets answer setter: expect `waitForAnswer` and `updatePlayers(answerSetterId=...)`.
- Setter submits `setAnswer`: all clients receive `gameStart` (setter gets `isAnswerSetter=true`).

### Sync / nonstop
- Enable `syncMode`: verify `syncWaiting` and `syncRoundStart` are emitted and round ends when all complete.
- Enable `nonstopMode`: verify `nonstopProgress` updates and `gameEnded` finalizes after observers enter.

