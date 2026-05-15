import Icon from '../Icon';

function ConnectionStatusBanner({ isJoined, connectionStatus, reconnectAttempts, maxReconnectAttempts }) {
  if (!isJoined || connectionStatus === 'connected') {
    return null;
  }

  return (
    <div className={`connection-status ${connectionStatus}`}>
      <div className="connection-status-content">
        {connectionStatus === 'reconnecting' && (
          <>
            <Icon name="sync" className="app-icon-spin" />
            <span>连接断开，正在重连... ({reconnectAttempts}/{maxReconnectAttempts})</span>
          </>
        )}
        {connectionStatus === 'failed' && (
          <>
            <Icon name="exclamationTriangle" />
            <span>连接失败，请刷新页面重试</span>
          </>
        )}
        {connectionStatus === 'disconnected' && (
          <>
            <Icon name="exclamationCircle" />
            <span>连接已断开</span>
          </>
        )}
      </div>
    </div>
  );
}

export default ConnectionStatusBanner;
