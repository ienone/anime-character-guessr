import '../styles/popups.css';
import subaruIcon from '/assets/subaru.jpg';
import { useState } from 'react';
import TagContributionPopup from './TagContributionPopup';
import { idToTags } from '../data/id_tags';
import Image from './Image';
import Icon from './Icon';

function renderSummaryWithTags(summary) {
  if (!summary || typeof summary !== 'string') return summary;

  const nodes = [];
  let i = 0;
  let key = 0;

  const pushText = (text) => {
    if (!text) return;
    nodes.push(<span key={`t-${key++}`}>{text}</span>);
  };

  while (i < summary.length) {
    const nextMask = summary.indexOf('[mask]', i);
    const nextUrl = summary.indexOf('[url=', i);
    const candidates = [nextMask, nextUrl].filter((n) => n !== -1);
    const next = candidates.length ? Math.min(...candidates) : -1;

    if (next === -1) {
      pushText(summary.slice(i));
      break;
    }

    if (next > i) {
      pushText(summary.slice(i, next));
    }

    // [mask]...[/mask]
    if (next === nextMask) {
      const start = next + '[mask]'.length;
      const end = summary.indexOf('[/mask]', start);
      if (end === -1) {
        // 不完整标签：按纯文本处理剩余内容
        pushText(summary.slice(next));
        break;
      }
      const inner = summary.slice(start, end);
      nodes.push(
        <span
          key={`m-${key++}`}
          className="summary-mask"
          tabIndex={0}
          aria-label="隐藏内容，悬停或聚焦以显示"
          title="悬停显示"
        >
          {inner}
        </span>
      );
      i = end + '[/mask]'.length;
      continue;
    }

    // [url=https://...]text[/url]
    if (next === nextUrl) {
      const closeBracket = summary.indexOf(']', next);
      if (closeBracket === -1) {
        pushText(summary.slice(next));
        break;
      }
      const urlRaw = summary.slice(next + '[url='.length, closeBracket).trim();
      const end = summary.indexOf('[/url]', closeBracket + 1);
      if (end === -1) {
        pushText(summary.slice(next));
        break;
      }
      const label = summary.slice(closeBracket + 1, end);
      const isSafeHttp = /^https?:\/\//i.test(urlRaw);
      if (isSafeHttp) {
        nodes.push(
          <a
            key={`u-${key++}`}
            className="summary-link"
            href={urlRaw}
            target="_blank"
            rel="noopener noreferrer"
          >
            {label}
          </a>
        );
      } else {
        // 非 http(s) 链接：降级为纯文本，避免注入
        pushText(label);
      }
      i = end + '[/url]'.length;
      continue;
    }

    // 兜底：避免死循环
    pushText(summary.slice(next, next + 1));
    i = next + 1;
  }

  return nodes;
}

function GameEndPopup({ result, answer, onClose }) {
  const [showTagPopup, setShowTagPopup] = useState(false);

  if (showTagPopup) {
    return (
      <TagContributionPopup
        character={answer}
        onClose={() => {
          setShowTagPopup(false);
          onClose();
        }}
      />
    );
  }

  return (
    <div className="popup-overlay">
      <div className="popup-content">
        <button className="popup-close" onClick={onClose}><Icon name="xmark" /></button>
        <div className="popup-header">
          <h2>{result === 'win' ? '🎉 给你猜对了，有点东西' : '😢 已经结束咧'}</h2>
        </div>
        <div className="popup-body">
          <div className="answer-character">
            <Image
              src={answer.image}
              alt={answer.name}
              preferSource
              className="answer-character-image"
            />
            <div className="answer-character-info">
              <div className="character-name-container">
                <a
                  href={`https://bgm.tv/character/${answer.id}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="character-link"
                  style={{ display: 'block', textAlign: 'left' }}
                >
                  <div className="answer-character-name" style={{ textAlign: 'left' }}>{answer.name}</div>
                  <div className="answer-character-name-cn" style={{ textAlign: 'left' }}>{answer.nameCn}</div>
                </a>
                <div className="button-container">
                  <div className="button-group-vertical">
                    <button
                      className="contribute-tag-btn"
                      onClick={() => setShowTagPopup(true)}
                    >
                      贡献标签
                    </button>
                    <button
                      className="contribute-tag-btn"
                      onClick={() => window.open('https://github.com/kennylimz/anime-character-guessr/issues/new', '_blank', 'noopener,noreferrer')}
                    >
                      反馈Bug
                    </button>
                  </div>
                  <img src={subaruIcon} alt="" className="button-icon" />
                </div>
              </div>

              {/* 角色出演作品 */}
              {answer.appearances && answer.appearances.length > 0 && (
                <div className="answer-appearances">
                  <h3>出演作品：</h3>
                  <ul className="appearances-list">
                    {answer.appearances.slice(0, 3).map((appearance, index) => (
                      <li key={index}>{appearance}</li>
                    ))}
                    {answer.appearances.length > 3 && (
                      <li>...等 {answer.appearances.length} 部作品</li>
                    )}
                  </ul>
                </div>
              )}

              {/* 角色标签 */}
              {idToTags[answer.id] && idToTags[answer.id].length > 0 && (
                <div className="answer-tags">
                  <h3>角色标签：</h3>
                  <div className="tags-container">
                    {idToTags[answer.id].map((tag, index) => (
                      <span key={index} className="character-tag">{tag}</span>
                    ))}
                  </div>
                </div>
              )}

              {/* 角色简介 */}
              {answer.summary && (
                <div className="answer-summary">
                  <h3>角色简介：</h3>
                  <div className="summary-content">{renderSummaryWithTags(answer.summary)}</div>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default GameEndPopup;
