import { useCallback, useMemo } from 'react';

function useMultiplayerDerivedState({
  players,
  syncStatus,
  socketId,
  gameSettings,
  endGameSettings,
  globalGameEnd
}) {
  const allSpectators = useMemo(() => {
    if (!players || players.length === 0) return false;
    return players.every(p => p.disconnected || p.team === '0');
  }, [players]);

  const getFilteredSyncStatus = useCallback(() => {
    const statusList = syncStatus?.syncStatus || [];
    return statusList.filter((entry) => {
      const player = players.find(p => p.id === entry.id);
      const isDisconnected = !!player?.disconnected;
      return !(entry.completed && isDisconnected);
    });
  }, [players, syncStatus]);

  const displaySettings = useMemo(
    () => globalGameEnd ? (endGameSettings || gameSettings) : gameSettings,
    [endGameSettings, gameSettings, globalGameEnd]
  );

  const isTeamObserver = useMemo(() => {
    if (!socketId) return false;
    const me = players.find(p => p.id === socketId);
    return me?.team === '0';
  }, [players, socketId]);

  return {
    allSpectators,
    getFilteredSyncStatus,
    displaySettings,
    isTeamObserver
  };
}

export default useMultiplayerDerivedState;
