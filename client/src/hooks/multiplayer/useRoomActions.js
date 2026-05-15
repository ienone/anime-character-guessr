import { useCallback } from 'react';

function useRoomActions({
  roomId,
  roomUrl,
  roomName,
  setRoomName,
  setPlayers,
  socketRef,
  isHost,
  showNotification
}) {
  const copyRoomUrl = useCallback(async () => {
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(roomUrl);
        return;
      }
      const input = document.createElement('textarea');
      input.value = roomUrl;
      input.setAttribute('readonly', '');
      input.style.position = 'fixed';
      input.style.left = '-9999px';
      document.body.appendChild(input);
      input.select();
      input.setSelectionRange(0, input.value.length);
      const copied = document.execCommand('copy');
      document.body.removeChild(input);
      if (!copied) {
        throw new Error('copy command failed');
      }
    } catch (error) {
      console.error('复制失败:', error);
      showNotification('复制失败，请手动复制房间链接', 'error');
    }
  }, [roomUrl, showNotification]);

  const handleVisibilityToggle = useCallback(() => {
    socketRef.current?.emit('toggleRoomVisibility', { roomId });
  }, [roomId, socketRef]);

  const handleRoomNameChange = useCallback((event) => {
    setRoomName(event.target.value);
  }, [setRoomName]);

  const handleRoomNameBlur = useCallback(() => {
    if (!isHost || !socketRef.current) return;
    const trimmed = roomName.trim();
    if (trimmed !== roomName) {
      setRoomName(trimmed);
    }
    socketRef.current.emit('updateRoomName', { roomId, roomName: trimmed });
  }, [isHost, roomId, roomName, setRoomName, socketRef]);

  const handleRoomNameKeyDown = useCallback((event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      event.currentTarget.blur();
    }
  }, []);

  const handleMessageChange = useCallback((newMessage) => {
    setPlayers(prevPlayers => prevPlayers.map(p =>
      p.id === socketRef.current?.id ? { ...p, message: newMessage } : p
    ));
    socketRef.current?.emit('updatePlayerMessage', { roomId, message: newMessage });
  }, [roomId, setPlayers, socketRef]);

  const handleTeamChange = useCallback((playerId, newTeam) => {
    if (!socketRef.current) return;
    setPlayers(prevPlayers => prevPlayers.map(p =>
      p.id === playerId ? { ...p, team: newTeam || null } : p
    ));
    socketRef.current.emit('updatePlayerTeam', { roomId, team: newTeam || null });
  }, [roomId, setPlayers, socketRef]);

  return {
    copyRoomUrl,
    handleVisibilityToggle,
    handleRoomNameChange,
    handleRoomNameBlur,
    handleRoomNameKeyDown,
    handleMessageChange,
    handleTeamChange
  };
}

export default useRoomActions;
