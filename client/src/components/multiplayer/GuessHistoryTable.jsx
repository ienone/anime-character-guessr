import Image from '../Image';

function buildDisplayRows(guessesHistory, collapsedCount) {
  const history = Array.isArray(guessesHistory) ? guessesHistory : [];
  const displayData = history.map(playerGuesses => {
    const guesses = Array.isArray(playerGuesses.guesses) ? playerGuesses.guesses : [];
    const startIdx = collapsedCount > 0 ? Math.max(0, guesses.length - collapsedCount) : 0;
    return {
      username: playerGuesses.username,
      displayGuesses: guesses.slice(startIdx)
    };
  });
  const maxDisplayRows = Math.max(...displayData.map(d => d.displayGuesses.length), 0);
  return { displayData, maxDisplayRows };
}

function GuessHistoryTable({ guessesHistory, showNames, collapsedCount = 0 }) {
  const history = Array.isArray(guessesHistory) ? guessesHistory : [];
  const { displayData, maxDisplayRows } = buildDisplayRows(history, collapsedCount);

  return (
    <div className="guess-history-table">
      <table>
        <thead>
          <tr>
            {history.map((playerGuesses, index) => (
              <th key={playerGuesses.username}>
                {showNames ? playerGuesses.username : `玩家${index + 1}`}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {Array.from({ length: maxDisplayRows }).map((_, rowIndex) => (
            <tr key={rowIndex}>
              {displayData.map(playerData => {
                const guess = playerData.displayGuesses[rowIndex];
                return (
                  <td key={playerData.username}>
                    {guess && (
                      <>
                        <Image className="character-icon" src={guess.guessData.image} alt={guess.guessData.name} />
                        <div className="character-name">{guess.guessData.name}</div>
                        <div className="character-name-cn">{guess.guessData.nameCn}</div>
                      </>
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export default GuessHistoryTable;
