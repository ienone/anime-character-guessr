import { useCallback, useMemo, useReducer } from 'react';

const initialRoundState = {
  isGameStarted: false,
  guesses: [],
  guessesLeft: 10,
  isGuessing: false,
  isGameStarting: false,
  answerCharacter: null,
  hints: [],
  useImageHint: 0,
  imgHint: null,
  shouldResetTimer: false,
  gameEnd: false,
  scoreDetails: null,
  globalGameEnd: false,
  endGameSettings: null,
  guessesHistory: [],
  showCharacterPopup: false,
  showSetAnswerPopup: false,
  showFeedbackPopup: false,
  isAnswerSetter: false,
  canShowSelectedAnswer: false,
  answerViewMode: 'simple',
  isGuessTableCollapsed: false,
  waitingForSync: false,
  syncStatus: {},
  nonstopProgress: null,
  isObserver: false,
  bannedSharedTags: []
};

function roundReducer(state, action) {
  if (action.type === 'set') {
    const current = state[action.field];
    const nextValue = typeof action.value === 'function' ? action.value(current) : action.value;
    if (Object.is(current, nextValue)) return state;
    return {
      ...state,
      [action.field]: nextValue
    };
  }

  return state;
}

function useRoundReducer() {
  const [state, dispatch] = useReducer(roundReducer, initialRoundState);

  const setField = useCallback((field, value) => {
    dispatch({ type: 'set', field, value });
  }, []);

  const setters = useMemo(() => ({
    setIsGameStarted: value => setField('isGameStarted', value),
    setGuesses: value => setField('guesses', value),
    setGuessesLeft: value => setField('guessesLeft', value),
    setIsGuessing: value => setField('isGuessing', value),
    setIsGameStarting: value => setField('isGameStarting', value),
    setAnswerCharacter: value => setField('answerCharacter', value),
    setHints: value => setField('hints', value),
    setUseImageHint: value => setField('useImageHint', value),
    setImgHint: value => setField('imgHint', value),
    setShouldResetTimer: value => setField('shouldResetTimer', value),
    setGameEnd: value => setField('gameEnd', value),
    setScoreDetails: value => setField('scoreDetails', value),
    setGlobalGameEnd: value => setField('globalGameEnd', value),
    setEndGameSettings: value => setField('endGameSettings', value),
    setGuessesHistory: value => setField('guessesHistory', value),
    setShowCharacterPopup: value => setField('showCharacterPopup', value),
    setShowSetAnswerPopup: value => setField('showSetAnswerPopup', value),
    setShowFeedbackPopup: value => setField('showFeedbackPopup', value),
    setIsAnswerSetter: value => setField('isAnswerSetter', value),
    setCanShowSelectedAnswer: value => setField('canShowSelectedAnswer', value),
    setAnswerViewMode: value => setField('answerViewMode', value),
    setIsGuessTableCollapsed: value => setField('isGuessTableCollapsed', value),
    setWaitingForSync: value => setField('waitingForSync', value),
    setSyncStatus: value => setField('syncStatus', value),
    setNonstopProgress: value => setField('nonstopProgress', value),
    setIsObserver: value => setField('isObserver', value),
    setBannedSharedTags: value => setField('bannedSharedTags', value)
  }), [setField]);

  return useMemo(() => ({
    ...state,
    ...setters
  }), [state, setters]);
}

export default useRoundReducer;
