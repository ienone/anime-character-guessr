import { useCallback, useEffect, useRef, useState } from 'react';

function useTimedNotification(defaultDuration = 5000) {
  const [notification, setNotification] = useState(null);
  const timerRef = useRef(null);

  const clearNotification = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    setNotification(null);
  }, []);

  const showNotification = useCallback((message, type = 'info', duration = defaultDuration) => {
    if (!message) {
      return;
    }
    if (timerRef.current) {
      clearTimeout(timerRef.current);
    }
    setNotification({ message, type });
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      setNotification(null);
    }, duration);
  }, [defaultDuration]);

  useEffect(() => () => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  return {
    notification,
    showNotification,
    clearNotification
  };
}

export default useTimedNotification;
