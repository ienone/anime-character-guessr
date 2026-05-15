import { useEffect, useRef } from 'react';

function useMultiplayerSocketEvents(context) {
  const contextRef = useRef(context);
  const socket = context.socket;

  useEffect(() => {
    contextRef.current = context;
  }, [context]);

  useEffect(() => {
    const {
      socket,
      socketRef,
      roomIdRef,
      usernameRef,
      isJoinedRef,
      isManualDisconnectRef,
      isAutoReconnectingRef,
      reconnectAttemptsRef,
      pendingUpdatePlayersRef,
      updatePlayersFrameRef,
      latestPlayersRef,
      gameSettingsRef,
      gameEndedRef,
      answerCharacterRef,
      clearLatestPlayers,
      setLatestPlayers,
      refreshRoomListIfVisible,
      maxReconnectAttempts,
      navigate,
      showKickNotification,
      formatSocketError,
      getRoomJoinPayload,
      getAttemptCount,
      hasRoundResult,
      hasPendingGuess,
      rejectGuess,
      resolveGuess,
      setPlayers,
      setIsPublic,
      setAnswerSetterId,
      setIsJoined,
      setError,
      setIsHost,
      setGuessesLeft,
      setIsObserver,
      setCanShowSelectedAnswer,
      setRoomName,
      setWaitingForAnswer,
      setIsManualMode,
      setShowSetAnswerPopup,
      setSyncStatus,
      setWaitingForSync,
      setShouldResetTimer,
      setNonstopProgress,
      setBannedSharedTags,
      setConnectionStatus,
      setIsGameStarting,
      setGameEnd,
      setAnswerCharacter,
      setGameSettings,
      setGuessesHistory,
      setHints,
      setUseImageHint,
      setImgHint,
      setGlobalGameEnd,
      setEndGameSettings,
      setScoreDetails,
      setIsGameStarted,
      setGuesses,
      setIsAnswerSetter
    } = contextRef.current;

    if (!socket) {
      return undefined;
    }

    const newSocket = socket;
    clearLatestPlayers();
    const kickEventProcessed = {};

    const updateGuessesLeftFromPlayer = (player) => {
      if (!player || player.isAnswerSetter || player.team === '0') {
        return;
      }

      const used = getAttemptCount(player);
      const max = gameSettingsRef.current?.maxAttempts || 10;
      const left = Math.max(0, max - used);
      setGuessesLeft(left);

      if (player.roundResult === 'dead') {
        setIsObserver(true);
        setCanShowSelectedAnswer(true);
      }
    };

    const applyUpdatePlayers = ({ players, isPublic, answerSetterId }) => {
      setPlayers(players);
      setLatestPlayers(players);
      if (isPublic !== undefined) {
        setIsPublic(isPublic);
      }
      if (answerSetterId !== undefined) {
        setAnswerSetterId(answerSetterId);
      }
      const me = players.find(p => p.id === newSocket.id);
      if (me) {
        setIsJoined(true);
        setError('');
        setIsHost(me.isHost);
        if (me.team === '0') {
          setIsObserver(true);
        }
        updateGuessesLeftFromPlayer(me);
      }
    };

    newSocket.on('updatePlayers', (payload) => {
      pendingUpdatePlayersRef.current = payload;
      if (updatePlayersFrameRef.current !== null) return;
      updatePlayersFrameRef.current = window.requestAnimationFrame(() => {
        updatePlayersFrameRef.current = null;
        const nextPayload = pendingUpdatePlayersRef.current;
        pendingUpdatePlayersRef.current = null;
        if (nextPayload) {
          applyUpdatePlayers(nextPayload);
        }
      });
    });

    newSocket.on('playerPatched', ({ player }) => {
      if (!player?.id) return;
      setPlayers(prevPlayers => {
        const current = Array.isArray(prevPlayers) ? prevPlayers : [];
        const next = current.map(p => p.id === player.id ? { ...p, ...player } : p);
        setLatestPlayers(next);
        const me = next.find(p => p.id === newSocket.id);
        if (me) {
          setIsHost(me.isHost);
          if (me.team === '0') {
            setIsObserver(true);
          }
          updateGuessesLeftFromPlayer(me);
        }
        return next;
      });
    });

    newSocket.on('roomNameUpdated', ({ roomName: updatedRoomName }) => {
      setRoomName(updatedRoomName || '');
    });

    newSocket.on('roomsUpdated', () => {
      refreshRoomListIfVisible();
    });

    newSocket.on('waitForAnswer', ({ answerSetterId }) => {
      setWaitingForAnswer(true);
      setIsManualMode(false);
      if (answerSetterId) {
        setAnswerSetterId(answerSetterId);
      }
      if (answerSetterId === newSocket.id) {
        setShowSetAnswerPopup(true);
      }
    });

    newSocket.on('waitForAnswerCanceled', ({ message }) => {
      setWaitingForAnswer(false);
      setAnswerSetterId(null);
      setShowSetAnswerPopup(false);
      console.log(`[INFO] ${message}`);
      if (message) {
        showKickNotification(message, 'warning');
      }
    });

    newSocket.on('syncWaiting', ({ round, syncStatus, completedCount, totalCount }) => {
      setSyncStatus({ round, syncStatus, completedCount, totalCount });
      const myStatus = syncStatus?.find(p => p.id === newSocket.id);
      const iAmCompleted = myStatus?.completed || false;
      setWaitingForSync(iAmCompleted && completedCount < totalCount);
    });

    newSocket.on('syncRoundStart', ({ round }) => {
      setWaitingForSync(false);
      setSyncStatus(prevStatus => ({
        ...prevStatus,
        round,
        syncStatus: prevStatus.syncStatus?.map(p => ({ ...p, completed: false })) || []
      }));
      setShouldResetTimer(true);
      setTimeout(() => setShouldResetTimer(false), 100);
      console.log(`[同步模式] 第 ${round} 轮开始`);
    });

    newSocket.on('nonstopProgress', (progress) => {
      setNonstopProgress(progress);
      console.log(`[血战模式] 进度更新: ${progress.winners?.length || 0}人猜对，剩余${progress.remainingCount}人`);
    });

    newSocket.on('tagBanStateUpdate', ({ tagBanState = [] }) => {
      const normalizedState = Array.isArray(tagBanState) ? tagBanState : [];
      const me = latestPlayersRef.current.find(player => player?.id === newSocket.id);
      if (!me || me.isAnswerSetter || me.team === '0') {
        setBannedSharedTags([]);
        return;
      }

      const allowedIds = new Set([newSocket.id]);
      if (me.team && me.team !== '0' && me.team !== '' && me.team !== null && me.team !== undefined) {
        latestPlayersRef.current.forEach(player => {
          if (player && player.team === me.team) {
            allowedIds.add(player.id);
          }
        });
      }

      const banned = new Set();
      normalizedState.forEach(entry => {
        if (!entry || typeof entry.tag !== 'string') {
          return;
        }
        const tagName = entry.tag.trim();
        if (!tagName) {
          return;
        }
        const revealerIds = Array.isArray(entry.revealer) ? entry.revealer : [];
        const hasAccess = revealerIds.some(id => allowedIds.has(id));
        if (!hasAccess) {
          banned.add(tagName);
        }
      });
      setBannedSharedTags(Array.from(banned));
    });

    newSocket.on('connect', () => {
      console.log('[WebSocket] 连接成功');
      setConnectionStatus('connected');
      isAutoReconnectingRef.current = false;
      reconnectAttemptsRef.current = 0;

      if (isJoinedRef.current && roomIdRef.current && usernameRef.current) {
        newSocket.emit('joinRoom', getRoomJoinPayload(roomIdRef.current, usernameRef.current));
        newSocket.emit('requestGameSettings', { roomId: roomIdRef.current });
      }
    });

    newSocket.on('disconnect', (reason) => {
      console.log('[WebSocket] 连接断开:', reason);
      setCanShowSelectedAnswer(false);
      setIsGameStarting(false);

      if (isManualDisconnectRef.current) {
        setConnectionStatus('disconnected');
        return;
      }

      setConnectionStatus('reconnecting');
      isAutoReconnectingRef.current = true;

      if (reason === 'io server disconnect') {
        setConnectionStatus('failed');
        setIsGameStarting(false);
        showKickNotification('连接被服务器断开，请刷新页面或稍后再试', 'error');
        setError('连接失败，请刷新页面重试');
      }
    });

    newSocket.io.on('reconnect_attempt', (attempt) => {
      reconnectAttemptsRef.current = attempt;
      isAutoReconnectingRef.current = true;
      setConnectionStatus('reconnecting');
      console.log(`[WebSocket] 尝试重连 (${attempt}/${maxReconnectAttempts})...`);
    });

    newSocket.io.on('reconnect_failed', () => {
      reconnectAttemptsRef.current = maxReconnectAttempts;
      isAutoReconnectingRef.current = false;
      setConnectionStatus('failed');
      setIsGameStarting(false);
      showKickNotification('连接已断开，多次重试失败，请刷新页面或稍后再试', 'error');
      setError('连接失败，请刷新页面重试');
    });

    newSocket.on('connect_error', (error) => {
      console.error('[WebSocket] 连接错误:', error);

      if (!isManualDisconnectRef.current) {
        setConnectionStatus('reconnecting');
      }
    });

    newSocket.on('teamWin', ({ winnerName, message }) => {
      console.log(`[血战模式+同步模式] 队友猜对: ${winnerName}`);
      showKickNotification(message, 'info');
      setGameEnd(true);
      gameEndedRef.current = true;
    });

    newSocket.on('gameStart', ({ character, settings, players, isPublic, hints = null, isAnswerSetter: isAnswerSetterFlag }) => {
      setIsGameStarting(false);
      setCanShowSelectedAnswer(false);
      const visibleAnswer = character && character.id ? {
        ...character,
        rawTags: new Map(Object.entries(character.rawTags || {}))
      } : null;
      setAnswerCharacter(visibleAnswer);
      answerCharacterRef.current = visibleAnswer;
      setGameSettings(settings);

      const currentPlayer = players?.find(p => p.id === newSocket.id);
      const guessesMade = getAttemptCount(currentPlayer);
      const remainingGuesses = Math.max(0, (settings?.maxAttempts ?? 10) - guessesMade);
      setGuessesLeft(remainingGuesses);

      const observerFlag = currentPlayer?.team === '0';
      const hasGameEnded = hasRoundResult(currentPlayer);

      if (hasGameEnded) {
        gameEndedRef.current = true;
        setGameEnd(true);
      } else {
        gameEndedRef.current = false;
        setGameEnd(false);
      }

      const effectiveObserver = !!observerFlag || !!hasGameEnded;
      setIsObserver(effectiveObserver);

      setIsAnswerSetter(isAnswerSetterFlag);
      setCanShowSelectedAnswer(!!isAnswerSetterFlag || effectiveObserver);
      if (players) {
        setPlayers(players);
      }
      if (isPublic !== undefined) {
        setIsPublic(isPublic);
      }

      setGuessesHistory([]);

      let hintTexts = [];
      if (Array.isArray(settings?.useHints) && settings.useHints.length > 0 && hints) {
        hintTexts = hints;
      } else if (Array.isArray(settings?.useHints) && settings.useHints.length > 0 && visibleAnswer && visibleAnswer.summary) {
        const sentences = visibleAnswer.summary.replace('[mask]', '').replace('[/mask]','')
          .split(/[。、，。！？ ""]/).filter(s => s.trim());
        if (sentences.length > 0) {
          const selectedIndices = new Set();
          while (selectedIndices.size < Math.min(settings.useHints.length, sentences.length)) {
            selectedIndices.add(Math.floor(Math.random() * sentences.length));
          }
          hintTexts = Array.from(selectedIndices).map(i => "……"+sentences[i].trim()+"……");
        }
      }
      setHints(hintTexts);
      setUseImageHint(settings?.useImageHint ?? 0);
      setImgHint((settings?.useImageHint ?? 0) > 0 && visibleAnswer ? visibleAnswer.image : null);
      setGlobalGameEnd(false);
      setEndGameSettings(null);
      setScoreDetails(null);
      setIsGameStarted(true);
      setGuesses([]);

      if (settings?.syncMode) {
        const syncPlayers = players?.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected) || [];
        setSyncStatus({
          round: 1,
          syncStatus: syncPlayers.map(p => ({ id: p.id, username: p.username, completed: false })),
          completedCount: 0,
          totalCount: syncPlayers.length
        });
      } else {
        setWaitingForSync(false);
        setSyncStatus({});
      }
      if (settings?.nonstopMode) {
        const activePlayers = players?.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected) || [];
        setNonstopProgress({
          winners: [],
          remainingCount: activePlayers.length,
          totalCount: activePlayers.length
        });
      } else {
        setNonstopProgress(null);
      }
      setWaitingForAnswer(false);
      setAnswerSetterId(null);
      setShowSetAnswerPopup(false);
    });

    newSocket.on('guessHistoryUpdate', ({ guesses }) => {
      setGuessesHistory(guesses);
      const currentPlayer = latestPlayersRef.current.find(p => p.id === newSocket.id);
      if (currentPlayer) {
        updateGuessesLeftFromPlayer(currentPlayer);
      }
    });

    newSocket.on('guessAppended', ({ username, entry }) => {
      if (!username || !entry) return;
      setGuessesHistory(prev => {
        const next = Array.isArray(prev) ? [...prev] : [];
        const existingIndex = next.findIndex(item => item?.username === username);
        const sameEntry = guess => JSON.stringify(guess) === JSON.stringify(entry);
        if (existingIndex >= 0) {
          const guesses = Array.isArray(next[existingIndex].guesses)
            ? next[existingIndex].guesses
            : [];
          if (guesses.some(sameEntry)) return prev;
          next[existingIndex] = {
            ...next[existingIndex],
            guesses: [...guesses, entry]
          };
          return next;
        }
        return [...next, { username, guesses: [entry] }];
      });
    });

    newSocket.on('roomClosed', ({ message }) => {
      showKickNotification(message || '房主已断开连接，房间已关闭。', 'warning');
      setError('房间已关闭');
      navigate('/multiplayer/');
    });

    newSocket.on('hostTransferred', ({ oldHostName, newHostId, newHostName }) => {
      if (newHostId === newSocket.id) {
        setIsHost(true);
        if (oldHostName === newHostName) {
          showKickNotification(`原房主已断开连接，你已成为新房主！`, 'host');
        } else {
          showKickNotification(`房主 ${oldHostName} 已将房主权限转移给你！`, 'host');
        }
      } else {
        showKickNotification(`房主权限已从 ${oldHostName} 转移给 ${newHostName}`, 'host');
      }
    });

    newSocket.on('error', ({ message, event }) => {
      if (
        isAutoReconnectingRef.current &&
        isJoinedRef.current &&
        roomIdRef.current &&
        usernameRef.current &&
        typeof message === 'string' &&
        message.includes('换个名字吧')
      ) {
        setTimeout(() => {
          socketRef.current?.emit('joinRoom', getRoomJoinPayload(roomIdRef.current, usernameRef.current));
          socketRef.current?.emit('requestGameSettings', { roomId: roomIdRef.current });
        }, 500);
        return;
      }
      if (
        typeof message === 'string' &&
        message.startsWith('playerGuess:') &&
        hasPendingGuess()
      ) {
        rejectGuess(new Error(message));
        return;
      }

      const formattedMessage = formatSocketError(message, event);
      const eventName = event || (typeof message === 'string' ? message.match(/^([A-Za-z0-9_]+):/)?.[1] : '');
      if (eventName === 'gameStart') {
        setIsGameStarting(false);
      }
      showKickNotification(formattedMessage, 'error');
      setError(formattedMessage);
      if (
        typeof message === 'string' &&
        (message.startsWith('createRoom:') || message.startsWith('joinRoom:'))
      ) {
        setIsJoined(false);
      }
      if (message && message.includes('头像被用了😭😭😭')) {
        sessionStorage.removeItem('avatarId');
        sessionStorage.removeItem('avatarImage');
        setIsJoined(false);
        navigate('/multiplayer/');
      }
    });

    newSocket.on('serverShutdown', ({ message }) => {
      showKickNotification(message, 'error');
      setError(message);
      setIsJoined(false);
      setGameEnd(true);
      navigate('/multiplayer/');
    });

    newSocket.on('updateGameSettings', ({ settings }) => {
      setGameSettings(prevSettings => {
        if (JSON.stringify(prevSettings) === JSON.stringify(settings)) {
          return prevSettings;
        }
        return settings;
      });
    });

    newSocket.on('gameEnded', ({ guesses, scoreDetails, answerCharacter }) => {
      if (answerCharacter) {
        const revealedAnswer = {
          ...answerCharacter,
          rawTags: new Map(Object.entries(answerCharacter.rawTags || {}))
        };
        setAnswerCharacter(revealedAnswer);
        answerCharacterRef.current = revealedAnswer;
      }
      setEndGameSettings(gameSettingsRef.current);
      setScoreDetails(scoreDetails || null);
      setGlobalGameEnd(true);
      setGuessesHistory(guesses);
      setIsGameStarted(false);
      setIsGameStarting(false);
      setIsObserver(false);
      setIsAnswerSetter(false);
      setCanShowSelectedAnswer(false);
    });

    newSocket.on('answerReveal', ({ character }) => {
      if (!character) return;
      const revealedAnswer = {
        ...character,
        rawTags: new Map(Object.entries(character.rawTags || {}))
      };
      setAnswerCharacter(revealedAnswer);
      answerCharacterRef.current = revealedAnswer;
      setCanShowSelectedAnswer(true);
    });

    newSocket.on('guessResult', (payload) => {
      resolveGuess(payload);
    });

    newSocket.on('resetReadyStatus', () => {
      setPlayers(prevPlayers => prevPlayers.map(player => ({
        ...player,
        ready: player.isHost ? player.ready : false
      })));
    });

    newSocket.on('playerKicked', ({ playerId, username }) => {
      const eventId = `${playerId}-${Date.now()}`;
      if (kickEventProcessed[eventId]) return;
      kickEventProcessed[eventId] = true;

      if (playerId === newSocket.id) {
        showKickNotification('你已被房主踢出房间', 'kick');
        setIsJoined(false);
        setGameEnd(true);
        setTimeout(() => {
          navigate('/multiplayer/');
        }, 100);
      } else {
        showKickNotification(`玩家 ${username} 已被踢出房间`, 'kick');
        setPlayers(prevPlayers => prevPlayers.filter(p => p.id !== playerId));
      }
    });

    newSocket.on('boardcastTeamGuess', ({ guess, playerId, playerName }) => {
      if (!guess) return;
      setGuesses(prev => [...prev, {
        ...guess,
        playerId,
        playerName,
        guessrName: guess.guessrName || playerName
      }]);

      setPlayers(currentPlayers => {
        const currentPlayer = currentPlayers.find(p => p.id === newSocket.id);
        const isObserver = currentPlayer?.team === '0';
        const isAnswerSetterPlayer = currentPlayer?.isAnswerSetter;

        if (!isObserver && !isAnswerSetterPlayer) {
          setShouldResetTimer(true);
          setTimeout(() => setShouldResetTimer(false), 100);
        }

        return currentPlayers;
      });
    });

    newSocket.on('resetTimer', () => {
      setShouldResetTimer(true);
      setTimeout(() => setShouldResetTimer(false), 100);
    });

    return () => {
      isManualDisconnectRef.current = true;

      newSocket.io.off('reconnect_attempt');
      newSocket.io.off('reconnect_failed');
      newSocket.off('playerKicked');
      newSocket.off('hostTransferred');
      newSocket.off('updatePlayers');
      newSocket.off('playerPatched');
      newSocket.off('waitForAnswer');
      newSocket.off('waitForAnswerCanceled');
      newSocket.off('gameStart');
      newSocket.off('guessHistoryUpdate');
      newSocket.off('guessAppended');
      if (updatePlayersFrameRef.current !== null) {
        window.cancelAnimationFrame(updatePlayersFrameRef.current);
        updatePlayersFrameRef.current = null;
      }
      pendingUpdatePlayersRef.current = null;
      newSocket.off('roomClosed');
      newSocket.off('error');
      newSocket.off('serverShutdown');
      newSocket.off('updateGameSettings');
      newSocket.off('gameEnded');
      newSocket.off('answerReveal');
      newSocket.off('guessResult');
      newSocket.off('resetReadyStatus');
      newSocket.off('boardcastTeamGuess');
      newSocket.off('resetTimer');
      newSocket.off('syncWaiting');
      newSocket.off('syncRoundStart');
      newSocket.off('nonstopProgress');
      newSocket.off('teamWin');
      newSocket.off('roomNameUpdated');
      newSocket.off('roomsUpdated');
      newSocket.off('tagBanStateUpdate');
      newSocket.off('connect');
      newSocket.off('disconnect');
      newSocket.off('connect_error');
      clearLatestPlayers();
      setBannedSharedTags([]);
    };
  }, [socket]);
}

export default useMultiplayerSocketEvents;
