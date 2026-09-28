import '../styles/game.css';
import Image from './Image';

function GameInfo({ gameEnd, guessesLeft, onRestart, finishInit, hints, useHints = [], onSurrender, imgHint=null, useImageHint=0, initFailed=false, isRestarting=false }) {
  return (
    <div className="game-info">
      {gameEnd ? (
        <button className="restart-button" onClick={onRestart} disabled={isRestarting}>
          {isRestarting ? '正在加载...' : '再玩一次'}
        </button>
      ) : (
        <div className="game-info-container">
          <div className="game-controls">
            <span>剩余次数: {guessesLeft}</span>
            {initFailed ? (
              <button className="restart-button" onClick={onRestart} disabled={isRestarting}>
                {isRestarting ? '正在加载...' : '重试'}
              </button>
            ) : (
              onSurrender && (
                <button disabled={!finishInit} className="surrender-button" onClick={onSurrender}>
                  投降 🏳️
                </button>
              )
            )}
          </div>
          {useHints && hints && useHints.map((val, idx) => (
            <div key={idx}>
              {guessesLeft <= val && hints[idx] && (
                <div className="hint-container">
                  <span className="hint-label">提示 {idx+1}:</span>
                  <span className="hint-text">{hints[idx]}</span>
                </div>
              )}
            </div>
          ))}
          {guessesLeft <= useImageHint && imgHint && (
            <div className="hint-container">
              <Image className="hint-image" src={imgHint} preferSource style={{height: '200px', filter: `blur(${guessesLeft}px)`}} alt="提示" />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default GameInfo;
