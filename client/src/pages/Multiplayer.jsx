import { useState, useEffect, useRef, useMemo } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { v4 as uuidv4 } from 'uuid';
import { io } from 'socket.io-client';
import SettingsPopup from '../components/SettingsPopup';
import SearchBar from '../components/SearchBar';
import GuessesTable from '../components/GuessesTable';
import Timer from '../components/Timer';
import PlayerList from '../components/PlayerList';
import RoomList from '../components/RoomList';
import GameEndPopup from '../components/GameEndPopup';
import SetAnswerPopup from '../components/SetAnswerPopup';
import FeedbackPopup from '../components/FeedbackPopup';
import GameSettingsDisplay from '../components/GameSettingsDisplay';
import Leaderboard from '../components/Leaderboard';
import Roulette from '../components/Roulette';
import Image from '../components/Image';
import usePendingGuess from '../hooks/usePendingGuess';
import useRoomLobby from '../hooks/useRoomLobby';
import logCollector from '../utils/logCollector';
import '../styles/Multiplayer.css';
import '../styles/game.css';
import axios from 'axios';
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

function getAttemptCount(player) {
  return Array.isArray(player?.attemptMarks) ? player.attemptMarks.length : 0;
}

function hasRoundResult(player) {
  return Boolean(player?.roundResult);
}

