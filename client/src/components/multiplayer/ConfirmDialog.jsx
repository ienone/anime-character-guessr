function ConfirmDialog({ dialog, onCancel }) {
  if (!dialog) {
    return null;
  }

  return (
    <div className="confirm-dialog-backdrop" role="presentation" onMouseDown={onCancel}>
      <div className="confirm-dialog" role="dialog" aria-modal="true" onMouseDown={event => event.stopPropagation()}>
        <div className="confirm-dialog-message">{dialog.message}</div>
        <div className="confirm-dialog-actions">
          <button className="secondary-btn" onClick={onCancel}>
            取消
          </button>
          <button
            className="primary-btn danger-btn"
            onClick={() => {
              onCancel();
              dialog.onConfirm?.();
            }}
          >
            确定
          </button>
        </div>
      </div>
    </div>
  );
}

export default ConfirmDialog;
