import { lazy, Suspense, useState, useEffect, useRef } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { v4 as uuidv4 } from 'uuid';
import PlayerList from '../components/PlayerList';
import RoomList from '../components/RoomList';
import GameSettingsDisplay from '../components/GameSettingsDisplay';
import Icon from '../components/Icon';
import ConfirmDialog from '../components/multiplayer/ConfirmDialog';
import ConnectionStatusBanner from '../components/multiplayer/ConnectionStatusBanner';
import GameEndView from '../components/multiplayer/GameEndView';
import HostRoomControls from '../components/multiplayer/HostRoomControls';
import HostWaitingControls from '../components/multiplayer/HostWaitingControls';
import InGameRoundView from '../components/multiplayer/InGameRoundView';
import MultiplayerLobby from '../components/multiplayer/MultiplayerLobby';
import MultiplayerNotification from '../components/multiplayer/MultiplayerNotification';
import useMultiplayerSocket from '../hooks/useMultiplayerSocket';
import useManualAnswerFlow from '../hooks/multiplayer/useManualAnswerFlow';
import useMultiplayerDerivedState from '../hooks/multiplayer/useMultiplayerDerivedState';
import useMultiplayerSocketEvents from '../hooks/multiplayer/useMultiplayerSocketEvents';
import useRoomActions from '../hooks/multiplayer/useRoomActions';
import useRoundReducer from '../hooks/multiplayer/useRoundReducer';
import usePendingGuess from '../hooks/usePendingGuess';
import useRoomLobby from '../hooks/useRoomLobby';
import useRoundState from '../hooks/useRoundState';
import useTimedNotification from '../hooks/useTimedNotification';
import { getRandomCharacter } from '../utils/bangumi';
import logCollector from '../utils/logCollector';
import '../styles/Multiplayer.css';
import '../styles/game.css';
import axios from 'axios';

const FeedbackPopup = lazy(() => import('../components/FeedbackPopup'));
const GameEndPopup = lazy(() => import('../components/GameEndPopup'));
const Leaderboard = lazy(() => import('../components/Leaderboard'));
const Roulette = lazy(() => import('../components/Roulette'));
const SetAnswerPopup = lazy(() => import('../components/SetAnswerPopup'));
const SettingsPopup = lazy(() => import('../components/SettingsPopup'));

const SOCKET_URL = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '');
const PLAYER_SESSION_KEY = 'animeGuessrPlayerSessionId';

const SOCKET_EVENT_LABELS = {
  createRoom: '创建房间失败',
  joinRoom: '加入房间失败',
  gameStart: '开始游戏失败',
  setAnswer: '设置答案失败',
  playerGuess: '提交猜测失败',
  nonstopWin: '血战模式结算失败',
  gameEnd: '结束游戏失败',
  enterObserverMode: '进入旁观失败',
  timeOut: '计时处理失败'
};

const SOCKET_ERROR_HINTS = [
  {
    test: message => message.includes('当前标签页的连接没有绑定到房间玩家'),
    hint: '常见原因：同一浏览器开了多个同名标签页，或旧标签页被服务端判定为当前玩家。'
  },
  {
    test: message => message.includes('旁观者'),
    hint: '如果你是游戏开始后加入的玩家，本局会自动作为旁观者，下一局才能参与猜测。'
  },
  {
    test: message => message.includes('名字已经在房间里'),
    hint: '同一个房间内用户名必须唯一；同设备多标签页也需要使用不同名字。'
  },
  {
    test: message => message.includes('游戏未开始') || message.includes('本轮已经结束'),
    hint: '请等待房主开始下一局，或刷新页面同步当前房间状态。'
  }
];

function formatSocketError(message, event) {
  const rawMessage = String(message || '未知错误');
  const match = rawMessage.match(/^([A-Za-z0-9_]+):\s*(.*)$/);
  const eventName = event || match?.[1] || '';
  const detail = match?.[2] || rawMessage;
  const title = SOCKET_EVENT_LABELS[eventName] || '操作失败';
  const hint = SOCKET_ERROR_HINTS.find(item => item.test(detail))?.hint;
  return hint ? `${title}: ${detail}\n${hint}` : `${title}: ${detail}`;
}

