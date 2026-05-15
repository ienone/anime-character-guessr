import { lazy, Suspense, useEffect, useState, useRef } from 'react';
import { getRandomCharacter, getCharacterAppearances, generateFeedback } from '../utils/bangumi';
import SearchBar from '../components/SearchBar';
import GuessesTable from '../components/GuessesTable';
import SocialLinks from '../components/SocialLinks';
import GameInfo from '../components/GameInfo';
import Timer from '../components/Timer';
import Icon from '../components/Icon';
import logCollector from '../utils/logCollector';
import { notify } from '../utils/notifications';
import '../styles/game.css';
import '../styles/SinglePlayer.css';
import '../styles/social.css';
import axios from 'axios';
import { useLocalStorage } from 'usehooks-ts';

const SettingsPopup = lazy(() => import('../components/SettingsPopup'));
const HelpPopup = lazy(() => import('../components/HelpPopup'));
const GameEndPopup = lazy(() => import('../components/GameEndPopup'));
const FeedbackPopup = lazy(() => import('../components/FeedbackPopup'));
const SERVER_URL = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '');

function SinglePlayer() {
  const [guesses, setGuesses] = useState([]);
  const [guessesLeft, setGuessesLeft] = useState(10);
  const [isGuessing, setIsGuessing] = useState(false);
  const [gameEnd, setGameEnd] = useState(false);
  const [gameEndPopup, setGameEndPopup] = useState(null);
  const [answerCharacter, setAnswerCharacter] = useState(null);
  const [settingsPopup, setSettingsPopup] = useState(false);
  const [helpPopup, setHelpPopup] = useState(false);
  const [finishInit, setFinishInit] = useState(false);
  const [initFailed, setInitFailed] = useState(false);
  const [shouldResetTimer, setShouldResetTimer] = useState(false);
  const [hints, setHints] = useState([]);
  const [imgHint, setImgHint] = useState(null);
  const [useImageHint, setUseImageHint] = useState(0);
  const [showFeedbackPopup, setShowFeedbackPopup] = useState(false);
  const [isGameRestarting, setIsGameRestarting] = useState(false); // 防止重复点击"再玩一次"
  const [gameSettings, setGameSettings] = useLocalStorage('singleplayer-game-settings', {
    startYear: new Date().getFullYear()-10,
    endYear: new Date().getFullYear(),
    useSubjectPerYear: false,
    topNSubjects: 0,
    metaTags: ["", "", ""],
    useIndex: false,
    indexId: null,
    addedSubjects: [],
    mainCharacterOnly: true,
    characterNum: 6,
    maxAttempts: 10,
    useHints: [],
    useImageHint: 0,
    includeGame: false,
    timeLimit: null,
    subjectSearch: true,
    characterTagNum: 6,
    subjectTagNum: 6,
    commonTags: true
  });
  const [currentGameSettings, setCurrentGameSettings] = useState(gameSettings);

  // Initialize game
  useEffect(() => {
    let isMounted = true;

    const initializeGame = async () => {
      setInitFailed(false);
      setGuesses([]);
      setGuessesLeft(gameSettings.maxAttempts || 10);
      setIsGuessing(false);
      setGameEnd(false);
      setGameEndPopup(null);
      setAnswerCharacter(null);
      setShouldResetTimer(true);
      setFinishInit(false);
      setHints([]);
      setImgHint(null);
      setUseImageHint(0);
      try {
        if (Array.isArray(gameSettings.addedSubjects) && gameSettings.addedSubjects.length > 0) {
          await axios.post(`${SERVER_URL}/api/subject-added`, {
            addedSubjects: gameSettings.addedSubjects
          });
        }
      } catch (error) {
        console.error('Failed to update subject count:', error);
      }
      try {
        const character = await getRandomCharacter(gameSettings);
        setCurrentGameSettings({ ...gameSettings });
        if (isMounted) {
          setAnswerCharacter(character);
          setGuessesLeft(gameSettings.maxAttempts || 10);
          // Prepare hints based on settings
          let hintTexts = [];
          if (Array.isArray(gameSettings.useHints) && gameSettings.useHints.length > 0 && character.summary) {
            const sentences = character.summary.replace('[mask]', '').replace('[/mask]','')
              .split(/[。、，。！？ ""]/).filter(s => s.trim());
            if (sentences.length > 0) {
              // Randomly select as many hints as needed
              const selectedIndices = new Set();
              while (selectedIndices.size < Math.min(gameSettings.useHints.length, sentences.length)) {
                selectedIndices.add(Math.floor(Math.random() * sentences.length));
              }
              hintTexts = Array.from(selectedIndices).map(i => "……"+sentences[i].trim()+"……");
            }
          }
          setHints(hintTexts);
          setUseImageHint(gameSettings.useImageHint);
          setImgHint(gameSettings.useImageHint > 0 ? character.image : null);
          setFinishInit(true);
          setInitFailed(false);
        }
      } catch (error) {
        console.error('Failed to initialize game:', error);
        if (isMounted) {
          const message = error?.response?.data?.message || error?.message || '游戏初始化失败，请刷新页面重试，或在设置里清理缓存';
          notify(message, 'error');
          setInitFailed(true);
          setFinishInit(false);
        }
      }
    };

    initializeGame();

    return () => {
      isMounted = false;
    };
  }, [gameSettings]);

  const handleCharacterSelect = async (character) => {
    if (isGuessing || !answerCharacter) return;

    setIsGuessing(true);
    setShouldResetTimer(true);
    if (character.id === 56822 || character.id === 56823) {
      notify('有点意思');
    }

    try {
      const appearances = await getCharacterAppearances(character.id, currentGameSettings);

      const guessData = {
        ...character,
        ...appearances
      };

      const isCorrect = guessData.id === answerCharacter.id;
      const newGuessesLeft = guessesLeft - 1;

      if (isCorrect) {
        setGuessesLeft(newGuessesLeft);
        setGuesses(prevGuesses => [...prevGuesses, {
          id: guessData.id,
          icon: guessData.image,
          name: guessData.name,
          nameCn: guessData.nameCn,
          nameEn: guessData.nameEn,
          gender: guessData.gender,
          genderFeedback: 'yes',
          latestAppearance: guessData.latestAppearance,
          latestAppearanceFeedback: '=',
          earliestAppearance: guessData.earliestAppearance,
          earliestAppearanceFeedback: '=',
          highestRating: guessData.highestRating,
          ratingFeedback: '=',
          appearancesCount: guessData.appearances.length,
          appearancesCountFeedback: '=',
          popularity: guessData.popularity,
          popularityFeedback: '=',
          appearanceIds: guessData.appearanceIds,
          sharedAppearances: {
            first: appearances.appearances[0] || '',
            count: appearances.appearances.length
          },
          metaTags: guessData.metaTags,
          sharedMetaTags: guessData.metaTags,
          isAnswer: true
        }]);

        setGameEnd(true);
        setGameEndPopup({
          result: 'win',
          answer: answerCharacter
        });
      } else if (newGuessesLeft <= 0) {
        const feedback = await generateFeedback(guessData, answerCharacter, currentGameSettings);
        setGuessesLeft(newGuessesLeft);
        setGuesses(prevGuesses => [...prevGuesses, {
          id: guessData.id,
          icon: guessData.image,
          name: guessData.name,
          nameCn: guessData.nameCn,
          nameEn: guessData.nameEn,
          gender: guessData.gender,
          genderFeedback: feedback.gender.feedback,
          latestAppearance: guessData.latestAppearance,
          latestAppearanceFeedback: feedback.latestAppearance.feedback,
          earliestAppearance: guessData.earliestAppearance,
          earliestAppearanceFeedback: feedback.earliestAppearance.feedback,
          highestRating: guessData.highestRating,
          ratingFeedback: feedback.rating.feedback,
          appearancesCount: guessData.appearances.length,
          appearancesCountFeedback: feedback.appearancesCount.feedback,
          popularity: guessData.popularity,
          popularityFeedback: feedback.popularity.feedback,
          appearanceIds: guessData.appearanceIds,
          sharedAppearances: feedback.shared_appearances,
          metaTags: feedback.metaTags.guess,
          sharedMetaTags: feedback.metaTags.shared,
          isAnswer: false
        }]);

        setGameEnd(true);
        setGameEndPopup({
          result: 'lose',
          answer: answerCharacter
        });
      } else {
        const feedback = await generateFeedback(guessData, answerCharacter, currentGameSettings);
        setGuessesLeft(newGuessesLeft);
        setGuesses(prevGuesses => [...prevGuesses, {
          id: guessData.id,
          icon: guessData.image,
          name: guessData.name,
          nameCn: guessData.nameCn,
          nameEn: guessData.nameEn,
          gender: guessData.gender,
          genderFeedback: feedback.gender.feedback,
          latestAppearance: guessData.latestAppearance,
          latestAppearanceFeedback: feedback.latestAppearance.feedback,
          earliestAppearance: guessData.earliestAppearance,
          earliestAppearanceFeedback: feedback.earliestAppearance.feedback,
          highestRating: guessData.highestRating,
          ratingFeedback: feedback.rating.feedback,
          appearancesCount: guessData.appearances.length,
          appearancesCountFeedback: feedback.appearancesCount.feedback,
          popularity: guessData.popularity,
          popularityFeedback: feedback.popularity.feedback,
          appearanceIds: guessData.appearanceIds,
          sharedAppearances: feedback.shared_appearances,
          metaTags: feedback.metaTags.guess,
          sharedMetaTags: feedback.metaTags.shared,
          isAnswer: false
        }]);
      }
    } catch (error) {
      console.error('Error processing guess:', error);
      notify('出错了，请重试', 'error');
    } finally {
      setIsGuessing(false);
      setShouldResetTimer(false);
    }
  };

  const handleSettingsChange = (setting, value) => {
    if (typeof setting === 'object' && setting !== null) {
      setGameSettings(prev => ({
        ...prev,
        ...setting
      }));
      return;
    }

    setGameSettings(prev => ({
      ...prev,
      [setting]: value
    }));
  };

  const handleRestartWithSettings = async () => {
    // 防止重复点击："再玩一次"按钮
    if (isGameRestarting) return;
    
    setIsGameRestarting(true);
    
    try {
      setGuesses([]);
      setGuessesLeft(gameSettings.maxAttempts);
      setIsGuessing(false);
      setGameEnd(false);
      setGameEndPopup(null);
      setAnswerCharacter(null);
      setSettingsPopup(false);
      setShouldResetTimer(true);
      setFinishInit(false);
      setInitFailed(false);
      setHints([]);
      setImgHint(null);
      setUseImageHint(0);

      try {
        if (Array.isArray(gameSettings.addedSubjects) && gameSettings.addedSubjects.length > 0) {
          await axios.post(`${SERVER_URL}/api/subject-added`, {
            addedSubjects: gameSettings.addedSubjects
          });
        }
      } catch (error) {
        console.error('Failed to update subject count:', error);
      }
      try {
        setCurrentGameSettings({ ...gameSettings });
        const character = await getRandomCharacter(gameSettings);
        setAnswerCharacter(character);
        // Prepare hints based on settings for new game
        let hintTexts = [];
        if (Array.isArray(gameSettings.useHints) && gameSettings.useHints.length > 0 && character.summary) {
          const sentences = character.summary.replace('[mask]', '').replace('[/mask]','')
            .split(/[。、，。！？ ""]/).filter(s => s.trim());
          if (sentences.length > 0) {
            const selectedIndices = new Set();
            while (selectedIndices.size < Math.min(gameSettings.useHints.length, sentences.length)) {
              selectedIndices.add(Math.floor(Math.random() * sentences.length));
            }
            hintTexts = Array.from(selectedIndices).map(i => "……"+sentences[i].trim()+"……");
          }
        }
        setHints(hintTexts);
        setUseImageHint(gameSettings.useImageHint);
        setImgHint(gameSettings.useImageHint > 0 ? character.image : null);
        setFinishInit(true);
        setInitFailed(false);
      } catch (error) {
        console.error('Failed to initialize new game:', error);
        const message = error?.response?.data?.message || error?.message || '游戏初始化失败，请刷新页面重试，或在设置里清理缓存';
        notify(message, 'error');
        setInitFailed(true);
      }
    } finally {
      setIsGameRestarting(false);
    }
  };

  const timeUpRef = useRef(false);

  const handleTimeUp = () => {
    if (timeUpRef.current) return; // prevent multiple triggers
    timeUpRef.current = true;

    setGuessesLeft(prev => {
      const newGuessesLeft = prev - 1;
      if (newGuessesLeft <= 0) {
        setGameEnd(true);
        setGameEndPopup({
          result: 'lose',
          answer: answerCharacter
        });
      }
      return newGuessesLeft;
    });
    setShouldResetTimer(true);
    setTimeout(() => {
      setShouldResetTimer(false);
      timeUpRef.current = false;
    }, 100);
  };

  const handleSurrender = () => {
    if (gameEnd) return;

    setGameEnd(true);
    setGameEndPopup({
      result: 'lose',
      answer: answerCharacter
    });
    notify('已投降！查看角色详情', 'warning');
  };

  const handleFeedbackSubmit = async ({ type, description, includeLogs }) => {
    const payload = {
      bugType: type,
      description,
    };

    if (includeLogs) {
      payload.logs = logCollector.getLogs();
      payload.errors = logCollector.getErrors();
      payload.diagnosticData = logCollector.getDiagnosticData();
    }

    await axios.post(`${SERVER_URL}/api/bug-feedback`, payload);
  };

  return (
    <div className="single-player-container">
      <button
        type="button"
        className="social-link floating-feedback-button"
        title="Bug/标签反馈"
        onClick={() => setShowFeedbackPopup(true)}
      >
        <Icon name="exclamationCircle" />
      </button>

      <SocialLinks
        onSettingsClick={() => setSettingsPopup(true)}
        onHelpClick={() => setHelpPopup(true)}
        onFeedbackClick={() => setShowFeedbackPopup(true)}
        showFeedbackInline={true}
      />

      <div className="search-bar">
        <SearchBar
          onCharacterSelect={handleCharacterSelect}
          isGuessing={isGuessing}
          gameEnd={gameEnd}
          subjectSearch={currentGameSettings.subjectSearch}
          finishInit={finishInit}
        />
      </div>

      {currentGameSettings.timeLimit && (
        <Timer
          timeLimit={currentGameSettings.timeLimit}
          onTimeUp={handleTimeUp}
          isActive={!gameEnd && !isGuessing}
          reset={shouldResetTimer}
        />
      )}

      <GameInfo
        gameEnd={gameEnd}
        guessesLeft={guessesLeft}
        onRestart={handleRestartWithSettings}
        answerCharacter={answerCharacter}
        finishInit={finishInit}
        initFailed={initFailed}
        hints={hints}
        useImageHint={useImageHint}
        imgHint = {imgHint}
        useHints={currentGameSettings.useHints}
        onSurrender={handleSurrender}
        isRestarting={isGameRestarting}
      />

      <GuessesTable
        guesses={guesses}
        gameSettings={currentGameSettings}
        answerCharacter={answerCharacter}
      />

      <Suspense fallback={null}>
        {settingsPopup && (
          <SettingsPopup
            gameSettings={gameSettings}
            onSettingsChange={handleSettingsChange}
            onClose={() => setSettingsPopup(false)}
          />
        )}

        {helpPopup && (
          <HelpPopup onClose={() => setHelpPopup(false)} />
        )}

        {gameEndPopup && (
          <GameEndPopup
            result={gameEndPopup.result}
            answer={gameEndPopup.answer}
            onClose={() => setGameEndPopup(null)}
          />
        )}

        {showFeedbackPopup && (
          <FeedbackPopup
            onClose={() => setShowFeedbackPopup(false)}
            onSubmit={handleFeedbackSubmit}
          />
        )}
      </Suspense>
    </div>
  );
}

export default SinglePlayer;
