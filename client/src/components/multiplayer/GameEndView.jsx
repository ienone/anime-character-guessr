import GameSettingsDisplay from '../GameSettingsDisplay';
import GuessHistoryTable from './GuessHistoryTable';
import HostRoomControls from './HostRoomControls';

function isWinnerResult(result) {
  return result === 'win' || result === 'bigWin' || result === 'teamWin';
}

function ScoreDetails({ scoreDetails, showNames }) {
  if (!scoreDetails || scoreDetails.length === 0) return null;

  const sortedDetails = scoreDetails
    .filter(item => item.type !== 'setter')
    .sort((a, b) => {
      const scoreA = a.type === 'team' ? a.teamScore : a.score;
      const scoreB = b.type === 'team' ? b.teamScore : b.score;
      return scoreB - scoreA;
    });

  return (
    <div className="score-details-list">
      {sortedDetails.map((item, idx) => {
        const rank = idx + 1;
        if (item.type === 'team') {
          const scoreText = item.teamScore >= 0 ? `+${item.teamScore}分` : `${item.teamScore}分`;
          const scoreClass = item.teamScore > 0 ? 'positive' : item.teamScore < 0 ? 'negative' : '';
          const boxClass = item.teamScore > 0 ? 'player-score-box positive' : item.teamScore < 0 ? 'player-score-box negative' : 'player-score-box';
          const memberDetails = item.members.map((m, mIdx) => {
            const memberScore = m.score >= 0 ? `+${m.score}` : `${m.score}`;
            const reasonParts = [];
            if (m.breakdown?.base) reasonParts.push(`基础${m.breakdown.base > 0 ? '+' : ''}${m.breakdown.base}`);
            if (m.breakdown?.bigWin) reasonParts.push(`大赢家+${m.breakdown.bigWin}`);
            if (m.breakdown?.quickGuess) reasonParts.push(`好快的猜+${m.breakdown.quickGuess}`);
            if (m.breakdown?.partial) reasonParts.push(`作品分+${m.breakdown.partial}`);
            const reasonText = reasonParts.length > 0 ? `(${reasonParts.join(' ')})` : '';
            const displayName = showNames ? m.username : `成员${mIdx + 1}`;
            return `${displayName}${memberScore}${reasonText}`;
          }).join(' ');

          return (
            <span key={`team-${item.teamId}`} className={boxClass}>
              <span className="player-rank">{rank}.</span>
              <span className="player-name">{showNames ? `队伍${item.teamId}` : `队伍${rank}`}</span>
              <span className={`score-value ${scoreClass}`}>{scoreText}</span>
              {memberDetails && <span className="score-breakdown">{memberDetails}</span>}
            </span>
          );
        }

        const scoreText = item.score >= 0 ? `+${item.score}分` : `${item.score}分`;
        const scoreClass = item.score > 0 ? 'positive' : item.score < 0 ? 'negative' : '';
        const boxClass = item.score > 0 ? 'player-score-box positive' : item.score < 0 ? 'player-score-box negative' : 'player-score-box';
        const breakdownParts = [];
        if (item.breakdown?.base) breakdownParts.push(`基础${item.breakdown.base > 0 ? '+' : ''}${item.breakdown.base}`);
        if (item.breakdown?.bigWin) breakdownParts.push(`大赢家+${item.breakdown.bigWin}`);
        if (item.breakdown?.quickGuess) breakdownParts.push(`好快的猜+${item.breakdown.quickGuess}`);
        if (item.breakdown?.partial) breakdownParts.push(`作品分+${item.breakdown.partial}`);
        const breakdownText = breakdownParts.length > 0 ? breakdownParts.join(' ') : '';

        return (
          <span key={item.id || idx} className={boxClass}>
            <span className="player-rank">{rank}.</span>
            <span className="player-name">{showNames ? item.username : `玩家${rank}`}</span>
            <span className={`score-value ${scoreClass}`}>{scoreText}</span>
            {breakdownText && <span className="score-breakdown">{breakdownText}</span>}
          </span>
        );
      })}
    </div>
  );
}

