import { Link } from 'react-router-dom';
import { useState, useEffect } from 'react';
import '../styles/Home.css';
import WelcomePopup from '../components/WelcomePopup';
import Icon from '../components/Icon';

const LINE_OPTIONS = [
  { url: 'https://anime-character-guessr.netlify.app/', name: 'Netlify' },
  { url: 'https://ccb.baka.website/', name: 'Baka专线' }
];

const Home = () => {
  const [roomCount, setRoomCount] = useState(0);
  const [showWelcomePopup, setShowWelcomePopup] = useState(false);
  // 线路选择当前域名状态
  const [currentOrigin, setCurrentOrigin] = useState('');

  useEffect(() => {
    const serverUrl = import.meta.env.VITE_SERVER_URL || '';
    let mounted = true;

    const fetchRoomCount = () => {
      fetch(`${serverUrl}/room-count`)
        .then(response => {
          if (!response.ok) throw new Error('Failed to fetch');
          return response.json();
        })
        .then(data => { if (mounted) setRoomCount(data.count); })
        .catch(error => console.error('Error fetching room count:', error));
    };

    // initial fetch
    fetchRoomCount();
    // refresh every 5 seconds
    const intervalId = setInterval(fetchRoomCount, 5000);

    setShowWelcomePopup(true);

    return () => { mounted = false; clearInterval(intervalId); };
  }, []);

  useEffect(() => {
    if (typeof window !== 'undefined') {
      setCurrentOrigin(window.location.origin);
    }
  }, []);

  const handleCloseWelcomePopup = () => {
    setShowWelcomePopup(false);
  };

  // 只在当前域名不在LINE_OPTIONS时才添加，否则只显示两条
  const cleanedOrigin = (currentOrigin || '').replace(/\/$/, '');
  const availableLines = LINE_OPTIONS.some(line => line.url.replace(/\/$/, '') === cleanedOrigin)
    ? LINE_OPTIONS
    : [...LINE_OPTIONS, { url: currentOrigin }];

  return (
    <div className="home-container">

      {showWelcomePopup && (
        <WelcomePopup onClose={handleCloseWelcomePopup} />
      )}

      <div className="center-block">
      <div className="game-modes">
        <Link to="/singleplayer" className="mode-button">
          <h2>单人</h2>
        </Link>
        <Link to="/multiplayer" className="mode-button">
          <h2>多人</h2>
          <small>当前房间数: {roomCount}</small>
        </Link>
      </div>

      <div className="line-selector">
        <div className="line-selector-header">
          <span className="line-selector-title">线路选择</span>
          <span className="line-selector-hint">如搜索、猜测缓慢可尝试切换</span>
        </div>
        <div className="line-selector-list">
          {availableLines.map((line, idx) => {
            if (!line.url) return null;
            const cleanedOrigin = (currentOrigin || '').replace(/\/$/, '');
            const cleanedLine = line.url.replace(/\/$/, '');
            const isCurrent = cleanedOrigin && cleanedOrigin === cleanedLine;
            // 判断是否为本地/局域网
            let displayName = line.name || line.url;
            if (idx === 2 || (!line.name && availableLines.length > 2 && idx === availableLines.length - 1)) {
              // 仅对第三线路或动态添加的线路做本地判断
              try {
                const urlObj = new URL(line.url, window.location.origin);
                const host = urlObj.hostname;
                if (
                  host === 'localhost' ||
                  host === '127.0.0.1' ||
                  /^192\.168\./.test(host) ||
                  /^10\./.test(host) ||
                  /^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(host)
                ) {
                  displayName = '本地部署';
                }
              } catch {
                // ignore malformed URL
              }
            }

            // 仅对第三线路或动态添加的线路显示为“抢先体验”（如果匹配 ccbeta.baka.website）
            if (idx === 2 || (!line.name && availableLines.length > 2 && idx === availableLines.length - 1)) {
              try {
                const lineHost = new URL(line.url, window.location.origin).hostname;
                const originHost = currentOrigin ? new URL(currentOrigin).hostname : '';
                if (lineHost === 'ccbeta.baka.website' || originHost === 'ccbeta.baka.website') {
                  displayName = '抢先体验';
                }
              } catch {
                // ignore malformed URL
              }
            }

            return (
              <a
                key={`${line.url}-${idx}`}
                className={`domain-link${isCurrent ? ' active' : ''}`}
                data-url={line.url}
                href={isCurrent ? '#' : line.url}
                onClick={e => { if (isCurrent) e.preventDefault(); }}
                style={{ pointerEvents: isCurrent ? 'none' : 'auto' }}
              >
                <div className="domain-info">
                  <span className="line-name">{displayName}</span>
                </div>
              </a>
            );
          })}
        </div>
      </div>
      </div>

      <div className="home-footer">
        <div className="button-group-grid">
          <a
            href="#"
            className="fotter-btn"
            onClick={e => { e.preventDefault(); setShowWelcomePopup(true); }}
          >
            <Icon name="bullhorn" className="app-icon-inline" />显示公告
          </a>
          <a
            href="https://status.baka.website/status/ccb"
            target="_blank"
            rel="noopener noreferrer"
            className="fotter-btn"
          >
            <Icon name="server" className="app-icon-inline" />服务状态
          </a>
          <a 
            href="https://www.bilibili.com/video/BV14CVRzUELs" 
            target="_blank" 
            rel="noopener noreferrer"
            className="fotter-btn"
          >
            <Icon name="bilibili" className="app-icon-inline" />玩法简介
          </a>
          <a 
            href="https://github.com/kennylimz/anime-character-guessr" 
            target="_blank" 
            rel="noopener noreferrer"
            className="fotter-btn"
          >
            <Icon name="github" className="app-icon-inline" />GitHub仓库
          </a>
          <a 
            href="https://qm.qq.com/q/2sWbSsCwBu" 
            target="_blank" 
            rel="noopener noreferrer"
            className="fotter-btn"
          >
            <Icon name="qq" className="app-icon-inline" />加入QQ群
          </a>
          <a 
            href="https://www.bilibili.com/video/BV1MstxzgEhg/" 
            target="_blank" 
            rel="noopener noreferrer"
            className="fotter-btn"
          >
            <Icon name="desktop" className="app-icon-inline" />作者的新玩具
          </a>
        </div>
        <p>
          {/* <a href="https://vertikarl.github.io/anime-character-guessr-english/"> ENGLISH ver. </a> */}
          一个猜动漫/游戏角色的网站，建议使用桌面端浏览器游玩
          <br/>
          灵感来源<a href="https://blast.tv/counter-strikle"> BLAST.tv </a> &nbsp;
          数据来源<a href="https://bgm.tv/"> Bangumi </a>
        </p>
      </div>
    </div>
  );
};

export default Home;
