function HostWaitingControls({ onCancel }) {
  return (
    <div className="host-game-controls waiting-answer-controls">
      <span className="waiting-answer-message">
        正在等待出题人提交答案
      </span>
      <button
        type="button"
        onClick={onCancel}
        className="manual-mode-button cancel-wait-button"
      >
        取消等待
      </button>
    </div>
  );
}

export default HostWaitingControls;
