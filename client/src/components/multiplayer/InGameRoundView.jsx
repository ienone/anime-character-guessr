import SearchBar from '../SearchBar';
import GuessesTable from '../GuessesTable';
import Timer from '../Timer';
import Image from '../Image';
import GuessHistoryTable from './GuessHistoryTable';

function SyncProgress({ enabled, syncStatus, getFilteredSyncStatus, showNames }) {
  if (!enabled) return null;

  const filtered = getFilteredSyncStatus();
  const completed = filtered.filter(p => p.completed).length;
  const total = filtered.length;

  return (
    <div className="sync-waiting-banner">
      <span>⏳ 同步模式 - 第 {syncStatus.round || 1} 轮 ({completed}/{total})</span>
      <div className="sync-status">
        {filtered.map((player, idx) => (
          <span key={player.id} className={`sync-player ${player.completed ? 'done' : 'waiting'}`}>
            {showNames ? player.username : `玩家${idx + 1}`}: {player.completed ? '✓' : '...'}
          </span>
        ))}
      </div>
    </div>
  );
}

function NonstopProgress({ enabled, nonstopProgress, players, showNames, anonymizeWinners = true }) {
  if (!enabled) return null;

  const activeCount = players.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected).length;
  return (
    <div className="nonstop-progress-banner">
      <span>🔥 血战模式 - 剩余 {nonstopProgress?.remainingCount ?? activeCount}/{nonstopProgress?.totalCount ?? activeCount} 人</span>
      {nonstopProgress?.winners && nonstopProgress.winners.length > 0 && (
        <div className="nonstop-winners">
          {nonstopProgress.winners.map((winner, idx) => (
            <span key={winner.username} className="nonstop-winner">
              #{winner.rank} {anonymizeWinners && !showNames ? `玩家${idx + 1}` : winner.username} (+{winner.score}分)
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function AnswerViewToolbar({
  answerViewMode,
  setAnswerViewMode,
  isObserver,
  isTeamObserver,
  isAnswerSetter,
  isGuessTableCollapsed,
  setIsGuessTableCollapsed
}) {
  return (
    <div className="answer-view-toolbar">
      <button
        className={`answer-view-button ${answerViewMode === 'simple' ? 'active' : ''}`}
        onClick={() => setAnswerViewMode('simple')}
      >
        {(isObserver && !isTeamObserver && !isAnswerSetter) ? '旁观' : '简单'}
      </button>
      <button
        className={`answer-view-button ${answerViewMode === 'detailed' ? 'active' : ''}`}
        onClick={() => setAnswerViewMode('detailed')}
      >
        {(isObserver && !isTeamObserver && !isAnswerSetter) ? '我的' : '详细'}
      </button>
      <div className="guess-collapse-control">
        <div
          className={`guess-collapse-toggle ${isGuessTableCollapsed ? 'active' : ''}`}
          onClick={() => setIsGuessTableCollapsed(!isGuessTableCollapsed)}
        >
          <div className="guess-collapse-thumb" />
        </div>
        <span className="guess-collapse-label">只显示最新3条</span>
      </div>
    </div>
  );
}

function PlayerRoundView({
  onCharacterSelect,
  isGuessing,
  waitingForSync,
  gameEnd,
  gameSettings,
  isGameStarted,
  syncStatus,
  getFilteredSyncStatus,
  showNames,
  nonstopProgress,
  players,
  onTimeUp,
  isObserver,
  isAnswerSetter,
  canShowSelectedAnswer,
  shouldResetTimer,
  guessesLeft,
  onSurrender,
  hints,
  useImageHint,
  imgHint,
  guesses,
  answerCharacter,
  bannedTags
}) {
  const hintBlur = Math.max(0, Math.min(15, Number(guessesLeft) || 0));

  return (
    <>
      <SearchBar
        onCharacterSelect={onCharacterSelect}
        isGuessing={isGuessing || waitingForSync}
        gameEnd={gameEnd}
        subjectSearch={gameSettings.subjectSearch}
        finishInit={isGameStarted}
      />
      <SyncProgress
        enabled={gameSettings.syncMode}
        syncStatus={syncStatus}
        getFilteredSyncStatus={getFilteredSyncStatus}
        showNames={showNames}
      />
      <NonstopProgress
        enabled={gameSettings.nonstopMode}
        nonstopProgress={nonstopProgress}
        players={players}
        showNames={showNames}
      />
      {gameSettings.timeLimit && !gameEnd && !waitingForSync && (
        <Timer
          timeLimit={gameSettings.timeLimit}
          onTimeUp={onTimeUp}
          isActive={!isGuessing && !waitingForSync && !isObserver && !isAnswerSetter && !canShowSelectedAnswer}
          reset={shouldResetTimer}
        />
      )}
      <div className="game-info">
        <div className="guesses-left">
          <span>剩余猜测次数: {guessesLeft}</span>
          <button
            className="surrender-button"
            onClick={onSurrender}
            disabled={isObserver || gameEnd}
          >
            投降 🏳️
          </button>
        </div>
        {Array.isArray(gameSettings.useHints) && gameSettings.useHints.length > 0 && hints && hints.length > 0 && (
          <div className="hints">
            {gameSettings.useHints.map((val, idx) => (
              guessesLeft <= val && hints[idx] && (
                <div className="hint" key={idx}>提示{idx + 1}: {hints[idx]}</div>
              )
            ))}
          </div>
        )}
        {guessesLeft <= useImageHint && imgHint && (
          <div className="hint-container">
            <Image src={imgHint} preferSource className={`image-hint hint-blur-${hintBlur}`} alt="提示" />
          </div>
        )}
      </div>
      <GuessesTable
        guesses={guesses}
        gameSettings={gameSettings}
        answerCharacter={answerCharacter}
        bannedTags={bannedTags}
      />
    </>
  );
}

function AnswerSetterRoundView({
  canShowSelectedAnswer,
  answerCharacter,
  isAnswerSetter,
  isTeamObserver,
  gameSettings,
  nonstopProgress,
  players,
  syncStatus,
  getFilteredSyncStatus,
  showNames,
  answerViewMode,
  setAnswerViewMode,
  isObserver,
  isGuessTableCollapsed,
  setIsGuessTableCollapsed,
  guessesHistory,
  guesses,
  bannedTags
}) {
  return (
    <div className="answer-setter-view">
      {canShowSelectedAnswer && answerCharacter && (isAnswerSetter || isTeamObserver) && (
        <div className="selected-answer">
          <Image src={answerCharacter.imageGrid} preferSource alt={answerCharacter.name} className="answer-image" />
          <div className="answer-info">
            <div>{answerCharacter.name}</div>
            <div>{answerCharacter.nameCn}</div>
          </div>
        </div>
      )}
      <NonstopProgress
        enabled={gameSettings.nonstopMode}
        nonstopProgress={nonstopProgress}
        players={players}
        showNames={showNames}
        anonymizeWinners={false}
      />
      <SyncProgress
        enabled={gameSettings.syncMode}
        syncStatus={syncStatus}
        getFilteredSyncStatus={getFilteredSyncStatus}
        showNames={showNames}
      />
      <AnswerViewToolbar
        answerViewMode={answerViewMode}
        setAnswerViewMode={setAnswerViewMode}
        isObserver={isObserver}
        isTeamObserver={isTeamObserver}
        isAnswerSetter={isAnswerSetter}
        isGuessTableCollapsed={isGuessTableCollapsed}
        setIsGuessTableCollapsed={setIsGuessTableCollapsed}
      />
      {answerViewMode === 'simple' ? (
        <GuessHistoryTable
          guessesHistory={guessesHistory}
          showNames={showNames}
          collapsedCount={isGuessTableCollapsed ? 3 : 0}
        />
      ) : (
        <div className="detailed-guesses-panel">
          <GuessesTable
            guesses={guesses}
            gameSettings={gameSettings}
            answerCharacter={answerCharacter}
            collapsedCount={isGuessTableCollapsed ? 3 : 0}
            bannedTags={bannedTags}
          />
        </div>
      )}
    </div>
  );
}

function InGameRoundView(props) {
  const {
    isAnswerSetter,
    isTeamObserver
  } = props;

  return (
    <div className="container">
      {!isAnswerSetter && !isTeamObserver ? (
        <PlayerRoundView {...props} />
      ) : (
        <AnswerSetterRoundView {...props} />
      )}
    </div>
  );
}

export default InGameRoundView;
