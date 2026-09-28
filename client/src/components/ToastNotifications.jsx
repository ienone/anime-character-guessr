import { useEffect, useState } from 'react';
import { APP_NOTIFICATION_EVENT } from '../utils/notifications';
import '../styles/ToastNotifications.css';

const TOAST_LIFETIME_MS = 3200;

function ToastNotifications() {
  const [items, setItems] = useState([]);

  useEffect(() => {
    const handleNotify = (event) => {
      const detail = event.detail || {};
      if (!detail.message) {
        return;
      }
      const item = {
        id: detail.id || `${Date.now()}-${Math.random().toString(36).slice(2)}`,
        message: detail.message,
        type: detail.type || 'info'
      };
      setItems(prev => [...prev, item].slice(-4));

      window.setTimeout(() => {
        setItems(prev => prev.filter(existing => existing.id !== item.id));
      }, TOAST_LIFETIME_MS);
    };

    window.addEventListener(APP_NOTIFICATION_EVENT, handleNotify);
    return () => window.removeEventListener(APP_NOTIFICATION_EVENT, handleNotify);
  }, []);

  if (items.length === 0) {
    return null;
  }

  return (
    <div className="toast-stack" role="status" aria-live="polite">
      {items.map(item => (
        <div key={item.id} className={`toast-message toast-${item.type}`}>
          {item.message}
        </div>
      ))}
    </div>
  );
}

export default ToastNotifications;
