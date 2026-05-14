import { useEffect, useRef, useState } from 'react';
import { io } from 'socket.io-client';

function useMultiplayerSocket(socketUrl) {
  const [socket, setSocket] = useState(null);
  const socketRef = useRef(null);

  useEffect(() => {
    const nextSocket = io(socketUrl);
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

  return { socket, socketRef };
}

export default useMultiplayerSocket;