function describeGuessError(error) {
  const message = error?.message || '提交猜测失败';
  if (message.includes('猜测响应超时')) {
    return '提交猜测超时：服务端没有在 10 秒内返回结果。\n可能是连接断开、房间状态已变化，或当前标签页不再是房间内的有效玩家。请刷新页面后重试。';
  }
  if (message.startsWith('playerGuess:')) {
    return formatSocketError(message, 'playerGuess');
  }
  if (message.includes('Network Error')) {
    return '获取角色登场信息失败：无法连接服务器。请确认后端仍在运行，且前端访问地址和服务器地址一致。';
  }
  return message;
}

function getPlayerSessionId() {
  const storageCandidates = [
    typeof window !== 'undefined' ? window.localStorage : null,
    typeof window !== 'undefined' ? window.sessionStorage : null
  ].filter(Boolean);

  for (const storage of storageCandidates) {
    try {
      let sessionId = storage.getItem(PLAYER_SESSION_KEY);
      if (!sessionId) {
        sessionId = uuidv4();
        storage.setItem(PLAYER_SESSION_KEY, sessionId);
      }
      return sessionId;
    } catch {
      // Ignore unavailable storage and fall through to the next candidate.
    }
  }

  return uuidv4();
}

function getRoomJoinPayload(roomId, username) {
  const avatarId = sessionStorage.getItem('avatarId');
  const avatarImage = sessionStorage.getItem('avatarImage');
  const avatarPayload = avatarId !== null ? { avatarId, avatarImage } : {};
  return {
    roomId,
    username,
    playerSessionId: getPlayerSessionId(),
    ...avatarPayload
  };
}

function serializeCharacterForSocket(character) {
  const rawTags = character.rawTags instanceof Map
    ? Object.fromEntries(character.rawTags.entries())
    : character.rawTags || {};
  return {
    ...character,
    rawTags
  };
}

function getAttemptCount(player) {
  return Array.isArray(player?.attemptMarks) ? player.attemptMarks.length : 0;
}

function hasRoundResult(player) {
  return Boolean(player?.roundResult);
}