function SetterScore({ scoreDetails, showNames }) {
  const setterInfo = scoreDetails?.find(item => item.type === 'setter');
  if (!setterInfo) return null;

  const scoreText = setterInfo.score >= 0 ? `+${setterInfo.score}分` : `${setterInfo.score}分`;
  const boxClass = setterInfo.score > 0 ? 'player-score-box positive' : setterInfo.score < 0 ? 'player-score-box negative' : 'player-score-box';
  const scoreClass = setterInfo.score > 0 ? 'positive' : setterInfo.score < 0 ? 'negative' : '';

  return (
    <span className="setter-info-inline">
      ，出题人
      <span className={boxClass}>
        <span className="player-name">{showNames ? setterInfo.username : '**'}</span>
        <span className={`score-value ${scoreClass}`}>{scoreText}</span>
        {setterInfo.reason && <span className="score-breakdown">{setterInfo.reason}</span>}
      </span>
    </span>
  );
}

function ModeTags({ settings }) {
  return (
    <div className="mode-tags">
      {!settings.nonstopMode && !settings.syncMode && !settings.globalPick && !settings.tagBan && (
        <span className="mode-tag normal">普通模式</span>
      )}
      {settings.nonstopMode && <span className="mode-tag nonstop">血战模式</span>}
      {settings.syncMode && <span className="mode-tag sync">同步模式</span>}
      {settings.globalPick && <span className="mode-tag global-bp">角色全局BP</span>}
      {settings.tagBan && <span className="mode-tag tag-ban">标签全局BP</span>}
    </div>
  );
}

function AnswerButton({ players, socketId, answerCharacter, onShowCharacter }) {
  const currentPlayer = players.find(p => p.id === socketId);
  const isObserver = currentPlayer?.team === '0';
  const isCurrentPlayerWin = isWinnerResult(currentPlayer?.roundResult);
  const isCurrentPlayerLose = Boolean(currentPlayer?.roundResult) && !isCurrentPlayerWin;
  let answerButtonClass = 'answer-character-button';

  if (!isObserver && isCurrentPlayerWin) {
    answerButtonClass = 'answer-character-button win';
  } else if (!isObserver && isCurrentPlayerLose) {
    answerButtonClass = 'answer-character-button lose';
  }

  return (
    <button className={answerButtonClass} onClick={onShowCharacter}>
      {answerCharacter.nameCn || answerCharacter.name}
    </button>
  );
}

function GameEndView({
  isHost,
  isPublic,
  roomName,
  roomUrl,
  onRoomNameChange,
  onRoomNameBlur,
  onRoomNameKeyDown,
  onCopyRoomUrl,
  onOpenSettings,
  onToggleVisibility,
  onStartGame,
  onManualMode,
  isManualMode,
  hostControlsDisabled,
  displaySettings,
  answerCharacter,
  players,
  socketId,
  scoreDetails,
  showNames,
  onShowCharacter,
  guessesHistory
}) {
  if (!answerCharacter) return null;

  return (
    <div className="game-end-view-container">
      {isHost && (
        <HostRoomControls
          isPublic={isPublic}
          roomName={roomName}
          roomUrl={roomUrl}
          onRoomNameChange={onRoomNameChange}
          onRoomNameBlur={onRoomNameBlur}
          onRoomNameKeyDown={onRoomNameKeyDown}
          onCopyRoomUrl={onCopyRoomUrl}
          onOpenSettings={onOpenSettings}
          onToggleVisibility={onToggleVisibility}
          onStartGame={onStartGame}
          onManualMode={onManualMode}
          isManualMode={isManualMode}
          disabled={hostControlsDisabled}
        />
      )}
      <div className="game-end-message-table-wrapper">
        <table className="game-end-message-table">
          <thead>
            <tr>
              <th className="game-end-header-cell">
                <div className="game-end-header-content">
                  <ModeTags settings={displaySettings} />
                  <span className="answer-label">答案是</span>
                  <AnswerButton
                    players={players}
                    socketId={socketId}
                    answerCharacter={answerCharacter}
                    onShowCharacter={onShowCharacter}
                  />
                  <SetterScore scoreDetails={scoreDetails} showNames={showNames} />
                  {scoreDetails && scoreDetails.length > 0 && (
                    <span className="score-details-title">，得分详情：</span>
                  )}
                </div>
              </th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td className="game-end-body-cell">
                <ScoreDetails scoreDetails={scoreDetails} showNames={showNames} />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div className="game-end-container">
        {!isHost && <GameSettingsDisplay settings={displaySettings} />}
        <GuessHistoryTable guessesHistory={guessesHistory} showNames={showNames} />
      </div>
    </div>
  );
}

export default GameEndView;