function isWinnerResult(result) {
  return result === 'win' || result === 'bigWin' || result === 'teamWin';
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
  const [socket, setSocket] = useState(null);
  const socketRef = useRef(null);
  const roomIdRef = useRef(roomId);
  const usernameRef = useRef(username);
  const isJoinedRef = useRef(isJoined);
  const isHostRef = useRef(isHost);
  const [error, setError] = useState('');
  const [showSettings, setShowSettings] = useState(false);
  const [isPublic, setIsPublic] = useState(true);
  const [roomName, setRoomName] = useState('');
  const [isManualMode, setIsManualMode] = useState(false);
  const [answerSetterId, setAnswerSetterId] = useState(null);
  const [waitingForAnswer, setWaitingForAnswer] = useState(false);
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

  // Game state
  const [isGameStarted, setIsGameStarted] = useState(false);
  const [guesses, setGuesses] = useState([]);
  const [guessesLeft, setGuessesLeft] = useState(10);
  const [isGuessing, setIsGuessing] = useState(false);
  const [isGameStarting, setIsGameStarting] = useState(false); // 防止重复点击开始按钮
  const answerCharacterRef = useRef(null);
  const gameSettingsRef = useRef(gameSettings);
  const [answerCharacter, setAnswerCharacter] = useState(null);
  const [hints, setHints] = useState([]);
  const [useImageHint, setUseImageHint] = useState(0);
  const [imgHint, setImgHint] = useState(null);
  const [shouldResetTimer, setShouldResetTimer] = useState(false);
  const [gameEnd, setGameEnd] = useState(false);
  const timeUpRef = useRef(0);
  const lastTimeoutEmitRef = useRef(0);
  const pendingUpdatePlayersRef = useRef(null);
  const updatePlayersFrameRef = useRef(null);
  const gameEndedRef = useRef(false);
  const [scoreDetails, setScoreDetails] = useState(null);
  const [globalGameEnd, setGlobalGameEnd] = useState(false);
  const [endGameSettings, setEndGameSettings] = useState(null); // 上一局的模式快照
  const [guessesHistory, setGuessesHistory] = useState([]);
  const [showNames, setShowNames] = useState(true);
  const [showCharacterPopup, setShowCharacterPopup] = useState(false);
  const [showSetAnswerPopup, setShowSetAnswerPopup] = useState(false);
  const [showFeedbackPopup, setShowFeedbackPopup] = useState(false);
  const [isAnswerSetter, setIsAnswerSetter] = useState(false);
  // 是否允许在本局游戏中显示 selected-answer（答案卡片）。
  // 该状态必须：每局开始时默认 false；仅在收到服务端“本客户端应显示答案”的信号后置为 true（出题人/旁观者/临时旁观者）；每局结束时重置。
  const [canShowSelectedAnswer, setCanShowSelectedAnswer] = useState(false);
  const [kickNotification, setKickNotification] = useState(null);
  const [answerViewMode, setAnswerViewMode] = useState('simple'); // 'simple' or 'detailed'
  const [isGuessTableCollapsed, setIsGuessTableCollapsed] = useState(false); // 折叠猜测表格（只显示最新3个）
  const [waitingForSync, setWaitingForSync] = useState(false); // 同步模式：等待其他玩家
  const [syncStatus, setSyncStatus] = useState({}); // 同步模式：各玩家状态
  const [nonstopProgress, setNonstopProgress] = useState(null); // 血战模式：进度信息
  const [isObserver, setIsObserver] = useState(false);
  const [bannedSharedTags, setBannedSharedTags] = useState([]);
  const latestPlayersRef = useRef([]);
  const [connectionStatus, setConnectionStatus] = useState('connected');
  const reconnectAttemptsRef = useRef(0);
  const maxReconnectAttempts = 5;
  const reconnectTimerRef = useRef(null);
  const isManualDisconnectRef = useRef(false);
  const isAutoReconnectingRef = useRef(false);
  const [confirmDialog, setConfirmDialog] = useState(null);
  const {
    submitGuess,
    resolveGuess,
    rejectGuess,
    hasPendingGuess
  } = usePendingGuess();

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
  const allSpectators = useMemo(() => {
    if (!players || players.length === 0) return false;
    return players.every(p => p.disconnected || p.team === '0');
  }, [players]);

  // 同步模式队列展示过滤：已完成且（断线/投降/猜对/队伍胜利）的不显示
  const getFilteredSyncStatus = () => {
    const statusList = syncStatus?.syncStatus || [];
    return statusList.filter((entry) => {
      const player = players.find(p => p.id === entry.id);
      const isDisconnected = !!player?.disconnected;
      // 保留已完成的赢家在当前轮展示，下一轮已被服务器移出列表；仅隐藏断线玩家
      return !(entry.completed && isDisconnected);
    });
  };

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

  useEffect(() => {
    // Initialize socket connection
    const newSocket = io(SOCKET_URL);
    setSocket(newSocket);
    socketRef.current = newSocket;
    latestPlayersRef.current = [];

    // 用于追踪事件是否已经被处理
    const kickEventProcessed = {}; 

    // 辅助函数：从玩家数据更新剩余次数和检查死亡状态
    const updateGuessesLeftFromPlayer = (player) => {
      if (!player || player.isAnswerSetter || player.team === '0') {
        return;
      }

      const used = getAttemptCount(player);
      const max = gameSettingsRef.current?.maxAttempts || 10;
      const left = Math.max(0, max - used);
      setGuessesLeft(left);

      if (player.roundResult === 'dead') {
        // 已被服务器判死，进入旁观状态，避免重复触发结束逻辑
        setIsObserver(true);
        // 死亡后属于“临时旁观者”，允许看到答案卡片
        setCanShowSelectedAnswer(true);
      }
    };

    // Socket event listeners
    const applyUpdatePlayers = ({ players, isPublic, answerSetterId }) => {
      setPlayers(players);
      latestPlayersRef.current = Array.isArray(players) ? players : [];
      if (isPublic !== undefined) {
        setIsPublic(isPublic);
      }
      if (answerSetterId !== undefined) {
        setAnswerSetterId(answerSetterId);
      }
      // Sync isHost state from player list to ensure correctness
      const me = players.find(p => p.id === newSocket.id);
      if (me) {
        setIsJoined(true);
        setError('');
        setIsHost(me.isHost);
        // 同时检查是否应该进入旁观模式（防止网络卡顿导致的状态不同步）
        if (me.team === '0') {
          setIsObserver(true);
        }

        // 立即更新剩余次数并检查死亡状态
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
        latestPlayersRef.current = next;
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
      // Show popup if current user is the answer setter
      if (answerSetterId === newSocket.id) {
        setShowSetAnswerPopup(true);
      }
    });

    // 手动出题被取消（出题人离开或被踢出）
    newSocket.on('waitForAnswerCanceled', ({ message }) => {
      setWaitingForAnswer(false);
      setAnswerSetterId(null);
      setShowSetAnswerPopup(false);
      console.log(`[INFO] ${message}`);
      // Optionally show notification to user
      if (message) {
        showKickNotification(message, 'warning');
      }
    });

    // 同步模式：等待其他玩家
    newSocket.on('syncWaiting', ({ round, syncStatus, completedCount, totalCount }) => {
      setSyncStatus({ round, syncStatus, completedCount, totalCount });
      // 只有当前玩家自己已完成猜测时才进入等待状态
      const myStatus = syncStatus?.find(p => p.id === newSocket.id);
      const iAmCompleted = myStatus?.completed || false;
      setWaitingForSync(iAmCompleted && completedCount < totalCount);
    });

    // 同步模式：收到服务端通知，开始下一轮
    newSocket.on('syncRoundStart', ({ round }) => {
      setWaitingForSync(false);
      // 保持同步状态显示，但重置为新一轮的初始状态（避免闪屏）
      setSyncStatus(prevStatus => ({
        ...prevStatus,
        round,
        syncStatus: prevStatus.syncStatus?.map(p => ({ ...p, completed: false })) || []
      }));
      setShouldResetTimer(true);
      setTimeout(() => setShouldResetTimer(false), 100);
      console.log(`[同步模式] 第 ${round} 轮开始`);
    });

    // 血战模式：进度更新
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
      
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      
      if (isJoinedRef.current && roomIdRef.current && usernameRef.current) {
        newSocket.emit('joinRoom', getRoomJoinPayload(roomIdRef.current, usernameRef.current));
        newSocket.emit('requestGameSettings', { roomId: roomIdRef.current });
      }
    });

    newSocket.on('disconnect', (reason) => {
      console.log('[WebSocket] 连接断开:', reason);

      // 断线期间不展示答案卡片，避免状态残留导致的短暂泄露
      setCanShowSelectedAnswer(false);
      setIsGameStarting(false);
      
      if (isManualDisconnectRef.current) {
        setConnectionStatus('disconnected');
        return;
      }
      
      setConnectionStatus('reconnecting');
      isAutoReconnectingRef.current = true;
      
      if (reason === 'io server disconnect') {
        newSocket.connect();
      }
      
      if (!newSocket.connected && reconnectAttemptsRef.current < maxReconnectAttempts) {
        reconnectAttemptsRef.current += 1;
        const attempt = reconnectAttemptsRef.current;
        
        console.log(`[WebSocket] 尝试重连 (${attempt}/${maxReconnectAttempts})...`);
        
        reconnectTimerRef.current = setTimeout(() => {
          if (!newSocket.connected && reconnectAttemptsRef.current <= maxReconnectAttempts) {
            newSocket.connect();
          }
        }, 3000);
      } else if (reconnectAttemptsRef.current >= maxReconnectAttempts) {
        setConnectionStatus('failed');
        setIsGameStarting(false);
        showKickNotification('连接已断开，多次重试失败，请刷新页面或稍后再试', 'error');
        setError('连接失败，请刷新页面重试');
      }
    });

    newSocket.on('connect_error', (error) => {
      console.error('[WebSocket] 连接错误:', error);
      
      if (!isManualDisconnectRef.current && reconnectAttemptsRef.current < maxReconnectAttempts) {
        setConnectionStatus('reconnecting');
      }
    });

    // 血战模式+同步模式：队友猜对通知
    newSocket.on('teamWin', ({ winnerName, message }) => {
      console.log(`[血战模式+同步模式] 队友猜对: ${winnerName}`);
      // 显示通知
      showKickNotification(message, 'info');
      // 标记游戏结束
      setGameEnd(true);
      gameEndedRef.current = true;
    });

    newSocket.on('gameStart', ({ character, settings, players, isPublic, hints = null, isAnswerSetter: isAnswerSetterFlag }) => {
      setIsGameStarting(false);
      // 每局开始先默认不显示答案卡片，避免网络卡顿/状态乱序导致短暂泄露
      setCanShowSelectedAnswer(false);
      const visibleAnswer = character && character.id ? {
        ...character,
        rawTags: new Map(Object.entries(character.rawTags || {}))
      } : null;
      setAnswerCharacter(visibleAnswer);
      answerCharacterRef.current = visibleAnswer;
      setGameSettings(settings);
      
      // Calculate guesses left based on current player's guess history
      const currentPlayer = players?.find(p => p.id === newSocket.id);
      const guessesMade = getAttemptCount(currentPlayer);
      const remainingGuesses = Math.max(0, (settings?.maxAttempts ?? 10) - guessesMade);
      setGuessesLeft(remainingGuesses);
      
      // 检查当前玩家是否为旁观者
      const observerFlag = currentPlayer?.team === '0';
      
      // 检查当前玩家是否已经结束游戏（重连时恢复状态）
      const hasGameEnded = hasRoundResult(currentPlayer);
      
      if (hasGameEnded) {
        // 玩家已经结束游戏，恢复结束状态
        gameEndedRef.current = true;
        setGameEnd(true);
      } else {
        gameEndedRef.current = false;
        setGameEnd(false);
      }

      // 旁观者（team==='0'）与已结束玩家（临时旁观者：猜对/投降/死亡等）都应进入旁观视角
      const effectiveObserver = !!observerFlag || !!hasGameEnded;
      setIsObserver(effectiveObserver);
      
      setIsAnswerSetter(isAnswerSetterFlag);
      // 仅当服务端明确告知“本客户端应显示答案”（出题人/旁观者/临时旁观者）时才允许显示 selected-answer
      setCanShowSelectedAnswer(!!isAnswerSetterFlag || effectiveObserver);
      if (players) {
        setPlayers(players);
      }
      if (isPublic !== undefined) {
        setIsPublic(isPublic);
      }

      setGuessesHistory([]);

      // Prepare hints if enabled
      let hintTexts = [];
      if (Array.isArray(settings?.useHints) && settings.useHints.length > 0 && hints) {
        hintTexts = hints;
      } else if (Array.isArray(settings?.useHints) && settings.useHints.length > 0 && visibleAnswer && visibleAnswer.summary) {
        // Automatic mode - generate hints from summary
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
      setEndGameSettings(null); // 新局开始时清空上一局模式快照
      setScoreDetails(null);
      setIsGameStarted(true);
      setGuesses([]);
      // 初始化同步和血战模式的进度显示
      if (settings?.syncMode) {
        // 初始化同步模式进度：所有非出题人、非旁观者、未断连的玩家
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
        // 初始化血战模式进度：0人猜对
        const activePlayers = players?.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected) || [];
        setNonstopProgress({
          winners: [],
          remainingCount: activePlayers.length,
          totalCount: activePlayers.length
        });
      } else {
        setNonstopProgress(null);
      }
      // 重置手动出题状态：清空等待状态和弹窗
      setWaitingForAnswer(false);
      setAnswerSetterId(null);
      setShowSetAnswerPopup(false);
    });

    newSocket.on('guessHistoryUpdate', ({ guesses }) => {
      setGuessesHistory(guesses);

      // 使用统一的辅助函数更新剩余次数
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
      // 如果当前用户是新房主，则更新状态
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
      // 只在特定情况下将玩家踢出房间，游戏开始相关错误不应该踢出房主
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
      setEndGameSettings(gameSettingsRef.current); // 保存上一局的模式设置用于结算展示
      setScoreDetails(scoreDetails || null);
      setGlobalGameEnd(true);
      setGuessesHistory(guesses);
      setIsGameStarted(false);
      setIsGameStarting(false); // 重置游戏启动标志，允许下一局开始
      setIsObserver(false); // 重置旁观者状态，下一局开始时会重新判断
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
      // 使用唯一标识确保同一事件不会处理多次
      const eventId = `${playerId}-${Date.now()}`;
      if (kickEventProcessed[eventId]) return;
      kickEventProcessed[eventId] = true;
      
      if (playerId === newSocket.id) {
        // 如果当前玩家被踢出，显示通知并返回主页
        showKickNotification('你已被房主踢出房间', 'kick');
        setIsJoined(false); 
        setGameEnd(true); 
        setTimeout(() => {
          navigate('/multiplayer/');
        }, 100); // 延长延迟时间确保通知显示后再跳转
      } else {
        showKickNotification(`玩家 ${username} 已被踢出房间`, 'kick');
        setPlayers(prevPlayers => prevPlayers.filter(p => p.id !== playerId));
      }
    });

    // Listen for team guess broadcasts
    newSocket.on('boardcastTeamGuess', ({ guess, playerId, playerName }) => {
      if (!guess) return;
      setGuesses(prev => [...prev, {
        ...guess,
        playerId,
        playerName,
        guessrName: guess.guessrName || playerName
      }]);
      
      // 只有正在参与游戏的玩家（非旁观者、非出题人）才需要减少猜测次数和触发游戏结束
      // 旁观者和出题人只是接收猜测信息用于显示，不参与游戏逻辑
      setPlayers(currentPlayers => {
        const currentPlayer = currentPlayers.find(p => p.id === newSocket.id);
        const isObserver = currentPlayer?.team === '0';
        const isAnswerSetterPlayer = currentPlayer?.isAnswerSetter;
        
        if (!isObserver && !isAnswerSetterPlayer) {
          // guessesLeft is synced via playerPatched/updatePlayers
          setShouldResetTimer(true);
          setTimeout(() => setShouldResetTimer(false), 100);
        }
        
        return currentPlayers; // 不修改 players 状态
      });
    });

    // Listen for reset timer event (team mode: when teammate times out)
    newSocket.on('resetTimer', () => {
      setShouldResetTimer(true);
      setTimeout(() => setShouldResetTimer(false), 100);
    });

    return () => {
      isManualDisconnectRef.current = true;
      
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      
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
      newSocket.disconnect();
      latestPlayersRef.current = [];
      setBannedSharedTags([]);
    };
  }, [hasPendingGuess, navigate, refreshRoomListIfVisible, rejectGuess, resolveGuess]);

  useEffect(() => {
    // If user is no longer host, ensure manual mode is disabled
    if (!isHost && isManualMode) {
      setIsManualMode(false);
    }
  }, [isHost, isManualMode]);

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
  }, [roomId, navigate]);

  useEffect(() => {
    if (isHost && isJoined) {
      socketRef.current?.emit('updateGameSettings', { roomId, settings: gameSettings });
    }
  }, [isHost, isJoined, roomId, gameSettings]);

  useEffect(() => {
    gameSettingsRef.current = gameSettings;
  }, [gameSettings]);

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
    setGameSettings(prev => ({
      ...prev,
      [key]: value
    }));
  };

  const copyRoomUrl = async () => {
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
      showKickNotification('复制失败，请手动复制房间链接', 'error');
    }
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
        socketRef.current?.emit('gameStart', {
          roomId,
          settings: gameSettings
        });
      } catch (error) {
        console.error('Failed to start game:', error);
        setIsGameStarting(false);
        showKickNotification('开始游戏失败，请重试', 'error');
      }
    }
  };

  const handleManualMode = () => {
    if (isManualMode) {
      setAnswerSetterId(null);
      setIsManualMode(false);
    } else {
      // 保存最新创建的多人模式设置
      if (isHost) {
        try {
          localStorage.setItem('latestMultiplayerSettings', JSON.stringify(gameSettings));
        } catch (e) { /* ignore */ }
      }
      // Set all players as ready when entering manual mode
      socketRef.current?.emit('enterManualMode', { roomId });
      setIsManualMode(true);
    }
  };

  const handleSetAnswerSetter = (setterId) => {
    if (!isHost || !isManualMode) return;
    socketRef.current?.emit('setAnswerSetter', { roomId, setterId });
  };

  const handleVisibilityToggle = () => {
    socketRef.current?.emit('toggleRoomVisibility', { roomId });
  };

  const handleRoomNameChange = (event) => {
    setRoomName(event.target.value);
  };

  const handleRoomNameBlur = () => {
    if (!isHost || !socketRef.current) return;
    const trimmed = roomName.trim();
    if (trimmed !== roomName) {
      setRoomName(trimmed);
    }
    socketRef.current.emit('updateRoomName', { roomId, roomName: trimmed });
  };

  const handleRoomNameKeyDown = (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      event.currentTarget.blur();
    }
  };

  const handleSetAnswer = async ({ character, hints }) => {
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
      showKickNotification('设置答案失败，请重试', 'error');
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

  // 创建一个函数显示踢出通知
  const showKickNotification = (message, type = 'kick') => {
    setKickNotification({ message, type });
    setTimeout(() => {
      setKickNotification(null);
    }, 5000); // 5秒后自动关闭通知
  };

  const requestConfirm = (message, onConfirm) => {
    setConfirmDialog({ message, onConfirm });
  };

  // Handle player message change
  const handleMessageChange = (newMessage) => {
    setPlayers(prevPlayers => prevPlayers.map(p =>
      p.id === socketRef.current?.id ? { ...p, message: newMessage } : p
    ));
    // Emit to server for sync
    socketRef.current?.emit('updatePlayerMessage', { roomId, message: newMessage });
  };

  // Handle player team change
  const handleTeamChange = (playerId, newTeam) => {
    if (!socketRef.current) return;
    setPlayers(prevPlayers => prevPlayers.map(p =>
      p.id === playerId ? { ...p, team: newTeam || null } : p
    ));
    // Emit to server for sync
    socketRef.current.emit('updatePlayerTeam', { roomId, team: newTeam || null });
  };


  const displaySettings = globalGameEnd ? (endGameSettings || gameSettings) : gameSettings;

  // 区分：真正旁观者（team==='0'） vs. 答对后进入旁观模式（isObserver===true 但仍保留原队伍）
  const isTeamObserver = useMemo(() => {
    const myId = socketRef.current?.id;
    if (!myId) return false;
    const me = players.find(p => p.id === myId);
    return me?.team === '0';
  }, [players]);

  if (!roomId) {
    return (
      <div className="multiplayer-container">
        <div className="top-row">
          <div className="room-info">
            <h2>多人游戏大厅</h2>
            <p>选择一个公开房间加入，或创建新房间。</p>
          </div>
        </div>

        <div className="settings-and-players">
          <div className="settings-panel">
            <div className="settings-header">
              <h3>加入或创建</h3>
            </div>
            <div className="form-row">
              <label htmlFor="username">用户名</label>
              <input
                id="username"
                type="text"
                value={username}
                onChange={e => setUsername(e.target.value)}
                placeholder="请输入用户名"
              />
            </div>
            <div className="button-group">
              <button className="primary-btn" onClick={() => navigate('/multiplayer', { replace: true, state: { autoCreate: true } })}>
                创建新房间
              </button>
              <button className="secondary-btn" onClick={handleQuickJoin}>
                快速加入公开房间
              </button>
            </div>
          </div>

          <div className="player-list">
            <div className="player-list-header">
              <div>
                <h3>公开房间 {roomList.length > 0 && `(${roomList.length})`}</h3>
                <small>展开列表以刷新（每 5 秒自动刷新）</small>
              </div>
              <div className="button-group">
                <button className="secondary-btn" onClick={() => { fetchRoomList(); setRoomListExpanded(true); }}>
                  刷新
                </button>
              </div>
            </div>

            <RoomList
              rooms={roomList}
              loading={loadingRooms}
              page={roomListPage}
              roomsPerPage={ROOMS_PER_PAGE}
              onPageChange={setRoomListPage}
              onJoinRoom={handleJoinSpecificRoom}
              variant="lobby"
            />
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="multiplayer-container">
      {/* 连接状态指示器 */}
      {isJoined && connectionStatus !== 'connected' && (
        <div className={`connection-status ${connectionStatus}`}>
          <div className="connection-status-content">
            {connectionStatus === 'reconnecting' && (
              <>
                <i className="fas fa-sync fa-spin"></i>
                <span>连接断开，正在重连... ({reconnectAttemptsRef.current}/{maxReconnectAttempts})</span>
              </>
            )}
            {connectionStatus === 'failed' && (
              <>
                <i className="fas fa-exclamation-triangle"></i>
                <span>连接失败，请刷新页面重试</span>
              </>
            )}
            {connectionStatus === 'disconnected' && (
              <>
                {/* 与其它同类型提醒保持一致的图标样式 */}
                <i className="fas fa-exclamation-circle"></i>
                <span>连接已断开</span>
              </>
            )}
          </div>
        </div>
      )}
      {/* 添加踢出通知 */}
      {kickNotification && (
        <div className={`kick-notification ${kickNotification.type ? `${kickNotification.type}-notification` : ''}`}>
          <div className="kick-notification-content">
            <i className={`fas ${kickNotification.type === 'host' ? 'fa-crown' : kickNotification.type === 'reconnect' ? 'fa-wifi' : kickNotification.type === 'warning' ? 'fa-exclamation-triangle' : 'fa-exclamation-circle'}`}></i>
            <span>{kickNotification.message}</span>
          </div>
        </div>
      )}
      {confirmDialog && (
        <div className="confirm-dialog-backdrop" role="presentation" onMouseDown={() => setConfirmDialog(null)}>
          <div className="confirm-dialog" role="dialog" aria-modal="true" onMouseDown={event => event.stopPropagation()}>
            <div className="confirm-dialog-message">{confirmDialog.message}</div>
            <div className="confirm-dialog-actions">
              <button className="secondary-btn" onClick={() => setConfirmDialog(null)}>
                取消
              </button>
              <button
                className="primary-btn danger-btn"
                onClick={() => {
                  const onConfirm = confirmDialog.onConfirm;
                  setConfirmDialog(null);
                  onConfirm?.();
                }}
              >
                确定
              </button>
            </div>
          </div>
        </div>
      )}
      <button
        type="button"
        className="social-link floating-back-button"
        title="Back"
        onClick={() => navigate('/')}
      >
        &larr;
      </button>
      <button
        type="button"
        className="social-link floating-feedback-button"
        title="Bug/标签反馈"
        onClick={() => setShowFeedbackPopup(true)}
      >
        📝
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
          
          <Roulette />
          <Leaderboard />
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
                <div className="host-controls">
                  <div className="room-url-container">
                    {isPublic && (
                      <input
                        type="text"
                        value={roomName}
                        placeholder="房间名（可选）"
                        maxLength={15}
                        className="room-name-input"
                        onChange={handleRoomNameChange}
                        onBlur={handleRoomNameBlur}
                        onKeyDown={handleRoomNameKeyDown}
                      />
                    )}
                    <input
                      type="text"
                      value={roomUrl}
                      readOnly
                      className="room-url-input"
                    />
                    <button onClick={copyRoomUrl} className="copy-button">复制</button>
                  </div>
                </div>
              )}
              {isHost && !waitingForAnswer && (
                <div className="host-game-controls">
                  <div className="button-group">
                    <div className="button-row">
                      <button
                        onClick={() => setShowSettings(true)}
                        className="settings-button"
                      >
                        设置
                      </button>
                      <button
                        onClick={handleVisibilityToggle}
                        className="visibility-button"
                      >
                        {isPublic ? '🔓公开' : '🔒私密'}
                      </button>
                      <button
                        onClick={handleStartGame}
                        className="start-game-button"
                        disabled={isGameStarting || players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
                      >
                        {isGameStarting ? '正在启动...' : '开始'}
                      </button>
                      <button
                        onClick={handleManualMode}
                        className={`manual-mode-button ${isManualMode ? 'active' : ''}`}
                        disabled={players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
                      >
                        有人想出题？
                      </button>
                    </div>
                  </div>
                </div>
              )}
              {!isHost && (
                <>
                  {/* 调试信息*/}
                  {/* <pre style={{ fontSize: '12px', color: '#666', padding: '5px', background: '#f5f5f5' }}>
                    {JSON.stringify({...gameSettings, __debug: '显示原始数据用于调试'}, null, 2)}
                  </pre> */}
                  <GameSettingsDisplay settings={gameSettings} />
                </>
              )}
            </>
          )}

          {isGameStarted && !globalGameEnd && (
            // In game
            <div className="container">
              {!isAnswerSetter && !isTeamObserver ? (
                // Regular player view
                <>
                  <SearchBar
                    onCharacterSelect={handleCharacterSelect}
                    isGuessing={isGuessing || waitingForSync}
                    gameEnd={gameEnd}
                    subjectSearch={gameSettings.subjectSearch}
                    finishInit={isGameStarted}
                  />
                  {/* 同步模式等待提示 */}
                  {gameSettings.syncMode && (
                    <div className="sync-waiting-banner">
                      {(() => {
                        const filtered = getFilteredSyncStatus();
                        const completed = filtered.filter(p => p.completed).length;
                        const total = filtered.length;
                        return (
                          <span>⏳ 同步模式 - 第 {syncStatus.round || 1} 轮 ({completed}/{total})</span>
                        );
                      })()}
                      <div className="sync-status">
                        {getFilteredSyncStatus().map((player, idx) => (
                          <span key={player.id} className={`sync-player ${player.completed ? 'done' : 'waiting'}`}>
                            {showNames ? player.username : `玩家${idx + 1}`}: {player.completed ? '✓' : '...'}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {/* 血战模式进度显示 */}
                  {gameSettings.nonstopMode && (
                    <div className="nonstop-progress-banner">
                      <span>🔥 血战模式 - 剩余 {nonstopProgress?.remainingCount ?? players.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected).length}/{nonstopProgress?.totalCount ?? players.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected).length} 人</span>
                          {nonstopProgress?.winners && nonstopProgress.winners.length > 0 && (
                        <div className="nonstop-winners">
                          {nonstopProgress.winners.map((winner, idx) => (
                            <span key={winner.username} className="nonstop-winner">
                              #{winner.rank} {showNames ? winner.username : `玩家${idx + 1}`} (+{winner.score}分)
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                  {gameSettings.timeLimit && !gameEnd && !waitingForSync && (
                    <Timer
                      timeLimit={gameSettings.timeLimit}
                      onTimeUp={handleTimeUp}
                      isActive={!isGuessing && !waitingForSync && !isObserver && !isAnswerSetter && !canShowSelectedAnswer}
                      reset={shouldResetTimer}
                    />
                  )}
                  <div className="game-info">
                    <div className="guesses-left">
                      <span>剩余猜测次数: {guessesLeft}</span>
                      <button
                        className="surrender-button"
                        onClick={handleSurrender}
                        disabled={isObserver || gameEnd}
                      >
                        投降 🏳️
                      </button>
                    </div>
                    {Array.isArray(gameSettings.useHints) && gameSettings.useHints.length > 0 && hints && hints.length > 0 && (
                      <div className="hints">
                        {gameSettings.useHints.map((val, idx) => (
                          guessesLeft <= val && hints[idx] && (
                            <div className="hint" key={idx}>提示{idx+1}: {hints[idx]}</div>
                          )
                        ))}
                      </div>
                    )}
                    {guessesLeft <= useImageHint && imgHint &&(
                      <div className="hint-container">
                        <Image src={imgHint} preferSource style={{height: '200px', filter: `blur(${guessesLeft}px)`}} alt="提示" />
                      </div>
                    )}
                  </div>
                  <GuessesTable
                    guesses={guesses}
                    gameSettings={gameSettings}
                    answerCharacter={answerCharacter}
                    bannedTags={bannedSharedTags}
                  />
                </>
              ) : (
                // Answer setter view
                <div className="answer-setter-view">
                  {canShowSelectedAnswer && answerCharacter && (isAnswerSetter || isTeamObserver) && (
                    <div className="selected-answer">
                      <Image src={answerCharacter.imageGrid} preferSource alt={answerCharacter.name} className="answer-image" />
                      <div className="answer-info">
                        <div>{answerCharacter.name}</div>
                        <div>{answerCharacter.nameCn}</div>
                      </div>
                    </div>
                  )}
                  {/* 血战模式进度显示（出题人视角）  */}
                  {gameSettings.nonstopMode && (
                    <div className="nonstop-progress-banner">
                      <span>🔥 血战模式 - 剩余 {nonstopProgress?.remainingCount ?? players.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected).length}/{nonstopProgress?.totalCount ?? players.filter(p => !p.isAnswerSetter && p.team !== '0' && !p.disconnected).length} 人</span>
                      {nonstopProgress?.winners && nonstopProgress.winners.length > 0 && (
                        <div className="nonstop-winners">
                          {nonstopProgress.winners.map((winner) => (
                            <span key={winner.username} className="nonstop-winner">
                              #{winner.rank} {winner.username} (+{winner.score}分)
                            </span>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                  {/* 同步模式进度显示（出题人/旁观者视角） */}
                  {gameSettings.syncMode && (
                    <div className="sync-waiting-banner">
                      {(() => {
                        const filtered = getFilteredSyncStatus();
                        const completed = filtered.filter(p => p.completed).length;
                        const total = filtered.length;
                        return (
                          <span>⏳ 同步模式 - 第 {syncStatus.round || 1} 轮 ({completed}/{total})</span>
                        );
                      })()}
                      <div className="sync-status">
                        {getFilteredSyncStatus().map((player, idx) => (
                          <span key={player.id} className={`sync-player ${player.completed ? 'done' : 'waiting'}`}>
                            {showNames ? player.username : `玩家${idx + 1}`}: {player.completed ? '✓' : '...'}
                          </span>
                        ))}
                      </div>
                    </div>
                  )}
                  {/* Switch for 简单/详细 */}
                  <div style={{ margin: '10px 0', textAlign: 'center', display: 'flex', justifyContent: 'center', alignItems: 'center', gap: '8px' }}>
                    <button
                      className={answerViewMode === 'simple' ? 'active' : ''}
                      style={{ padding: '4px 12px', borderRadius: 6, border: '1px solid #ccc', background: answerViewMode === 'simple' ? '#e0e0e0' : '#fff', cursor: 'pointer', color: 'inherit' }}
                      onClick={() => setAnswerViewMode('simple')}
                    >
                      {(isObserver && !isTeamObserver && !isAnswerSetter) ? '旁观' : '简单'}
                    </button>
                    <button
                      className={answerViewMode === 'detailed' ? 'active' : ''}
                      style={{ padding: '4px 12px', borderRadius: 6, border: '1px solid #ccc', background: answerViewMode === 'detailed' ? '#e0e0e0' : '#fff', cursor: 'pointer', color: 'inherit'}}
                      onClick={() => setAnswerViewMode('detailed')}
                    >
                      {(isObserver && !isTeamObserver && !isAnswerSetter) ? '我的' : '详细'}
                    </button>
                    <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginLeft: '8px' }}>
                      <div 
                        className={`toggle-switch ${isGuessTableCollapsed ? 'active' : ''}`}
                        style={{
                          position: 'relative',
                          width: '44px',
                          height: '24px',
                          borderRadius: '12px',
                          backgroundColor: isGuessTableCollapsed ? '#3b82f6' : '#e5e7eb',
                          cursor: 'pointer',
                          transition: 'background-color 0.2s'
                        }}
                        onClick={() => setIsGuessTableCollapsed(!isGuessTableCollapsed)}
                      >
                        <div 
                          className="toggle-thumb"
                          style={{
                            position: 'absolute',
                            top: '2px',
                            left: '2px',
                            width: '20px',
                            height: '20px',
                            borderRadius: '50%',
                            backgroundColor: 'white',
                            transition: 'transform 0.2s',
                            boxShadow: '0 1px 3px rgba(0,0,0,0.1)',
                            transform: isGuessTableCollapsed ? 'translateX(20px)' : 'translateX(0)'
                          }}
                        />
                      </div>
                      <span style={{ fontSize: '14px', color: '#475569' }}>
                        只显示最新3条
                      </span>
                    </div>
                  </div>
                  {answerViewMode === 'simple' ? (
                    <div className="guess-history-table">
                      <table>
                        <thead>
                          <tr>
                            {guessesHistory.map((playerGuesses, index) => (
                              <th key={playerGuesses.username}>
                                {showNames ? playerGuesses.username : `玩家${index + 1}`}
                              </th>
                            ))}
                          </tr>
                        </thead>
                        <tbody>
                          {(() => {
                            // 折叠时每个玩家只显示最新3条，需要计算每个玩家的显示范围
                            const collapsedLimit = 3;
                            const displayData = guessesHistory.map(playerGuesses => {
                              const total = playerGuesses.guesses.length;
                              const startIdx = isGuessTableCollapsed ? Math.max(0, total - collapsedLimit) : 0;
                              return {
                                username: playerGuesses.username,
                                displayGuesses: playerGuesses.guesses.slice(startIdx)
                              };
                            });
                            const maxDisplayRows = Math.max(...displayData.map(d => d.displayGuesses.length), 0);
                            return Array.from({ length: maxDisplayRows }).map((_, rowIndex) => (
                              <tr key={rowIndex}>
                                {displayData.map(playerData => (
                                  <td key={playerData.username}>
                                    {playerData.displayGuesses[rowIndex] && (
                                      <>
                                        <Image className="character-icon" src={playerData.displayGuesses[rowIndex].guessData.image} alt={playerData.displayGuesses[rowIndex].guessData.name} />
                                        <div className="character-name">{playerData.displayGuesses[rowIndex].guessData.name}</div>
                                        <div className="character-name-cn">{playerData.displayGuesses[rowIndex].guessData.nameCn}</div>
                                      </>
                                    )}
                                  </td>
                                ))}
                              </tr>
                            ));
                          })()}
                        </tbody>
                      </table>
                    </div>
                  ) : (
                    <div style={{ marginTop: 12 }}>
                      <GuessesTable
                        guesses={guesses}
                        gameSettings={gameSettings}
                        answerCharacter={answerCharacter}
                        collapsedCount={isGuessTableCollapsed ? 3 : 0}
                        bannedTags={bannedSharedTags}
                      />
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

          {!isGameStarted && globalGameEnd && (
            // After game ends
            <div className="game-end-view-container">
              {isHost && (
                <>
                  <div className="host-controls">
                    <div className="room-url-container">
                      {isPublic && (
                        <input
                          type="text"
                          value={roomName}
                          placeholder="房间名（可选）"
                          maxLength={15}
                          className="room-name-input"
                          onChange={handleRoomNameChange}
                          onBlur={handleRoomNameBlur}
                          onKeyDown={handleRoomNameKeyDown}
                        />
                      )}
                      <input
                        type="text"
                        value={roomUrl}
                        readOnly
                        className="room-url-input"
                      />
                      <button onClick={copyRoomUrl} className="copy-button">复制</button>
                    </div>
                  </div>
                  <div className="host-game-controls">
                    <div className="button-group">
                      <div className="button-row">
                        <button
                          onClick={() => setShowSettings(true)}
                          className="settings-button"
                        >
                          设置
                        </button>
                        <button
                          onClick={handleVisibilityToggle}
                          className="visibility-button"
                        >
                          {isPublic ? '🔓公开' : '🔒私密'}
                        </button>
                        <button
                          onClick={handleStartGame}
                          className="start-game-button"
                          disabled={players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
                        >
                          开始
                        </button>
                        <button
                          onClick={handleManualMode}
                          className={`manual-mode-button ${isManualMode ? 'active' : ''}`}
                          disabled={players.length < 2 || players.some(p => !p.isHost && !p.ready && !p.disconnected) || allSpectators}
                        >
                          有人想出题？
                        </button>
                      </div>
                    </div>
                  </div>
                </>
              )}
              <div className="game-end-message-table-wrapper">
                <table className="game-end-message-table">
                  <thead>
                    <tr>
                      <th className="game-end-header-cell">
                        <div className="game-end-header-content">
                          <div className="mode-tags">
                            {!displaySettings.nonstopMode && !displaySettings.syncMode && !displaySettings.globalPick && !displaySettings.tagBan && (
                              <span className="mode-tag normal">普通模式</span>
                            )}
                            {displaySettings.nonstopMode && (
                              <span className="mode-tag nonstop">血战模式</span>
                            )}
                            {displaySettings.syncMode && (
                              <span className="mode-tag sync">同步模式</span>
                            )}
                            {displaySettings.globalPick && (
                              <span className="mode-tag global-bp">角色全局BP</span>
                            )}
                            {displaySettings.tagBan && (
                              <span className="mode-tag tag-ban">标签全局BP</span>
                            )}
                          </div>
                          <span className="answer-label">答案是</span>
                          {(() => {
                            // 判断当前玩家是否猜对
                            const currentPlayer = players.find(p => p.id === socket?.id);
                            const isObserver = currentPlayer?.team === '0';
                            const isCurrentPlayerWin = isWinnerResult(currentPlayer?.roundResult);
                            const isCurrentPlayerLose = Boolean(currentPlayer?.roundResult) && !isCurrentPlayerWin;
                            let answerButtonClass = 'answer-character-button';
                            if (isObserver) {
                              answerButtonClass = 'answer-character-button';
                            } else if (isCurrentPlayerWin) {
                              answerButtonClass = 'answer-character-button win';
                            } else if (isCurrentPlayerLose) {
                              answerButtonClass = 'answer-character-button lose';
                            }
                            return (
                              <button
                                className={answerButtonClass}
                                onClick={() => setShowCharacterPopup(true)}
                              >
                                {answerCharacter.nameCn || answerCharacter.name}
                              </button>
                            );
                          })()}
                          {/* 出题人信息（如果存在） */}
                          {(() => {
                            const setterInfo = scoreDetails?.find(item => item.type === 'setter');
                            if (!setterInfo) return null;
                            const scoreText = setterInfo.score >= 0 ? `+${setterInfo.score}分` : `${setterInfo.score}分`;
                            const boxClass = setterInfo.score > 0 ? 'player-score-box positive' : setterInfo.score < 0 ? 'player-score-box negative' : 'player-score-box';
                            const scoreClass = setterInfo.score > 0 ? 'positive' : setterInfo.score < 0 ? 'negative' : '';
                            return (
                              <span className="setter-info-inline">
                                ，出题人
                                <span className={boxClass}>
                                  <span className="player-name">{showNames ? setterInfo.username : '**'}</span>
                                  <span className={`score-value ${scoreClass}`}>
                                    {scoreText}
                                  </span>
                                  {setterInfo.reason && <span className="score-breakdown">{setterInfo.reason}</span>}
                                </span>
                              </span>
                            );
                          })()}
                          {scoreDetails && scoreDetails.length > 0 && (
                            <span className="score-details-title">，得分详情：</span>
                          )}
                        </div>
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr>
                      <td className="game-end-body-cell">
                        {/* 详细得分统计列表 */}
                        {scoreDetails && scoreDetails.length > 0 && (
                          <div className="score-details-list">
                            {(() => {
                              // 过滤出非出题人的条目，按得分降序排序
                              const sortedDetails = scoreDetails
                                .filter(item => item.type !== 'setter')
                                .sort((a, b) => {
                                  const scoreA = a.type === 'team' ? a.teamScore : a.score;
                                  const scoreB = b.type === 'team' ? b.teamScore : b.score;
                                  return scoreB - scoreA;
                                });
                              
                              return sortedDetails.map((item, idx) => {
                                const rank = idx + 1;
                                if (item.type === 'team') {
                                  // 团队得分
                                  const scoreText = item.teamScore >= 0 ? `+${item.teamScore}分` : `${item.teamScore}分`;
                                  const scoreClass = item.teamScore > 0 ? 'positive' : item.teamScore < 0 ? 'negative' : '';
                                  const boxClass = item.teamScore > 0 ? 'player-score-box positive' : item.teamScore < 0 ? 'player-score-box negative' : 'player-score-box';
                                  
                                  // 构建队伍成员得分明细
                                  const memberDetails = item.members.map((m, mIdx) => {
                                    const memberScore = m.score >= 0 ? `+${m.score}` : `${m.score}`;
                                    const reasonParts = [];
                                    if (m.breakdown?.base) reasonParts.push(`基础${m.breakdown.base > 0 ? '+' : ''}${m.breakdown.base}`);
                                    if (m.breakdown?.bigWin) reasonParts.push(`大赢家+${m.breakdown.bigWin}`);
                                    if (m.breakdown?.quickGuess) reasonParts.push(`好快的猜+${m.breakdown.quickGuess}`);
                                    if (m.breakdown?.partial) reasonParts.push(`作品分+${m.breakdown.partial}`);
                                    const reasonText = reasonParts.length > 0 ? `(${reasonParts.join(' ')})` : '';
                                    const displayName = showNames ? m.username : `成员${mIdx + 1}`;
                                    return `${displayName}${memberScore}${reasonText}`;
                                  }).join(' ');
                                  
                                  return (
                                    <span key={`team-${item.teamId}`} className={boxClass}>
                                      <span className="player-rank">{rank}.</span>
                                      <span className="player-name">{showNames ? `队伍${item.teamId}` : `队伍${rank}`}</span>
                                      <span className={`score-value ${scoreClass}`}>{scoreText}</span>
                                      {memberDetails && <span className="score-breakdown">{memberDetails}</span>}
                                    </span>
                                  );
                                } else {
                                  // 个人得分 - 单行圆角矩形显示
                                  const scoreText = item.score >= 0 ? `+${item.score}分` : `${item.score}分`;
                                  const scoreClass = item.score > 0 ? 'positive' : item.score < 0 ? 'negative' : '';
                                  const boxClass = item.score > 0 ? 'player-score-box positive' : item.score < 0 ? 'player-score-box negative' : 'player-score-box';
                                  
                                  // 构建得分明细
                                  const breakdownParts = [];
                                  if (item.breakdown?.base) breakdownParts.push(`基础${item.breakdown.base > 0 ? '+' : ''}${item.breakdown.base}`);
                                  if (item.breakdown?.bigWin) breakdownParts.push(`大赢家+${item.breakdown.bigWin}`);
                                  if (item.breakdown?.quickGuess) breakdownParts.push(`好快的猜+${item.breakdown.quickGuess}`);
                                  if (item.breakdown?.partial) breakdownParts.push(`作品分+${item.breakdown.partial}`);
                                  const breakdownText = breakdownParts.length > 0 ? breakdownParts.join(' ') : '';
                                  
                                  return (
                                    <span key={item.id || idx} className={boxClass}>
                                      <span className="player-rank">{rank}.</span>
                                      <span className="player-name">{showNames ? item.username : `玩家${rank}`}</span>
                                      <span className={`score-value ${scoreClass}`}>{scoreText}</span>
                                      {breakdownText && <span className="score-breakdown">{breakdownText}</span>}
                                    </span>
                                  );
                                }
                              });
                            })()}
                          </div>
                        )}
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <div className="game-end-container">
                {!isHost && (
                  <>
                    {/* 调试信息*/}
                    {/* <pre style={{ fontSize: '12px', color: '#666', padding: '5px', background: '#f5f5f5' }}>
                      {JSON.stringify({...gameSettings, __debug: '显示原始数据用于调试'}, null, 2)}
                    </pre> */}
                    <GameSettingsDisplay settings={gameSettings} />
                  </>
                )}
                <div className="guess-history-table">
                  <table>
                    <thead>
                      <tr>
                        {guessesHistory.map((playerGuesses, index) => (
                          <th key={playerGuesses.username}>
                            {showNames ? playerGuesses.username : `玩家${index + 1}`}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {Array.from({ length: Math.max(...guessesHistory.map(g => g.guesses.length)) }).map((_, rowIndex) => (
                        <tr key={rowIndex}>
                          {guessesHistory.map(playerGuesses => (
                            <td key={playerGuesses.username}>
                              {playerGuesses.guesses[rowIndex] && (
                                <>
                                  <Image className="character-icon" src={playerGuesses.guesses[rowIndex].guessData.image} alt={playerGuesses.guesses[rowIndex].guessData.name} />
                                  <div className="character-name">{playerGuesses.guesses[rowIndex].guessData.name}</div>
                                  <div className="character-name-cn">{playerGuesses.guesses[rowIndex].guessData.nameCn}</div>
                                </>
                              )}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            </div>
          )}

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
              gameSettings={gameSettings}
            />
          )}
        </>

      )}
      {showFeedbackPopup && (
        <FeedbackPopup
          onClose={() => setShowFeedbackPopup(false)}
          onSubmit={handleFeedbackSubmit}
        />
      )}
    </div>
  );
};

export default Multiplayer;
