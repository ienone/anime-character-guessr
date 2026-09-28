function HostRoomControls({
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
  isGameStarting = false,
  isManualMode,
  disabled,
  startLabel
}) {
  return (
    <>
      <div className="host-controls">
        <div className="room-url-container">
          {isPublic && (
            <input
              type="text"
              value={roomName}
              placeholder="房间名（可选）"
              maxLength={15}
              className="room-name-input"
              onChange={onRoomNameChange}
              onBlur={onRoomNameBlur}
              onKeyDown={onRoomNameKeyDown}
            />
          )}
          <input
            type="text"
            value={roomUrl}
            readOnly
            className="room-url-input"
          />
          <button onClick={onCopyRoomUrl} className="copy-button">复制</button>
        </div>
      </div>
      <div className="host-game-controls">
        <div className="button-group">
          <div className="button-row">
            <button onClick={onOpenSettings} className="settings-button">
              设置
            </button>
            <button onClick={onToggleVisibility} className="visibility-button">
              {isPublic ? '🔓公开' : '🔒私密'}
            </button>
            <button
              onClick={onStartGame}
              className="start-game-button"
              disabled={disabled || isGameStarting}
            >
              {startLabel || (isGameStarting ? '正在启动...' : '开始')}
            </button>
            <button
              onClick={onManualMode}
              className={`manual-mode-button ${isManualMode ? 'active' : ''}`}
              disabled={disabled}
            >
              有人想出题？
            </button>
          </div>
        </div>
      </div>
    </>
  );
}

export default HostRoomControls;
