import { useCallback, useState } from 'react';

function useManualAnswerFlow({
  roomId,
  socketRef,
  isHost,
  gameSettings,
  setShowSetAnswerPopup,
  showNotification
}) {
  const [isManualMode, setIsManualMode] = useState(false);
  const [answerSetterId, setAnswerSetterId] = useState(null);
  const [waitingForAnswer, setWaitingForAnswer] = useState(false);

  const handleManualMode = useCallback(() => {
    if (isManualMode) {
      setAnswerSetterId(null);
      setIsManualMode(false);
    } else {
      if (isHost) {
        try {
          localStorage.setItem('latestMultiplayerSettings', JSON.stringify(gameSettings));
        } catch {
          // Ignore storage failures in privacy mode or restricted environments.
        }
      }
      socketRef.current?.emit('enterManualMode', { roomId });
      setIsManualMode(true);
    }
  }, [gameSettings, isHost, isManualMode, roomId, socketRef]);

  const handleSetAnswerSetter = useCallback((setterId) => {
    if (!isHost || !isManualMode) return;
    socketRef.current?.emit('setAnswerSetter', { roomId, setterId });
  }, [isHost, isManualMode, roomId, socketRef]);

  const handleCancelWaitForAnswer = useCallback(() => {
    socketRef.current?.emit('cancelWaitForAnswer', { roomId });
  }, [roomId, socketRef]);

  const handleSetAnswer = useCallback(async ({ character, hints }) => {
    try {
      const rawTags = Object.fromEntries(character.rawTags?.entries?.() || []);
      socketRef.current?.emit('setAnswer', {
        roomId,
        character: {
          ...character,
          rawTags
        },
        hints
      });
      setShowSetAnswerPopup(false);
    } catch (error) {
      console.error('Failed to set answer:', error);
      showNotification('设置答案失败，请重试', 'error');
    }
  }, [roomId, setShowSetAnswerPopup, showNotification, socketRef]);

  return {
    isManualMode,
    setIsManualMode,
    answerSetterId,
    setAnswerSetterId,
    waitingForAnswer,
    setWaitingForAnswer,
    handleManualMode,
    handleSetAnswerSetter,
    handleCancelWaitForAnswer,
    handleSetAnswer
  };
}

export default useManualAnswerFlow;
