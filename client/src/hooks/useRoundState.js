import { useCallback, useEffect, useRef } from 'react';

function useRoundState({ gameSettings }) {
  const gameSettingsRef = useRef(gameSettings);
  const latestPlayersRef = useRef([]);

  useEffect(() => {
    gameSettingsRef.current = gameSettings;
  }, [gameSettings]);

  const setLatestPlayers = useCallback((players) => {
    latestPlayersRef.current = Array.isArray(players) ? players : [];
  }, []);

  const clearLatestPlayers = useCallback(() => {
    latestPlayersRef.current = [];
  }, []);

  return {
    gameSettingsRef,
    latestPlayersRef,
    setLatestPlayers,
    clearLatestPlayers
  };
}

export default useRoundState;
