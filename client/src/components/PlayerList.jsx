import React, { useState, useEffect } from 'react';
import Image from './Image';

const attemptDisplay = {
  timeout: '⏱️',
  partial: '💡',
  correct: '✔',
  wrong: '❌'
};

const resultDisplay = {
  win: '✌',
  bigWin: '👑',
  dead: '💀',
  surrender: '🏳️',
  teamWin: '🏆'
};

function renderProgress(player) {
  const attempts = Array.isArray(player.attemptMarks) ? player.attemptMarks : [];
  return `${attempts.map(mark => attemptDisplay[mark] || '').join('')}${resultDisplay[player.roundResult] || ''}`;
}

const PlayerList = ({ players, socket, isGameStarted, handleReadyToggle, onAnonymousModeChange, isManualMode, isHost, answerSetterId, onSetAnswerSetter, onKickPlayer, onTransferHost, onMessageChange, onTeamChange }) => {
  const [showNames, setShowNames] = useState(true);
  const [waitingForAnswer, setWaitingForAnswer] = useState(false);
  const [activeMenu, setActiveMenu] = useState(null);
  const [editingMessagePlayerId, setEditingMessagePlayerId] = useState(null);
  const [messageDraft, setMessageDraft] = useState("");

  const teamOptions = [
    { value: '', label: '无' },
    { value: '0', label: '旁观' },
    ...Array.from({ length: 8 }, (_, i) => ({ value: (i + 1).toString(), label: (i + 1).toString() }))
  ];

  // Add socket event listener for waitForAnswer
 useEffect(() => {
    if (socket) {
      socket.on('waitForAnswer', () => {
        setWaitingForAnswer(true);
      });

      // Reset waiting state when game starts
      socket.on('gameStart', () => {
        setWaitingForAnswer(false);
      });
    }
  }, [socket]);

  // Add click outside handler to close menu
  useEffect(() => {
    function handleClickOutside(event) {
      if (activeMenu && !event.target.closest('.player-actions')) {
        setActiveMenu(null);
      }
    }
    
    document.addEventListener('mousedown', handleClickOutside);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, [activeMenu]);

  const handleShowNamesToggle = () => {
    const newShowNames = !showNames;
    setShowNames(newShowNames);
    if (onAnonymousModeChange) {
      onAnonymousModeChange(newShowNames);
    }
  };

  const getStatusDisplay = (player) => {
    const host = <span><i className={`fas fa-crown`}></i>房主</span>
    if (player.disconnected) {
      return renderStyledSpan('已断开','red');
    }

    // Use answerSetterId prop as the primary source of truth for "waiting for answer" state
    // This ensures late joiners or refreshed clients see the correct status
    if ((waitingForAnswer || answerSetterId) && !isGameStarted) {
      if (player.id === answerSetterId) {
        return renderStyledSpan('出题中','orange');
      }
      if (player.isHost) {
        return host;
      }
      return renderStyledSpan('已准备','green');
    }

    if (isManualMode && !isGameStarted && isHost) {
      if (player.id === answerSetterId) {
        return <button className="ready-button ready">出题中</button>;
      }
      return <button className="ready-button">选择</button>;
    }

    if (player.isHost) {
      return host;
    }

    if (player.id === socket?.id && !isGameStarted) {
      return (
        <button 
          onClick={handleReadyToggle}
          className={`ready-button ${player.ready ? 'ready' : ''}`}
        >
          {player.ready ? '取消准备' : '准备'}
        </button>
      );
    }

    return player.ready ? renderStyledSpan('已准备','green') : renderStyledSpan('未准备');
  };

  const renderStyledSpan = (text, color = "inherit") => (
    <span style={{ color }}>{text}</span>
  );

  const handlePlayerClick = (player) => {
    if (isHost && isManualMode && !isGameStarted && !waitingForAnswer) {
      onSetAnswerSetter(player.id);
    }
  };

  const handleKickClick = (e, playerId) => {
    e.stopPropagation(); // 阻止事件冒泡，防止触发行点击事件
    if (onKickPlayer) {
      onKickPlayer(playerId);
    }
  };

  const handleTransferHostClick = (e, playerId) => {
    e.stopPropagation(); // 阻止事件冒泡，防止触发行点击事件
    if (onTransferHost) {
      onTransferHost(playerId);
    }
  };

  const handleTeamChange = (playerId, newTeam) => {
    if (onTeamChange) {
      onTeamChange(playerId, newTeam);
    }
  };

  return (
    <div className="players-list">
      <table className="score-table">
        <thead>
          <tr>
            <th></th>
            <th>队</th>
            <th></th>
            <th>
              <button className='table-head-name-button'
                onClick={handleShowNamesToggle}>
                {showNames ? '名' : '无名' }
              </button>
            </th>
            <th>分</th>
            <th>猜</th>
            {isHost && <th><span style={{ width: "100px",display:"block" }}>操作</span></th>}
          </tr>
        </thead>
        <tbody>
          {players.map((player) => (
            <tr 
              key={player.id}
              onClick={() => handlePlayerClick(player)}
              style={{
                cursor: isHost && isManualMode && !isGameStarted && !waitingForAnswer ? 'pointer' : 'default'
              }}
            >
              <td>
                {getStatusDisplay(player)}
              </td>
              <td>
                {socket?.id === player.id && !player.ready && !isGameStarted ? (
                  <select
                    value={player.team || ''}
                    onChange={e => handleTeamChange(player.id, e.target.value)}
                    style={{ minWidth: '40px', background: 'inherit', color: 'inherit' }}
                  >
                    {teamOptions.map(opt => (
                      <option key={opt.value} value={opt.value}>{opt.label}</option>
                    ))}
                  </select>
                ) : (
                  <span>{player.team === '0' ? '旁观' : (player.team ? player.team : '无')}</span>
                )}
              </td>
              <td>
                {player.avatarId && player.avatarImage && (
                  <Image 
                    src={player.avatarImage} 
                    className="player-avatar" 
                    alt={player.memo ? player.memo : player.avatarName ? player.avatarName : player.username || 'avatar'} />
                )}
              </td>
              <td>
                {socket?.id === player.id && editingMessagePlayerId === player.id ? (
                  <input
                    type="text"
                    value={messageDraft}
                    placeholder='请友好交流（比心）'
                    autoFocus
                    maxLength={15}
                    style={{ width: '90%' }}
                    onChange={e => setMessageDraft(e.target.value)}
                    onBlur={() => {
                      setEditingMessagePlayerId(null);
                      if (onMessageChange) onMessageChange(messageDraft);
                    }}
                    onKeyDown={e => {
                      if (e.key === 'Enter') {
                        setEditingMessagePlayerId(null);
                        if (onMessageChange) onMessageChange(messageDraft);
                      }
                    }}
                  />
                ) : (
                  <span
                    style={{
                      backgroundColor: !showNames && player.id !== socket?.id ? '#000' : 'transparent',
                      color: !showNames && player.id !== socket?.id ? '#000' : 'inherit',
                      padding: !showNames && player.id !== socket?.id ? '2px 4px' : '0',
                      cursor: socket?.id === player.id ? 'pointer' : 'default',
                    }}
                    onClick={() => {
                      if (socket?.id === player.id) {
                        setEditingMessagePlayerId(player.id);
                        setMessageDraft(player.message || "");
                      }
                    }}
                  >
                    {player.username}
                    {player.message && (
                      <span>
                        : "{player.message}"
                      </span>
                    )}
                  </span>
                )}
              </td>
              <td>{player.score}</td>
              <td>{isGameStarted && player.isAnswerSetter ? '出题者' : renderProgress(player)}</td>
              {isHost && player.id !== socket?.id && (
                <td>
                  <div className="player-actions" style={{ position: 'relative' }}>
                    <button 
                      className="action-menu-button"
                      onClick={(e) => {
                        e.stopPropagation();
                        // 切换显示该玩家的操作菜单
                        setActiveMenu(activeMenu === player.id ? null : player.id);
                      }}
                    >
                      ⚙️ 操作
                    </button>
                    
                    {activeMenu === player.id && (
                      <div className="action-dropdown">
                        <button className='action-button'
                          onClick={(e) => {
                            e.stopPropagation();
                            handleKickClick(e, player.id);
                            setActiveMenu(null);
                          }}
                          >
                          <span>❌</span> 踢出
                        </button>
                        
                        {!player.disconnected && (
                          <button className='action-button'
                            onClick={(e) => {
                              e.stopPropagation();
                              handleTransferHostClick(e, player.id);
                              setActiveMenu(null);
                            }}
                            style={{ color: '#007bff' , borderBottom: '0px' }}
                            >
                            <span>👑</span> 转移房主
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
};

export default PlayerList; 
