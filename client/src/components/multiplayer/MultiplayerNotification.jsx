import Icon from '../Icon';

function notificationIcon(type) {
  if (type === 'host') return 'crown';
  if (type === 'reconnect') return 'wifi';
  if (type === 'warning') return 'exclamationTriangle';
  return 'exclamationCircle';
}

function MultiplayerNotification({ notification }) {
  if (!notification) {
    return null;
  }

  return (
    <div className={`kick-notification ${notification.type ? `${notification.type}-notification` : ''}`}>
      <div className="kick-notification-content">
        <Icon name={notificationIcon(notification.type)} />
        <span>{notification.message}</span>
      </div>
    </div>
  );
}

export default MultiplayerNotification;
