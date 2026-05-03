import '../styles/GuessesTable.css';
import { useState, useMemo, useRef, useEffect } from 'react';
import ModifiedTagDisplay from './ModifiedTagDisplay';
import Image from './Image';
import { subjectsWithExtraTags } from '../data/extra_tag_subjects';

function GuessesTable({ guesses, answerCharacter, collapsedCount = 0, bannedTags = [], showNames = true }) {
  const [clickedExpandTags, setClickedExpandTags] = useState(new Set());
  const [externalTagMode, setExternalTagMode] = useState(false);

  const bannedTagSet = useMemo(() => {
    if (!Array.isArray(bannedTags)) {
      return new Set();
    }
    return new Set(
      bannedTags
        .filter(tag => typeof tag === 'string')
        .map(tag => tag.trim())
        .filter(Boolean)
    );
  }, [bannedTags]);


  // 如果指定了折叠数量，只显示最新的 N 条记录
  const displayGuesses = collapsedCount > 0 && guesses.length > collapsedCount
    ? guesses.slice(-collapsedCount)
    : guesses;

  // Determine if any guess could have extra tags
  const hasAnyExtraTags = displayGuesses.some(guess =>
    Array.isArray(guess.appearanceIds) && guess.appearanceIds.some(id => subjectsWithExtraTags.has(id))
  );

  const getGenderEmoji = (gender) => {
    switch (gender) {
      case 'male':
        return '♂️';
      case 'female':
        return '♀️';
      default:
        return '❓';
    }
  };

  const handleExpandTagClick = (guessIndex, tagIndex) => {
    const key = `${guessIndex}-${tagIndex}`;
    setClickedExpandTags(prev => {
      const newSet = new Set(prev);
      newSet.add(key);
      return newSet;
    });
  };

  const handleToggleMode = () => {
    setExternalTagMode((prev) => !prev);
  };

  const listRef = useRef(null);

  useEffect(() => {
    if (typeof window === 'undefined' || !listRef.current) return;

    const updateMaxHeight = () => {
      const el = listRef.current;
      if (!el) return;
      // Only apply mobile behavior on narrow viewports
      if (window.innerWidth <= 768) {
        const rect = el.getBoundingClientRect();
        const bottomSpace = 16; // margin to bottom
        const maxH = Math.max(120, window.innerHeight - rect.top - bottomSpace);
        el.style.maxHeight = `${maxH}px`;
        el.style.overflowY = 'auto';
        el.style.overscrollBehavior = 'contain';
        el.style.WebkitOverflowScrolling = 'touch';
      } else {
        // reset styles on larger screens
        el.style.maxHeight = '';
        el.style.overflowY = '';
        el.style.overscrollBehavior = '';
        el.style.WebkitOverflowScrolling = '';
      }
    };

    updateMaxHeight();
    window.addEventListener('resize', updateMaxHeight);
    window.addEventListener('orientationchange', updateMaxHeight);
    // In case the layout shifts after images load or fonts load
    const ro = new ResizeObserver(updateMaxHeight);
    ro.observe(listRef.current);

    return () => {
      window.removeEventListener('resize', updateMaxHeight);
      window.removeEventListener('orientationchange', updateMaxHeight);
      ro.disconnect();
    };
  }, [listRef]);

  return (
    <div className="table-container">
      {/* Only show toggle if any guess could have extra tags */}
      {hasAnyExtraTags && (
        <div style={{ display: 'flex', justifyContent: 'center', marginBottom: '16px' }}>
          <button
            onClick={handleToggleMode}
            style={{
              padding: '8px 24px',
              borderRadius: '24px',
              border: 'none',
              background: externalTagMode ? '#4a90e2' : '#e0e0e0',
              color: externalTagMode ? '#fff' : '#333',
              fontWeight: 'bold',
              fontSize: '16px',
              boxShadow: '0 2px 8px rgba(0,0,0,0.08)',
              cursor: 'pointer',
              transition: 'background 0.2s, color 0.2s',
              outline: 'none',
            }}
            onMouseOver={e => {
              e.target.style.background = externalTagMode ? '#006a91' : '#d0d0d0';
            }}
            onMouseOut={e => {
              e.target.style.background = externalTagMode ? '#0084B4' : '#e0e0e0';
            }}
          >
            更多标签
          </button>
        </div>
      )}
      <table className={`guesses-table${externalTagMode ? ' external-tag-mode' : ''}`}>
        <thead>
          <tr>
            <th></th>
            <th>名字</th>
            {externalTagMode ? (
              <>
                <th>性别？</th>
                <th></th>
              </>
            ) : (
              <>
                <th>性别</th>
                <th>热度</th>
                <th>作品数<br/>最高分</th>
                <th>最晚登场<br/>最早登场</th>
              </>
            )}
            <th>标签</th>
            <th>共同出演</th>
          </tr>
        </thead>
        <tbody>
          {displayGuesses.map((guess, guessIndex) => (
            <tr key={guessIndex}>
              <td data-label="头像" className="cell-icon">
                <Image src={guess.icon} alt="character" className="character-icon" />
              </td>
              <td data-label="名字" className="cell-name">
                <div className={`character-name-container ${guess.isAnswer ? 'correct' : ''}`}>
                  {guess.guessrName && (
                    <div className="character-guessr-name" style={{ fontSize: '12px', color: '#888' }}>
                      来自：{showNames ? guess.guessrName : '玩家'}
                    </div>
                  )}
                  <div className="character-name">{guess.name}</div>
                  <div className="character-name-cn">{guess.nameCn}</div>
                </div>
              </td>
              <td data-label="性别" className="cell-gender">
                <span className={`feedback-cell ${guess.genderFeedback === 'yes' ? 'correct' : ''}`}>
                  {getGenderEmoji(guess.gender)}
                </span>
              </td>
              {externalTagMode ? (
                <td data-label="外部标签" className="cell-modified">
                  <ModifiedTagDisplay 
                    guessCharacter={guess} 
                    answerCharacter={answerCharacter}
                  />
                </td>
              ) : (
                <>
                  <td data-label="热度" className="cell-popularity">
                    <span className={`feedback-cell ${guess.popularityFeedback === '=' ? 'correct' : (guess.popularityFeedback === '+' || guess.popularityFeedback === '-') ? 'partial' : ''}`}>
                      {guess.popularity}{(guess.popularityFeedback === '+' || guess.popularityFeedback === '++') ? ' ↓' : (guess.popularityFeedback === '-' || guess.popularityFeedback === '--') ? ' ↑' : ''}
                    </span>
                  </td>
                  <td data-label="作品/最高分" className="cell-works">
                    <div className="appearance-container">
                      <div className={`feedback-cell appearance-count ${guess.appearancesCountFeedback === '=' ? 'correct' : (guess.appearancesCountFeedback === '+' || guess.appearancesCountFeedback === '-') ? 'partial' : guess.appearancesCountFeedback === '?' ? 'unknown' : ''}`}>
                        {guess.appearancesCount}{(guess.appearancesCountFeedback === '+' || guess.appearancesCountFeedback === '++') ? ' ↓' : (guess.appearancesCountFeedback === '-' || guess.appearancesCountFeedback === '--') ? ' ↑' : ''}
                      </div>
                      <div className={`feedback-cell appearance-rating ${guess.ratingFeedback === '=' ? 'correct' : (guess.ratingFeedback === '+' || guess.ratingFeedback === '-') ? 'partial' : guess.ratingFeedback === '?' ? 'unknown' : ''}`}>
                        {guess.highestRating === -1 ? '无' : guess.highestRating}{(guess.ratingFeedback === '+' || guess.ratingFeedback === '++') ? ' ↓' : (guess.ratingFeedback === '-' || guess.ratingFeedback === '--') ? ' ↑' : ''}
                      </div>
                    </div>
                  </td>
                  <td data-label="最晚/最早登场" className="cell-appearance">
                    <div className="appearance-container">
                      <div className={`feedback-cell latestAppearance ${guess.latestAppearanceFeedback === '=' ? 'correct' : (guess.latestAppearanceFeedback === '+' || guess.latestAppearanceFeedback === '-') ? 'partial' : guess.latestAppearanceFeedback === '?' ? 'unknown' : ''}`}>
                        {guess.latestAppearance === -1 ? '无' : guess.latestAppearance}{(guess.latestAppearanceFeedback === '+' || guess.latestAppearanceFeedback === '++') ? ' ↓' : (guess.latestAppearanceFeedback === '-' || guess.latestAppearanceFeedback === '--') ? ' ↑' : ''}
                      </div>
                      <div className={`feedback-cell earliestAppearance ${guess.earliestAppearanceFeedback === '=' ? 'correct' : (guess.earliestAppearanceFeedback === '+' || guess.earliestAppearanceFeedback === '-') ? 'partial' : guess.earliestAppearanceFeedback === '?' ? 'unknown' : ''}`}>
                        {guess.earliestAppearance === -1 ? '无' : guess.earliestAppearance}{(guess.earliestAppearanceFeedback === '+' || guess.earliestAppearanceFeedback === '++') ? ' ↓' : (guess.earliestAppearanceFeedback === '-' || guess.earliestAppearanceFeedback === '--') ? ' ↑' : ''}
                      </div>
                    </div>
                  </td>
                </>
              )}
              <td data-label="标签" className="cell-tags">
                <div className="meta-tags-container">
                  {guess.metaTags.map((tag, tagIndex) => {
                    const isExpandTag = tag === '展开';
                    const tagKey = `${guessIndex}-${tagIndex}`;
                    const isClicked = clickedExpandTags.has(tagKey);
                    const isSharedTag = Array.isArray(guess.sharedMetaTags) && guess.sharedMetaTags.includes(tag);
                    const isBanned = bannedTagSet.has(tag);
                    
                    return (
                      <span 
                        key={tagIndex}
                        className={`meta-tag ${isSharedTag && !isBanned ? 'shared' : ''} ${isBanned ? 'banned-tag' : ''} ${isExpandTag ? 'expand-tag' : ''}`}
                        onClick={isExpandTag ? () => handleExpandTagClick(guessIndex, tagIndex) : undefined}
                        style={isExpandTag && !isClicked ? { color: '#0084B4', cursor: 'pointer' } : undefined}
                      >
                        {isBanned ? '???' : tag}
                      </span>
                    );
                  })}
                </div>
              </td>
              <td data-label="共同出演" className="cell-coappearances">
                <span className={`shared-appearances ${guess.sharedAppearances.count > 0 ? 'has-shared' : ''}`}>
                  {guess.sharedAppearances.first}
                  {guess.sharedAppearances.count > 1 && ` +${guess.sharedAppearances.count - 1}`}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {/* Mobile-friendly list: shown on small screens */}
      <div className="guesses-list" aria-hidden="false" ref={listRef}>
        {displayGuesses.map((guess, idx) => (
          <div key={idx} className={`guess-card ${guess.isAnswer ? 'correct' : ''}`}>
            <div className="guess-card-header">
              <Image src={guess.icon} alt="character" className="character-icon" />
              <div className="guess-card-names">
                {guess.guessrName && (
                  <div className="character-guessr-name" style={{ fontSize: '12px', color: '#888' }}>
                    来自：{showNames ? guess.guessrName : '玩家'}
                  </div>
                )}
                <div className="character-name">{guess.name}</div>
                <div className="character-name-cn">{guess.nameCn}</div>
              </div>
              <div className={`guess-card-gender ${guess.genderFeedback === 'yes' ? 'feedback-cell correct' : ''}`}>{getGenderEmoji(guess.gender)}</div>
            </div>

            <div className="guess-card-row">
              <div className="label">热度</div>
              <div className="value">
                <span className={`feedback-cell ${guess.popularityFeedback === '=' ? 'correct' : (guess.popularityFeedback === '+' || guess.popularityFeedback === '-') ? 'partial' : ''}`}>
                  {guess.popularity}{(guess.popularityFeedback === '+' || guess.popularityFeedback === '++') ? ' ↓' : (guess.popularityFeedback === '-' || guess.popularityFeedback === '--') ? ' ↑' : ''}
                </span>
              </div>
            </div>

            <div className="guess-card-row">
              <div className="label">作品 / 最高分</div>
              <div className="value">
                <div style={{display: 'flex', gap: '8px', justifyContent: 'flex-end'}}>
                  <div className={`feedback-cell appearance-count ${guess.appearancesCountFeedback === '=' ? 'correct' : (guess.appearancesCountFeedback === '+' || guess.appearancesCountFeedback === '-') ? 'partial' : guess.appearancesCountFeedback === '?' ? 'unknown' : ''}`}>
                    {guess.appearancesCount}{(guess.appearancesCountFeedback === '+' || guess.appearancesCountFeedback === '++') ? ' ↓' : (guess.appearancesCountFeedback === '-' || guess.appearancesCountFeedback === '--') ? ' ↑' : ''}
                  </div>
                  <div className={`feedback-cell appearance-rating ${guess.ratingFeedback === '=' ? 'correct' : (guess.ratingFeedback === '+' || guess.ratingFeedback === '-') ? 'partial' : guess.ratingFeedback === '?' ? 'unknown' : ''}`}>
                    {guess.highestRating === -1 ? '无' : guess.highestRating}{(guess.ratingFeedback === '+' || guess.ratingFeedback === '++') ? ' ↓' : (guess.ratingFeedback === '-' || guess.ratingFeedback === '--') ? ' ↑' : ''}
                  </div>
                </div>
              </div>
            </div>

            <div className="guess-card-row">
              <div className="label">登场（晚 / 早）</div>
              <div className="value">
                <div style={{display: 'flex', gap: '6px', justifyContent: 'flex-end'}}>
                  <div className={`feedback-cell latestAppearance ${guess.latestAppearanceFeedback === '=' ? 'correct' : (guess.latestAppearanceFeedback === '+' || guess.latestAppearanceFeedback === '-') ? 'partial' : guess.latestAppearanceFeedback === '?' ? 'unknown' : ''}`}>
                    {guess.latestAppearance === -1 ? '无' : guess.latestAppearance}{(guess.latestAppearanceFeedback === '+' || guess.latestAppearanceFeedback === '++') ? ' ↓' : (guess.latestAppearanceFeedback === '-' || guess.latestAppearanceFeedback === '--') ? ' ↑' : ''}
                  </div>
                  <div className={`feedback-cell earliestAppearance ${guess.earliestAppearanceFeedback === '=' ? 'correct' : (guess.earliestAppearanceFeedback === '+' || guess.earliestAppearanceFeedback === '-') ? 'partial' : guess.earliestAppearanceFeedback === '?' ? 'unknown' : ''}`}>
                    {guess.earliestAppearance === -1 ? '无' : guess.earliestAppearance}{(guess.earliestAppearanceFeedback === '+' || guess.earliestAppearanceFeedback === '++') ? ' ↓' : (guess.earliestAppearanceFeedback === '-' || guess.earliestAppearanceFeedback === '--') ? ' ↑' : ''}
                  </div>
                </div>
              </div>
            </div>

            <div className="guess-card-tags">
              <div className="meta-tags-container" aria-label="标签">
                {guess.metaTags.map((tag, tagIndex) => {
                  const isExpandTag = tag === '展开';
                  const tagKey = `${idx}-${tagIndex}`;
                  const isClicked = clickedExpandTags.has(tagKey);
                  const isSharedTag = Array.isArray(guess.sharedMetaTags) && guess.sharedMetaTags.includes(tag);
                  const isBanned = bannedTagSet.has(tag);

                  return (
                    <span
                      key={tagIndex}
                      className={`meta-tag ${isSharedTag && !isBanned ? 'shared' : ''} ${isBanned ? 'banned-tag' : ''} ${isExpandTag ? 'expand-tag' : ''}`}
                      onClick={isExpandTag ? () => handleExpandTagClick(idx, tagIndex) : undefined}
                      style={isExpandTag && !isClicked ? { color: '#0084B4', cursor: 'pointer' } : undefined}
                    >
                      {isBanned ? '???' : tag}
                    </span>
                  );
                })}
              </div>
            </div>

            <div className="guess-card-row" style={{marginTop: 8}}>
              <div className="label">共同出演</div>
              <div className="value" style={{minWidth: 0}}>
                <span className={`shared-appearances ${guess.sharedAppearances.count > 0 ? 'has-shared' : ''}`}>
                  {guess.sharedAppearances.first}
                  {guess.sharedAppearances.count > 1 && ` +${guess.sharedAppearances.count - 1}`}
                </span>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export default GuessesTable; 
