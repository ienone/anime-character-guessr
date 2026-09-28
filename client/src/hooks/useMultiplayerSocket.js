import { useEffect, useRef, useState } from 'react';
import { io } from 'socket.io-client';

const SOCKET_RECONNECTION_ATTEMPTS = 5;

function useMultiplayerSocket(socketUrl) {
  const [socket, setSocket] = useState(null);
  const socketRef = useRef(null);

  useEffect(() => {
    const nextSocket = io(socketUrl, {
      reconnection: true,
      reconnectionAttempts: SOCKET_RECONNECTION_ATTEMPTS,
      reconnectionDelay: 1000,
      reconnectionDelayMax: 3000
    });
    socketRef.current = nextSocket;
    setSocket(nextSocket);

    return () => {
      nextSocket.disconnect();
      if (socketRef.current === nextSocket) {
        socketRef.current = null;
      }
      setSocket(null);
    };
  }, [socketUrl]);

  return { socket, socketRef, maxReconnectAttempts: SOCKET_RECONNECTION_ATTEMPTS };
}

export default useMultiplayerSocket;
