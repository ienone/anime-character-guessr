export const APP_NOTIFICATION_EVENT = 'anime-character-guessr:notify';

export function notify(message, type = 'info') {
  if (!message) {
    return;
  }

  if (typeof window === 'undefined') {
    return;
  }

  window.dispatchEvent(new CustomEvent(APP_NOTIFICATION_EVENT, {
    detail: {
      id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
      message,
      type
    }
  }));
}
