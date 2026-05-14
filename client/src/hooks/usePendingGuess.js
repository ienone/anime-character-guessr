import { useCallback, useEffect, useRef } from 'react';

function usePendingGuess(timeoutMs = 10000) {
  const resolverRef = useRef(null);
  const rejectRef = useRef(null);
  const timeoutRef = useRef(null);

  const clearPending = useCallback(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
      timeoutRef.current = null;
    }
    resolverRef.current = null;
    rejectRef.current = null;
  }, []);

  const submitGuess = useCallback(({ socket, roomId, characterId }) => {
    return new Promise((resolve, reject) => {
      clearPending();
      timeoutRef.current = setTimeout(() => {
        clearPending();
        reject(new Error('猜测响应超时'));
      }, timeoutMs);

      resolverRef.current = (payload) => {
        clearPending();
        resolve(payload);
      };
      rejectRef.current = (error) => {
        clearPending();
        reject(error);
      };

      socket?.emit('playerGuess', {
        roomId,
        characterId
      });
    });
  }, [clearPending, timeoutMs]);

  const resolveGuess = useCallback((payload) => {
    resolverRef.current?.(payload);
  }, []);

  const rejectGuess = useCallback((error) => {
    rejectRef.current?.(error);
  }, []);

  const hasPendingGuess = useCallback(() => Boolean(rejectRef.current), []);

  useEffect(() => clearPending, [clearPending]);

  return {
    submitGuess,
    resolveGuess,
    rejectGuess,
    hasPendingGuess
  };
}

export default usePendingGuess;
