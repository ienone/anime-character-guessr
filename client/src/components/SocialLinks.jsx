import '../styles/social.css';
import Icon from './Icon';

function SocialLinks({ onSettingsClick, onHelpClick, onFeedbackClick, showFeedbackInline = false }) {
  return (
    <div className="social-links">
      <div className="difficulty-hint">
        <span>太难了？调下难度</span>
        <div className="arrow"></div>
      </div>
      <button className="social-link settings-button" onClick={onSettingsClick}>
        <Icon name="cog" />
      </button>
      <a href="/" className="social-link" title="Home">
          <Icon name="home" />
      </a>
      <button className="social-link help-button" onClick={onHelpClick}>
        <Icon name="questionCircle" />
      </button>

      {/* Inline feedback button for small screens; shown only when requested */}
      {showFeedbackInline && (
        <button
          className="social-link inline-feedback-button"
          title="Bug/标签反馈"
          onClick={onFeedbackClick}
        >
          <Icon name="exclamationCircle" />
        </button>
      )}

      <a href="https://bangumi.tv/user/725027" target="_blank" rel="noopener noreferrer" className="social-link">
        <img src="https://avatars.githubusercontent.com/u/7521082?s=200&v=4" alt="Bangumi" className="bangumi-icon" />
      </a>
      <a href="https://github.com/kennylimz/anime-character-guessr" target="_blank" rel="noopener noreferrer" className="social-link">
        <Icon name="github" />
      </a>
      <a href="https://space.bilibili.com/87983557" target="_blank" rel="noopener noreferrer" className="social-link">
        <Icon name="bilibili" />
      </a>
    </div>
  );
}

export default SocialLinks; 
