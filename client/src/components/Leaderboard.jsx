import { useState, useEffect } from 'react';
import '../styles/Leaderboard.css';
import { fetchLeaderboardCharacters } from '../data/leaderboard_characters';
import { fetchLeaderboardGuesses, fetchLeaderboardWeekly } from '../data/leaderboard_guesses';
import Image from './Image';

const Leaderboard = ({ defaultExpanded = false }) => {
  const [isExpanded1, setIsExpanded1] = useState(defaultExpanded);
  const [isExpanded2, setIsExpanded2] = useState(defaultExpanded);
  const [characters1, setCharacters1] = useState([]);
  const [characters2, setCharacters2] = useState([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let mounted = true;
    Promise.all([
      fetchLeaderboardGuesses(30),
      fetchLeaderboardCharacters(30),
      fetchLeaderboardWeekly(30)
    ]).then(([guesses, characters, weekly]) => {
      if (mounted) {
        // 将周榜数据合并到总榜中，通过link匹配
        const weeklyMap = new Map(weekly.map(w => [w.link, w.count]));
        const guessesWithWeekly = guesses.map(char => ({
          ...char,
          weeklyCount: weeklyMap.get(char.link) || 0
        }));
        setCharacters1(guessesWithWeekly);
        setCharacters2(characters);
        setLoading(false);
      }
    }).catch(() => {
      if (mounted) setLoading(false);
    });
    return () => { mounted = false; };
  }, []);

  // Podium: 2nd, 1st, 3rd (left, center, right)
  const podiumOrder1 = characters1.length >= 3 ? [characters1[1], characters1[0], characters1[2]] : [];
  const podiumOrder2 = characters2.length >= 3 ? [characters2[1], characters2[0], characters2[2]] : [];

  const toggleExpand1 = () => setIsExpanded1((prev) => !prev);
  const toggleExpand2 = () => setIsExpanded2((prev) => !prev);

  return (
    <>
      <div className="leaderboard-container">
        <div className="leaderboard-header" onClick={toggleExpand1}>
          <h3>大家都在猜（每周一4：00清空周榜）</h3>
          <span className={`expand-icon ${isExpanded1 ? 'expanded' : ''}`}>{isExpanded1 ? '▼' : '▶'}</span>
        </div>
        {isExpanded1 && (
          <div className="leaderboard-content">
            {loading ? (
              <div className="leaderboard-loading">加载中...</div>
            ) : podiumOrder1.length === 0 ? (
              <div className="leaderboard-empty">暂无数据</div>
            ) : (
              <>
                <div className="leaderboard-podium">
                  {podiumOrder1.map((char, idx) => (
                    <div
                      className={`podium-place podium-place-${char.rank} ${char.rank === 1 ? 'podium-center' : ''}`}
                      key={char.link || idx}
                    >
                      <Image
                        src={char.image}
                        alt={char.name}
                        preferSource
                        className={`podium-image${char.rank === 1 ? ' podium-image-center' : ''}`}
                      />
                      <div className="podium-rank">#{char.rank}</div>
                      <a
                        href={char.link}
                        className="podium-name podium-link"
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {char.nameCn || char.name}
                      </a>
                      <div className="podium-count">
                        <span className="count-total">总计 {char.count}次</span>
                        {char.weeklyCount > 0 && <span className="count-weekly">本周 {char.weeklyCount}次</span>}
                      </div>
                    </div>
                  ))}
                </div>
                <div className="leaderboard-list">
                  {characters1.slice(3).map((char, idx) => (
                    <div className="leaderboard-list-item" key={char.link || idx}>
                      <div className="list-rank">#{char.rank}</div>
                      <Image src={char.image} alt={char.name} className="list-image" />
                      <a
                        href={char.link}
                        className="list-name podium-link"
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {char.nameCn || char.name}
                      </a>
                      <div className="list-count">
                        <span className="count-total">{char.count}次</span>
                        {char.weeklyCount > 0 && <span className="count-weekly">+{char.weeklyCount}</span>}
                      </div>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        )}
      </div>
      <div className="leaderboard-container">
        <div className="leaderboard-header" onClick={toggleExpand2}>
          <h3>热门出题角色</h3>
          <span className={`expand-icon ${isExpanded2 ? 'expanded' : ''}`}>{isExpanded2 ? '▼' : '▶'}</span>
        </div>
        {isExpanded2 && (
          <div className="leaderboard-content">
            {loading ? (
              <div className="leaderboard-loading">加载中...</div>
            ) : podiumOrder2.length === 0 ? (
              <div className="leaderboard-empty">暂无数据</div>
            ) : (
              <>
                <div className="leaderboard-podium">
                  {podiumOrder2.map((char, idx) => (
                    <div
                      className={`podium-place podium-place-${char.rank} ${char.rank === 1 ? 'podium-center' : ''}`}
                      key={char.link || idx}
                    >
                      <Image
                        src={char.image}
                        alt={char.name}
                        preferSource
                        className={`podium-image${char.rank === 1 ? ' podium-image-center' : ''}`}
                      />
                      <div className="podium-rank">#{char.rank}</div>
                      <a
                        href={char.link}
                        className="podium-name podium-link"
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {char.nameCn || char.name}
                      </a>
                      <div className="podium-count">{char.count}次</div>
                    </div>
                  ))}
                </div>
                <div className="leaderboard-list">
                  {characters2.slice(3).map((char, idx) => (
                    <div className="leaderboard-list-item" key={char.link || idx}>
                      <div className="list-rank">#{char.rank}</div>
                      <Image src={char.image} alt={char.name} className="list-image" />
                      <a
                        href={char.link}
                        className="list-name podium-link"
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        {char.nameCn || char.name}
                      </a>
                      <div className="list-count">{char.count}次</div>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </>
  );
};

export default Leaderboard; 
