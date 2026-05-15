import '../styles/popups.css';
import announcements from '../data/announcements';
import UpdateAnnouncement from './UpdateAnnouncement';
import Icon from './Icon';

function WelcomePopup({ onClose }) {
  return (
    <div className="popup-overlay">
      <div className="popup-content welcome-popup">
        <button className="popup-close" onClick={onClose}><Icon name="xmark" /></button>
        <div className="popup-header welcome-header">
          <div className="welcome-header-inner">
            <div className="title-container">
              <div className="title-line title-line-main" data-text="二刺猿笑傳">二刺猿笑傳</div>
              <div className="title-line title-line-separator" data-text="A N I M E &nbsp; C H A R A C T E R &nbsp; G U E S S R &nbsp;">A N I M E &nbsp; C H A R A C T E R &nbsp; G U E S S R &nbsp;</div>
              <div className="title-line title-line-sub" data-text="猜猜唄">猜猜唄</div>
            </div>

            <div className="title-divider" aria-hidden="true" />

            <div className="welcome-qq">
              <a href="https://qm.qq.com/q/2sWbSsCwBu" target="_blank" rel="noopener noreferrer" title="加入QQ群">
                <img src="/assets/qqgroup.png" alt="QQ群" className="welcome-qq-img" />
              </a>
            </div>
          </div>
        </div>
        <div className="popup-body">
          <div className="welcome-content">
            <div className="welcome-text">
              <p>猜猜呗终于<del>和Faze一起</del>地狱归来了！这段时间我们对多人模式进行了大量更新：</p>
              <ul>
                <li><b>血战模式</b>：第一个猜对的玩家不会立即结束游戏，所有玩家继续猜测，直到最后一人猜对或次数耗尽</li>
                <li><b>同步模式</b>：所有玩家需要等待其他玩家完成当前轮猜测后才能进行下一轮</li>
                <li><b>角色全局BP</b>：开启后一名角色只能被猜一次（除答案）</li>
                <li><b>标签全局BP</b>：开启后一个标签只能被首个猜到的玩家获取</li>
                <li>此外，还有<b>大量UIUX优化和Bug修复</b></li>
              </ul>
              感谢 <a href="https://github.com/trim21" target="_blank" rel="noopener noreferrer">Bangumi 管理员</a> 的优化支持，
                以及各位<a href="https://github.com/kennylimz/anime-character-guessr/graphs/contributors" target="_blank" rel="noopener noreferrer">网友</a>贡献的代码和数据。
                感谢大家这段时间的热情和支持
              <br/>
              <p><b>如果您有任何建议或问题，欢迎加入我们的<a href="https://qm.qq.com/q/2sWbSsCwBu" target="_blank" >QQ群</a>或<a href="https://github.com/kennylimz/anime-character-guessr/issues/new" target="_blank" >提交Issue</a>！</b></p>
              
              <hr style={{margin: '20px 0', border: '0', borderTop: '1px solid rgba(0,0,0,0.1)'}} />
              
              <UpdateAnnouncement 
                announcements={announcements} 
                defaultExpanded={false}
                initialVisibleCount={1}
              />

            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

export default WelcomePopup;