const Multiplayer = () => {
  const navigate = useNavigate();
  const { roomId } = useParams();
  const [isHost, setIsHost] = useState(false);
  const [players, setPlayers] = useState([]);
  const [roomUrl, setRoomUrl] = useState('');
  // 从 cookie 读取保存的用户名
  const getSavedUsername = () => {
    const match = document.cookie.match(/(?:^|; )multiplayerUsername=([^;]*)/);
    return match ? decodeURIComponent(match[1]) : '';
  };
  const [username, setUsername] = useState(getSavedUsername);
  const [isJoined, setIsJoined] = useState(false);
  const { socket, socketRef, maxReconnectAttempts } = useMultiplayerSocket(SOCKET_URL);
  const roomIdRef = useRef(roomId);
  const usernameRef = useRef(username);
  const isJoinedRef = useRef(isJoined);
  const isHostRef = useRef(isHost);
  const [error, setError] = useState('');
  const [showSettings, setShowSettings] = useState(false);
  const [isPublic, setIsPublic] = useState(true);
  const [roomName, setRoomName] = useState('');
  const ROOMS_PER_PAGE = 10;
  const {
    roomList,
    loadingRooms,
    roomListExpanded,
    setRoomListExpanded,
    roomListPage,
    setRoomListPage,
    fetchRoomList,
    refreshRoomListIfVisible
  } = useRoomLobby({ socketUrl: SOCKET_URL, isJoined, roomsPerPage: ROOMS_PER_PAGE });
  const [gameSettings, setGameSettings] = useState({
    // 默认设置
    startYear: new Date().getFullYear()-5, // 起始年份
    endYear: new Date().getFullYear(), // 结束年份
    topNSubjects: 0, // 条目数，0 表示全范围
    useSubjectPerYear: false, // 每年独立计算热度
    metaTags: ["", "", ""], // 筛选用标签
    useIndex: false, // 使用指定目录
    indexId: null, // 目录ID
    addedSubjects: [], // 已添加的作品
    mainCharacterOnly: true, // 仅主角
    characterNum: 6, // 每个作品的角色数
    maxAttempts: 10, // 最大尝试次数
    enableHints: false, // 提示出现次数
    includeGame: false, // 包含游戏作品
    timeLimit: 60, // 时间限制
    subjectSearch: true, // 启用作品搜索
    characterTagNum: 6, // 角色标签数量
    subjectTagNum: 6, // 作品标签数量
    commonTags: true, // 共同标签优先
    useHints: [], // 提示出现次数
    useImageHint: 0, // 图片提示时机
    imgHint: null, // 图片提示
    syncMode: false, // 同步模式
    nonstopMode: false, // 血战模式
    globalPick: false, // 角色全局BP
    tagBan: false, // 标签全局BP
  });
  const {
    gameSettingsRef,
    latestPlayersRef,
    setLatestPlayers,
    clearLatestPlayers
  } = useRoundState({ gameSettings });

  const {
    isGameStarted,
    setIsGameStarted,
    guesses,
    setGuesses,
    guessesLeft,
    setGuessesLeft,
    isGuessing,
    setIsGuessing,
    isGameStarting,
    setIsGameStarting,
    answerCharacter,
    setAnswerCharacter,
    hints,
    setHints,
    useImageHint,
    setUseImageHint,
    imgHint,
    setImgHint,
    shouldResetTimer,
    setShouldResetTimer,
    gameEnd,
    setGameEnd,
    scoreDetails,
    setScoreDetails,
    globalGameEnd,
    setGlobalGameEnd,
    endGameSettings,
    setEndGameSettings,
    guessesHistory,
    setGuessesHistory,
    showCharacterPopup,
    setShowCharacterPopup,
    showSetAnswerPopup,
    setShowSetAnswerPopup,
    showFeedbackPopup,
    setShowFeedbackPopup,
    isAnswerSetter,
    setIsAnswerSetter,
    canShowSelectedAnswer,
    setCanShowSelectedAnswer,
    answerViewMode,
    setAnswerViewMode,
    isGuessTableCollapsed,
    setIsGuessTableCollapsed,
    waitingForSync,
    setWaitingForSync,
    syncStatus,
    setSyncStatus,
    nonstopProgress,
    setNonstopProgress,
    isObserver,
    setIsObserver,
    bannedSharedTags,
    setBannedSharedTags
  } = useRoundReducer();
  const answerCharacterRef = useRef(null);
  const timeUpRef = useRef(0);
  const lastTimeoutEmitRef = useRef(0);
  const pendingUpdatePlayersRef = useRef(null);
  const updatePlayersFrameRef = useRef(null);
  const gameEndedRef = useRef(false);
  const [showNames, setShowNames] = useState(true);
  const {
    notification: kickNotification,
    showNotification: showKickNotification
  } = useTimedNotification(5000);
  const [connectionStatus, setConnectionStatus] = useState('connected');
  const reconnectAttemptsRef = useRef(0);
  const isManualDisconnectRef = useRef(false);
  const isAutoReconnectingRef = useRef(false);
  const [confirmDialog, setConfirmDialog] = useState(null);
  const {
    submitGuess,
    resolveGuess,
    rejectGuess,
    hasPendingGuess
  } = usePendingGuess();
  const {
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
  } = useManualAnswerFlow({
    roomId,
    socketRef,
    isHost,
    gameSettings,
    setShowSetAnswerPopup,
    showNotification: showKickNotification
  });
  const {
    allSpectators,
    getFilteredSyncStatus,
    displaySettings,
    isTeamObserver
  } = useMultiplayerDerivedState({
    players,
    syncStatus,
    socketId: socket?.id,
    gameSettings,
    endGameSettings,
    globalGameEnd
  });
  const {
    copyRoomUrl,
    handleVisibilityToggle,
    handleRoomNameChange,
    handleRoomNameBlur,
    handleRoomNameKeyDown,
    handleMessageChange,
    handleTeamChange
  } = useRoomActions({
    roomId,
    roomUrl,
    roomName,
    setRoomName,
    setPlayers,
    socketRef,
    isHost,
    showNotification: showKickNotification
  });

  useEffect(() => {
    roomIdRef.current = roomId;
  }, [roomId]);

  useEffect(() => {
    usernameRef.current = username;
  }, [username]);

  useEffect(() => {
    isJoinedRef.current = isJoined;
  }, [isJoined]);

  useEffect(() => {
    isHostRef.current = isHost;
  }, [isHost]);
  const handleFeedbackSubmit = async ({ type, description, includeLogs }) => {
    const payload = {
      bugType: type,
      description: roomId ? `[房间 ${roomId}] ${description}` : description,
    };

    if (includeLogs) {
      payload.logs = logCollector.getLogs();
      payload.errors = logCollector.getErrors();
      payload.diagnosticData = logCollector.getDiagnosticData();
    }

    await axios.post(`${SOCKET_URL}/api/bug-feedback`, payload);
  };

  useMultiplayerSocketEvents({
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
  });


  useEffect(() => {
    // If user is no longer host, ensure manual mode is disabled
    if (!isHost && isManualMode) {
      setIsManualMode(false);
    }
  }, [isHost, isManualMode, setIsManualMode]);

  useEffect(() => {
    if (!roomId) {
      // Create new room if no roomId in URL
      const newRoomId = uuidv4();
      setIsHost(true);
      navigate(`/multiplayer/${newRoomId}`);
    } else {
      // Set room URL for sharing
      setRoomUrl(window.location.href);
      
      // 检查是否有待加入的房间（从房间列表点击加入）
      const pendingUsername = sessionStorage.getItem('pendingUsername');
      const pendingRoomId = sessionStorage.getItem('pendingRoomId');
      
      if (pendingUsername && pendingRoomId === roomId) {
        // 清除 sessionStorage
        sessionStorage.removeItem('pendingUsername');
        sessionStorage.removeItem('pendingRoomId');
        
        // 设置用户名并自动加入
        setUsername(pendingUsername);
        setIsHost(false);
        
        // 保存用户名到 cookie，有效期 30 天
        const expires = new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toUTCString();
        document.cookie = `multiplayerUsername=${encodeURIComponent(pendingUsername)}; expires=${expires}; path=/`;
        
        // 延迟执行加入，确保 socket 已连接
        setTimeout(() => {
          socketRef.current?.emit('joinRoom', getRoomJoinPayload(roomId, pendingUsername));
          socketRef.current?.emit('requestGameSettings', { roomId });
        }, 100);
      }
    }
  }, [roomId, navigate, socketRef]);

  useEffect(() => {
    if (isHost && isJoined) {
      socketRef.current?.emit('updateGameSettings', { roomId, settings: gameSettings });
    }
  }, [isHost, isJoined, roomId, gameSettings, socketRef]);

  const handleJoinRoom = () => {
    if (!username.trim()) {
      showKickNotification('请输入用户名', 'warning');
      setError('请输入用户名');
      return;
    }

    setError('');
    const roomPayload = getRoomJoinPayload(roomId, username);
    if (isHost) {
      socketRef.current?.emit('createRoom', roomPayload);
      socketRef.current?.emit('updateGameSettings', { roomId, settings: gameSettings });
    } else {
      socketRef.current?.emit('joinRoom', roomPayload);
      socketRef.current?.emit('requestGameSettings', { roomId });
    }
    // 保存用户名到 cookie，有效期 30 天
    const expires = new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toUTCString();
    document.cookie = `multiplayerUsername=${encodeURIComponent(username)}; expires=${expires}; path=/`;
  };

  const handleReadyToggle = () => {
    socketRef.current?.emit('toggleReady', { roomId });
  };

  const handleSettingsChange = (key, value) => {
    if (typeof key === 'object' && key !== null) {
      setGameSettings(prev => ({
        ...prev,
        ...key
      }));
      return;
    }

    setGameSettings(prev => ({
      ...prev,
      [key]: value
    }));
  };

  const handleGameEnd = (isWin) => {
    if (gameEndedRef.current) return;

    // 血战模式下，猜对不结束游戏，只发送 nonstopWin 事件
    if (isWin && gameSettings.nonstopMode) {
      socketRef.current?.emit('nonstopWin', { roomId });
      // 血战模式下猜对后进入观战状态，但不设置 gameEnd
      setGameEnd(true);
      setWaitingForSync(false); // 重置同步等待状态
      gameEndedRef.current = true;
      return;
    }
    
    gameEndedRef.current = true;
    setGameEnd(true);
    setWaitingForSync(false); // 重置同步等待状态
    socketRef.current?.emit('gameEnd', {
      roomId,
      result: isWin ? 'win' : 'lose'
    });
  };

  const handleCharacterSelect = async (character) => {
    if (isGuessing || gameEnd) return;

    // 旁观者和出题人不能猜测（用 canShowSelectedAnswer 作为本局“出题人视角”的门闩，防止状态抖动）
    if (isObserver || isAnswerSetter || canShowSelectedAnswer) {
      return;
    }

    // 同步模式：等待其他玩家时不能猜测
    if (waitingForSync) {
      showKickNotification('【同步模式】请等待其他玩家完成本轮猜测', 'warning');
      return;
    }

    if (gameSettings.globalPick) {
      const duplicateInHistory = guessesHistory.filter(playerHistory => playerHistory.username !== username).some(playerHistory =>
        Array.isArray(playerHistory.guesses) &&
        playerHistory.guesses.some(guessEntry => guessEntry?.guessData?.id === character.id)
      );
      if (duplicateInHistory) {
        showKickNotification('【全局BP】已经被别人猜过了！请尝试其他角色', 'warning');
        return;
      }
    }

    setIsGuessing(true);
    setShouldResetTimer(true);

    try {
      if (!character?.id) {
        console.warn('Invalid guess character, not emitting');
        throw new Error('提交猜测失败：搜索结果缺少角色 ID 或名称，请重新选择角色');
      }
      if (!socketRef.current?.connected) {
        throw new Error('提交猜测失败：WebSocket 未连接，请等待重连或刷新页面');
      }
      const guessResult = await submitGuess({
        socket: socketRef.current,
        roomId,
        characterId: character.id
      });

      const { guess, isCorrect } = guessResult || {};
      if (!guess) {
        throw new Error('提交猜测失败：服务端返回了空结果，请刷新页面同步房间状态后重试');
      }
      if (gameSettings.tagBan && Array.isArray(guess.sharedMetaTags) && guess.sharedMetaTags.length > 0) {
        socketRef.current?.emit('tagBanSharedMetaTags', {
          roomId,
          tags: guess.sharedMetaTags
        });
      }
      setGuesses(prevGuesses => [...prevGuesses, guess]);
      if (isCorrect) {
        handleGameEnd(true);
      }
    } catch (error) {
      console.error('Error processing guess:', error);
      showKickNotification(describeGuessError(error), 'error');
    } finally {
      setIsGuessing(false);
      setShouldResetTimer(false);
    }
  };

  const handleTimeUp = () => {
    if (timeUpRef.current >= 5 || gameEnd || gameEndedRef.current) return;

    // 已结束/观战状态不再发送超时
    const myId = socketRef.current?.id || socket?.id;
    const me = latestPlayersRef.current.find(p => p?.id === myId);
    if (hasRoundResult(me)) return;

    // 客户端侧防抖，避免网络卡顿导致短时间内多次触发
    const now = Date.now();
    if (now - lastTimeoutEmitRef.current < 1500) return;
    lastTimeoutEmitRef.current = now;

    timeUpRef.current += 1;

    // 发送超时事件到服务器，由服务器统一处理次数扣除和死亡判定
    // 不在客户端手动减少 guessesLeft，避免与服务器状态不同步
    socketRef.current?.emit('timeOut', { roomId });

    setShouldResetTimer(true);
    setTimeout(() => {
      setShouldResetTimer(false);
      timeUpRef.current = 0;
    }, 100);
  };

  const handleEnterObserverMode = () => {
    // 进入旁观模式（不结束游戏，允许其他玩家继续）
    setIsObserver(true);
    // 进入旁观后允许看到答案卡片
    setCanShowSelectedAnswer(true);
    socketRef.current?.emit('enterObserverMode', {
      roomId
    });
  };

  const handleSurrender = () => {
    if (gameEnd || gameEndedRef.current) return;
    // 投降后进入旁观模式
    handleEnterObserverMode();
  };

  const handleStartGame = async () => {
    // 防止重复点击：如果正在初始化游戏或游戏已开始，则返回
    if (isGameStarting || isGameStarted) return;

    // 若全员为旁观者队伍，不允许开始
    if (allSpectators) {
      showKickNotification('至少需要一名非旁观者才能开始游戏', 'warning');
      return;
    }
     
    if (isHost) {
      if (!socketRef.current?.connected) {
        showKickNotification('连接未建立，无法开始游戏', 'error');
        return;
      }
      // 设置正在启动游戏的标志
      setIsGameStarting(true);
      
      try {
        // 保存最新创建的多人模式设置
        try {
          localStorage.setItem('latestMultiplayerSettings', JSON.stringify(gameSettings));
        } catch (e) { /* ignore */ }
        try {
          if (gameSettings.addedSubjects.length > 0) {
            await axios.post(SOCKET_URL + '/api/subject-added', {
              addedSubjects: gameSettings.addedSubjects
            });
          }
        } catch (error) {
          console.error('Failed to update subject count:', error);
        }
        const startPayload = {
          roomId,
          settings: gameSettings
        };
        if (gameSettings.useIndex) {
          const character = await getRandomCharacter(gameSettings);
          startPayload.character = serializeCharacterForSocket(character);
        }
        socketRef.current?.emit('gameStart', startPayload);
      } catch (error) {
        console.error('Failed to start game:', error);
        setIsGameStarting(false);
        showKickNotification('开始游戏失败，请重试', 'error');
      }
    }
  };

  const handleKickPlayer = (playerId) => {
    if (!isHost || !socketRef.current) return;
    
    // 确认当前玩家是房主
    const currentPlayer = players.find(p => p.id === socketRef.current.id);
    if (!currentPlayer || !currentPlayer.isHost) {
      showKickNotification('只有房主可以踢出玩家', 'warning');
      return;
    }
     
    // 防止房主踢出自己
    if (playerId === socketRef.current.id) {
      showKickNotification('房主不能踢出自己', 'warning');
      return;
    }
     
    requestConfirm('确定要踢出该玩家吗？', () => {
      try {
        socketRef.current.emit('kickPlayer', { roomId, playerId });
      } catch (error) {
        console.error('踢出玩家失败:', error);
        showKickNotification('踢出玩家失败，请重试', 'error');
      }
    });
  };

  const handleTransferHost = (playerId) => {
    if (!isHost || !socketRef.current) return;
     
    requestConfirm('确定要将房主权限转移给该玩家吗？', () => {
      socketRef.current.emit('transferHost', { roomId, newHostId: playerId });
      setIsHost(false);
    });
  };

  // Add handleQuickJoin function
  const handleQuickJoin = async () => {
    try {
      const response = await axios.get(`${SOCKET_URL}/quick-join`);
      const targetUrl = new URL(response.data.url, window.location.origin);
      if (targetUrl.origin === window.location.origin) {
        const route = targetUrl.hash.startsWith('#/')
          ? targetUrl.hash.slice(1)
          : `${targetUrl.pathname}${targetUrl.search}`;
        navigate(route);
      } else {
        window.location.assign(response.data.url);
      }
    } catch (error) {
      if (error.response && error.response.status === 404) {
        showKickNotification(error.response.data.error || '没有可用的公开房间', 'warning');
      } else {
        showKickNotification('快速加入失败，请重试', 'error');
      }
    }
  };

  // 加入指定房间
  const handleJoinSpecificRoom = (targetRoomId) => {
    if (!username.trim()) {
      showKickNotification('请输入用户名', 'warning');
      setError('请输入用户名');
      return;
    }
    
    // 将用户名保存到 sessionStorage，以便页面刷新后自动填充
    sessionStorage.setItem('pendingUsername', username);
    sessionStorage.setItem('pendingRoomId', targetRoomId);
    
    navigate(`/multiplayer/${targetRoomId}`);
  };

  const requestConfirm = (message, onConfirm) => {
    setConfirmDialog({ message, onConfirm });
  };

  if (!roomId) {
    return (
      <MultiplayerLobby
        username={username}
        onUsernameChange={setUsername}
        onCreateRoom={() => navigate('/multiplayer', { replace: true, state: { autoCreate: true } })}
        onQuickJoin={handleQuickJoin}
        roomList={roomList}
        loadingRooms={loadingRooms}
        roomListPage={roomListPage}
        roomsPerPage={ROOMS_PER_PAGE}
        onRoomListPageChange={setRoomListPage}
        onJoinRoom={handleJoinSpecificRoom}
        onRefreshRooms={() => {
          fetchRoomList();
          setRoomListExpanded(true);
        }}
      />
    );
  }

  return (
    <div className="multiplayer-container">
      <ConnectionStatusBanner
        isJoined={isJoined}
        connectionStatus={connectionStatus}
        reconnectAttempts={reconnectAttemptsRef.current}
        maxReconnectAttempts={maxReconnectAttempts}
      />
      <MultiplayerNotification notification={kickNotification} />
      <ConfirmDialog dialog={confirmDialog} onCancel={() => setConfirmDialog(null)} />
      <button
        type="button"
        className="social-link floating-back-button"
        title="Back"
        onClick={() => navigate('/')}
      >
        <Icon name="home" />
      </button>
      <button
        type="button"
        className="social-link floating-feedback-button"
        title="Bug/标签反馈"
        onClick={() => setShowFeedbackPopup(true)}
      >
        <Icon name="exclamationCircle" />
      </button>
      {!isJoined ? (
        <>
          <div className="join-container">
            <h2>{isHost ? '创建房间' : '加入房间'}</h2>
            {isHost && !isJoined && (
              <button onClick={handleQuickJoin} className="join-button quick-join-btn">快速加入</button>
            )}
            <input
              type="text"
              placeholder="输入用户名"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              className="username-input"
              maxLength={20}
            />
            <button onClick={handleJoinRoom} className="join-button">
              {isHost ? '创建' : '加入'}
            </button>
            {error && <p className="error-message">{error}</p>}
          </div>
          
          {/* 房间列表 - 使用与 Leaderboard 一致的样式 */}
          <div className="leaderboard-container room-list-wrapper">
            <div className="leaderboard-header" onClick={() => {
              const newExpanded = !roomListExpanded;
              setRoomListExpanded(newExpanded);
            }}>
              <h3>公开房间 {roomList.length > 0 && `(${roomList.length})`}</h3>
              <span className={`expand-icon ${roomListExpanded ? 'expanded' : ''}`}>{roomListExpanded ? '▼' : '▶'}</span>
            </div>
            {roomListExpanded && (
              <div className="leaderboard-content">
                <RoomList
                  rooms={roomList}
                  loading={loadingRooms}
                  page={roomListPage}
                  roomsPerPage={ROOMS_PER_PAGE}
                  onPageChange={setRoomListPage}
                  onJoinRoom={handleJoinSpecificRoom}
                />
              </div>
            )}
          </div>
          
          <Suspense fallback={null}>
            <Roulette />
            <Leaderboard />
          </Suspense>
        </>
      ) : (
        <>
          <PlayerList 
            players={players} 
            socket={socketRef.current} 
            isGameStarted={isGameStarted}
            handleReadyToggle={handleReadyToggle}
            onAnonymousModeChange={setShowNames}
            isManualMode={isManualMode}
            isHost={isHost}
            answerSetterId={answerSetterId}
            waitingForAnswer={waitingForAnswer}
            onSetAnswerSetter={handleSetAnswerSetter}
            onKickPlayer={handleKickPlayer}
            onTransferHost={handleTransferHost}
            onMessageChange={handleMessageChange}
            onTeamChange={handleTeamChange}
          />
          <div className="anonymous-mode-info">
            匿名模式？点表头"名"切换。<br/>
            沟通玩法？点自己名字编辑短信息。<br/>
            有Bug/缺标签？到<a href="https://github.com/kennylimz/anime-character-guessr/issues/new" target="_blank" rel="noopener noreferrer">Github Issues</a>反馈或加入下方QQ群。<br/>
            想找猜猜呗同好？QQ群：<a href="https://qm.qq.com/q/2sWbSsCwBu" target="_blank" rel="noopener noreferrer">467740403</a>。
          </div>

          {!isGameStarted && !globalGameEnd && (
            <>
              {isHost && !waitingForAnswer && (
                <HostRoomControls
                  isPublic={isPublic}
                  roomName={roomName}
                  roomUrl={roomUrl}
                  onRoomNameChange={handleRoomNameChange}
                  onRoomNameBlur={handleRoomNameBlur}
                  onRoomNameKeyDown={handleRoomNameKeyDown}
                  onCopyRoomUrl={copyRoomUrl}
                  onOpenSettings={() => setShowSettings(true)}
                  onToggleVisibility={handleVisibilityToggle}
                  onStartGame={handleStartGame}
                  onManualMode={handleManualMode}
                  isGameStarting={isGameStarting}
                  isManualMode={isManualMode}
                  disabled={players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
                />
              )}
              {isHost && waitingForAnswer && (
                <HostWaitingControls onCancel={handleCancelWaitForAnswer} />
              )}
              {!isHost && (
                <>
                  <GameSettingsDisplay settings={gameSettings} />
                </>
              )}
            </>
          )}

          {isGameStarted && !globalGameEnd && (
            <InGameRoundView
              isAnswerSetter={isAnswerSetter}
              isTeamObserver={isTeamObserver}
              onCharacterSelect={handleCharacterSelect}
              isGuessing={isGuessing}
              waitingForSync={waitingForSync}
              gameEnd={gameEnd}
              gameSettings={gameSettings}
              isGameStarted={isGameStarted}
              syncStatus={syncStatus}
              getFilteredSyncStatus={getFilteredSyncStatus}
              showNames={showNames}
              nonstopProgress={nonstopProgress}
              players={players}
              onTimeUp={handleTimeUp}
              isObserver={isObserver}
              canShowSelectedAnswer={canShowSelectedAnswer}
              shouldResetTimer={shouldResetTimer}
              guessesLeft={guessesLeft}
              onSurrender={handleSurrender}
              hints={hints}
              useImageHint={useImageHint}
              imgHint={imgHint}
              guesses={guesses}
              answerCharacter={answerCharacter}
              bannedTags={bannedSharedTags}
              answerViewMode={answerViewMode}
              setAnswerViewMode={setAnswerViewMode}
              isGuessTableCollapsed={isGuessTableCollapsed}
              setIsGuessTableCollapsed={setIsGuessTableCollapsed}
              guessesHistory={guessesHistory}
            />
          )}

          {!isGameStarted && globalGameEnd && (
            <GameEndView
              isHost={isHost}
              isPublic={isPublic}
              roomName={roomName}
              roomUrl={roomUrl}
              onRoomNameChange={handleRoomNameChange}
              onRoomNameBlur={handleRoomNameBlur}
              onRoomNameKeyDown={handleRoomNameKeyDown}
              onCopyRoomUrl={copyRoomUrl}
              onOpenSettings={() => setShowSettings(true)}
              onToggleVisibility={handleVisibilityToggle}
              onStartGame={handleStartGame}
              onManualMode={handleManualMode}
              isManualMode={isManualMode}
              hostControlsDisabled={players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
              displaySettings={displaySettings}
              answerCharacter={answerCharacter}
              players={players}
              socketId={socket?.id}
              scoreDetails={scoreDetails}
              showNames={showNames}
              onShowCharacter={() => setShowCharacterPopup(true)}
              guessesHistory={guessesHistory}
            />
          )}

          <Suspense fallback={null}>
            {showSettings && (
              <SettingsPopup
                gameSettings={gameSettings}
                onSettingsChange={handleSettingsChange}
                onClose={() => setShowSettings(false)}
                hideRestart={true}
                isMultiplayer={true}
              />
            )}

            {globalGameEnd && showCharacterPopup && answerCharacter && (
              <GameEndPopup
                result={guesses.some(g => g.isAnswer) ? 'win' : 'lose'}
                answer={answerCharacter}
                onClose={() => setShowCharacterPopup(false)}
              />
            )}

            {showSetAnswerPopup && (
              <SetAnswerPopup
                onSetAnswer={handleSetAnswer}
                onCancel={handleCancelWaitForAnswer}
                gameSettings={gameSettings}
              />
            )}
          </Suspense>
        </>

      )}
      {showFeedbackPopup && (
        <Suspense fallback={null}>
          <FeedbackPopup
            onClose={() => setShowFeedbackPopup(false)}
            onSubmit={handleFeedbackSubmit}
          />
        </Suspense>
      )}
    </div>
  );
};

export default Multiplayer;
