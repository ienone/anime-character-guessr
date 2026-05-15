import '../styles/popups.css';
import '../styles/SettingsPopup.css';
import { getIndexInfo, searchSubjects } from '../utils/bangumi';
import { useState, useEffect, useRef, useCallback } from 'react';
import axiosCache from '../utils/cached-axios';
import { getPresetConfig } from '../data/presets';
import { sanitizeHtml } from '../utils/sanitizeHtml';
import { notify } from '../utils/notifications';
import Icon from './Icon';

// Helper Components
const Tooltip = ({ content }) => (
  <div className="tooltip-wrapper">
    <div className="tooltip-icon">?</div>
    <div className="tooltip-content" dangerouslySetInnerHTML={{ __html: sanitizeHtml(content) }} />
  </div>
);

const ToggleSwitch = ({ checked, onChange, disabled }) => (
  <div
    className={`toggle-switch ${checked ? 'active' : ''} ${disabled ? 'disabled' : ''}`}
    onClick={() => !disabled && onChange(!checked)}
  >
    <div className="toggle-thumb" />
  </div>
);

const cloneSettings = (settings) => JSON.parse(JSON.stringify(settings || {}));

function SettingsPopup({ gameSettings: committedSettings, onSettingsChange, onClose, onRestart, hideRestart = false, isMultiplayer = false }) {
  const [indexInputValue, setIndexInputValue] = useState('');
  const [indexInfo, setIndexInfo] = useState(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState([]);
  const searchContainerRef = useRef(null);
  const searchAbortRef = useRef(null);
  const searchRequestSeqRef = useRef(0);
  const [hintInputs, setHintInputs] = useState(['8','5','3']);
  const [localSettings, setLocalSettings] = useState(() => cloneSettings(committedSettings));
  const [isGuessSettingsOpen, setIsGuessSettingsOpen] = useState(false);
  const [isAnswerSettingsOpen, setIsAnswerSettingsOpen] = useState(false);
  const gameSettings = localSettings;
  const updateLocalSetting = useCallback((key, value) => {
    setLocalSettings(prev => ({
      ...prev,
      [key]: value
    }));
  }, []);
  const exclusiveMetaCategories = ['全部', '游戏', '书籍', '三次元', 'Galgame'];
  const isExclusiveMetaCategory = exclusiveMetaCategories.includes((gameSettings.metaTags || [])[0]);

  useEffect(() => {
    const nextSettings = cloneSettings(committedSettings);
    setLocalSettings(nextSettings);
    setIndexInputValue(nextSettings.indexId || '');
  }, [committedSettings]);

  // Handle click outside to close dropdown
  useEffect(() => {
    function handleClickOutside(event) {
      // Add a small delay to allow click events to complete
      setTimeout(() => {
        if (searchContainerRef.current && !searchContainerRef.current.contains(event.target)) {
          setSearchResults([]);
        }
      }, 100);
    }

    document.addEventListener('mousedown', handleClickOutside);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, []);

  // Initialize indexInputValue and fetch indexInfo if indexId exists
  useEffect(() => {
    if (gameSettings.useIndex && gameSettings.indexId) {
      setIndexInputValue(gameSettings.indexId);
      getIndexInfo(gameSettings.indexId)
        .then(info => setIndexInfo(info))
        .catch(console.error);
    }
  }, [gameSettings.indexId, gameSettings.useIndex]);

  useEffect(() => {
    if (Array.isArray(gameSettings.useHints) && gameSettings.useHints.length > 0) {
      // Always keep 3 inputs, fill with '' if less than 3
      const arr = gameSettings.useHints.map(String);
      while (arr.length < 3) arr.push('');
      setHintInputs(arr);
    } else {
      setHintInputs(['','','']);
    }
  }, [gameSettings.useHints]);

  // Enforce commonTags to be true
  useEffect(() => {
    if (!gameSettings.commonTags) {
      updateLocalSetting('commonTags', true);
    }
  }, [gameSettings.commonTags, updateLocalSetting]);

  const setIndex = async (indexId) => {
    if (!indexId) {
      setLocalSettings(prev => ({
        ...prev,
        useIndex: false,
        indexId: null
      }));
      setIndexInputValue('');
      setIndexInfo(null);
      return;
    }

    try {
      const info = await getIndexInfo(indexId);
      setIndexInputValue(indexId);
      setIndexInfo(info);
      setLocalSettings(prev => ({
        ...prev,
        useIndex: true,
        indexId
      }));
    } catch (error) {
      console.error('Failed to fetch index info:', error);
      if (error.message === 'Index not found') {
        notify('目录不存在或者FIFA了', 'warning');
      } else {
        notify('导入失败，请稍后重试', 'error');
      }
      // Reset index settings on error
      setLocalSettings(prev => ({
        ...prev,
        useIndex: false,
        indexId: null
      }));
      setIndexInputValue('');
      setIndexInfo(null);
    }
  };

  const handleImport = async () => {
    if (!indexInputValue) {
      notify('请输入目录ID', 'warning');
      return;
    }
    try {
      const info = await getIndexInfo(indexInputValue);
      setIndexInputValue(indexInputValue);
      setIndexInfo(info);
      setLocalSettings(prev => ({
        ...prev,
        useIndex: true,
        indexId: indexInputValue
      }));
    } catch (error) {
      console.error('Failed to fetch index info:', error);
      if (error.message === 'Index not found') {
        notify('目录不存在或者FIFA了', 'warning');
      } else {
        notify('导入失败，请稍后重试', 'error');
      }
      // Reset index settings on error
      setLocalSettings(prev => ({
        ...prev,
        useIndex: false,
        indexId: null
      }));
      setIndexInputValue('');
      setIndexInfo(null);
    }
  };

  const handleSearch = useCallback(async () => {
    if (!searchQuery.trim()) return;
    const requestSeq = ++searchRequestSeqRef.current;
    searchAbortRef.current?.abort();
    const controller = new AbortController();
    searchAbortRef.current = controller;
    
    try {
      const results = await searchSubjects(searchQuery, { signal: controller.signal });
      if (requestSeq !== searchRequestSeqRef.current) return;
      setSearchResults(results);
    } catch (error) {
      if (error.code === 'ERR_CANCELED') return;
      console.error('Search failed:', error);
      setSearchResults([]);
    }
  }, [searchQuery]);

  // Debounced search function
  useEffect(() => {
    searchRequestSeqRef.current++;
    searchAbortRef.current?.abort();

    const timeoutId = setTimeout(() => {
      if (searchQuery.trim()) {
        handleSearch();
      } else {
        setSearchResults([]);
      }
    }, 500);

    return () => clearTimeout(timeoutId);
  }, [searchQuery, handleSearch]);

  const handleAddSubject = (subject) => {
    const newAddedSubjects = [
      ...(gameSettings.addedSubjects || []),
      {
        id: subject.id,
        name: subject.name,
        name_cn: subject.name_cn,
        type: subject.type,
      }
    ];
    updateLocalSetting('addedSubjects', newAddedSubjects);
    
    // Clear search
    setSearchQuery('');
    setSearchResults([]);
  };

  const handleRemoveSubject = (id) => {
    // Remove the subject from gameSettings
    const newAddedSubjects = (gameSettings.addedSubjects || []).filter(subject => subject.id !== id);
    updateLocalSetting('addedSubjects', newAddedSubjects);
  };

  const handleClearCache = () => {
    axiosCache.clearCache();
    notify('缓存已清空！', 'success');
  }

  const applyPresetConfig = async (presetName) => {
    const presetConfig = getPresetConfig(presetName);
    if (!presetConfig) return;
    
    // 处理所有普通配置项
    setLocalSettings(prev => {
      const next = { ...prev };
      Object.entries(presetConfig).forEach(([key, value]) => {
        if (key !== 'indexId') {
          next[key] = value;
        }
      });
      return next;
    });
    
    // 特殊处理indexId，确保使用setIndex函数
    if (presetConfig.useIndex && presetConfig.indexId) {
      await setIndex(presetConfig.indexId);
    } else {
      await setIndex(""); // 清除索引
    }
  };

  // 关闭时放弃本地更改（恢复到父级传入的 gameSettings）
  const handleClose = () => {
    setLocalSettings(cloneSettings(committedSettings));
    onClose();
  };

  // 确认时一次性同步草稿设置，避免关闭前触发重开局或多人设置广播
  const handleConfirm = () => {
    onSettingsChange(localSettings);
    if (typeof onRestart === 'function') onRestart(localSettings);
    onClose();
  };

  return (
    <div className="popup-overlay">
      <div className="popup-content settings-popup">
        <div className="popup-header group-header settings-popup-header">
          <div className="settings-header-title">
            <h2 className="settings-title">设置</h2>
            <div className="header-subtitle">将鼠标移到各设置的标签上可以看到提示，移到输入框上可以看到数值范围</div>
          </div>
            <div className="header-actions">
            <button className="header-btn clear" onClick={handleClearCache} title="清空缓存">
              <Icon name="trash" />
            </button>
            <button className="header-btn close" onClick={handleClose} title="关闭">
              <Icon name="xmark" />
            </button>
            <button className="header-btn confirm" onClick={handleConfirm} title="确认修改">
              <Icon name="check" />
            </button>
            </div>
        </div>
        
        <div className="settings-popup-content">
          <div className="settings-scroll-area">
            
            {isMultiplayer && (
              <div className="settings-group multiplayer-modes-group">
                <div className="group-header">
                    <h3 className="group-title">多人模式</h3>
                    <div className="group-subtitle">这些模式可以自由组合出2⁴=16种模式</div>
                  </div>
                <div className="multiplayer-modes-grid">
                  {/* 角色全局BP */}
                  <div 
                    className={`mode-card ${localSettings.globalPick ? 'active mode-red' : ''}`}
                    onClick={() => setLocalSettings(s => ({ ...s, globalPick: !s.globalPick }))}
                  >
                    <span className="mode-title">角色全局BP</span>
                    <p className="mode-desc">角色只能被猜一次</p>
                  </div>

                  {/* 标签全局BP */}
                  <div 
                    className={`mode-card ${localSettings.tagBan ? 'active mode-orange' : ''}`}
                    onClick={() => setLocalSettings(s => ({ ...s, tagBan: !s.tagBan }))}
                  >
                    <span className="mode-title">标签全局BP</span>
                    <p className="mode-desc">命中的标签会对别的玩家隐藏</p>
                  </div>

                  {/* 同步模式 */}
                  <div 
                    className={`mode-card ${localSettings.syncMode ? 'active mode-cyan' : ''}`}
                    onClick={() => setLocalSettings(s => ({ ...s, syncMode: !s.syncMode }))}
                  >
                    <span className="mode-title">同步模式</span>
                    <p className="mode-desc">全员猜完才进下一轮</p>
                  </div>

                  {/* 血战模式 */}
                  <div 
                    className={`mode-card ${localSettings.nonstopMode ? 'active mode-pink' : ''}`}
                    onClick={() => setLocalSettings(s => ({ ...s, nonstopMode: !s.nonstopMode }))}
                  >
                    <span className="mode-title">血战模式</span>
                    <p className="mode-desc">直到最后一人猜对或次数耗尽</p>
                  </div>
                </div>
              </div>
            )}

            {/* Group 1: Presets */}
            <div className="settings-group">
              <div className="group-header">
                <h3 className="group-title">预设配置</h3>
                <div className="group-subtitle">我们准备了一些开箱即用的难度配置，您可以直接选用</div>
                <div className="group-header-actions">
                    <button
                    className="action-btn"
                    onClick={() => {
                        const dataStr = "data:text/json;charset=utf-8," + encodeURIComponent(JSON.stringify(gameSettings, null, 2));
                        const dlAnchorElem = document.createElement('a');
                        dlAnchorElem.setAttribute("href", dataStr);
                        dlAnchorElem.setAttribute("download", "gameSettings.json");
                        document.body.appendChild(dlAnchorElem);
                        dlAnchorElem.click();
                        document.body.removeChild(dlAnchorElem);
                    }}
                    >
                    <Icon name="download" className="app-icon-inline" />导出配置
                    </button>
                    <button
                    className="action-btn"
                    onClick={() => {
                        const input = document.createElement('input');
                        input.type = 'file';
                        input.accept = '.json,application/json';
                        input.onchange = (e) => {
                        const file = e.target.files[0];
                        if (!file) return;
                        const reader = new FileReader();
                        reader.onload = (event) => {
                            try {
                            const imported = JSON.parse(event.target.result);
                            setLocalSettings(prev => ({ ...prev, ...imported }));
                            notify('设置已导入！', 'success');
                            } catch (err) {
                            notify('导入失败无效的JSON文件', 'error');
                            }
                        };
                        reader.readAsText(file);
                        };
                        input.click();
                    }}
                    >
                    <Icon name="upload" className="app-icon-inline" />导入配置
                    </button>
                </div>
              </div>
              <div className="presets-grid">
                {['入门', '冻鳗高手', '老番享受者', '瓶子严选', '木柜子痴', '二游高手', '米哈游高手', 'MOBA糕手'].map(preset => (
                   <button 
                    key={preset}
                    className="preset-card"
                    onClick={() => {
                        if (preset === '木柜子痴') notify('😅');
                        if (preset === '二游高手') notify('那很有生活了😅');
                        if (preset === 'MOBA糕手') notify('风暴要火');
                        applyPresetConfig(preset === '米哈游高手' ? '米哈游高手' : preset);
                    }}
                  >
                    {preset === '米哈游高手' ? '米哈游高高手' : preset}
                  </button>
                ))}
              </div>
            </div>

            {/* Group 2: Game Rules */}
            <div className="settings-group">
              <div
                className={`group-header collapsible-group-header ${isGuessSettingsOpen ? 'expanded' : ''}`}
                onClick={() => setIsGuessSettingsOpen(!isGuessSettingsOpen)}
              >
                <div className="settings-section-title-row">
                    <h3 className="group-title">猜测设置</h3>
                    <div className="group-subtitle">影响和玩家猜测有关的内容</div>
                </div>
                <Icon
                    name="chevronDown"
                    className={`settings-chevron ${isGuessSettingsOpen ? 'open' : ''}`}
                />
              </div>

              {isGuessSettingsOpen && (
              <>
              {/* Row 1: Search, Rounds, Time */}
              <div className="settings-row compact-row">
                <div className="setting-item-compact">
                    <label className="settings-label" title="开启后，猜测时可以搜索一个作品中所有人物并从中选择">搜索作品</label>
                    <ToggleSwitch 
                      checked={gameSettings.subjectSearch}
                      onChange={(val) => updateLocalSetting('subjectSearch', val)}
                    />
                </div>

                <div className="setting-item-compact">
                    <label className="settings-label settings-label-offset-attempts" title="一名玩家一局游戏能猜测的次数">猜测次数（次）</label>
                    <div className="compact-input-container" title="数值范围 1-15">
                        <input 
                            className="compact-input"
                            type="number"
                            value={gameSettings.maxAttempts || ''}
                            onChange={(e) => {
                                const val = e.target.value;
                                if (val === '') {
                                    updateLocalSetting('maxAttempts', '');
                                    return;
                                }
                                const num = parseInt(val);
                                if (!isNaN(num)) updateLocalSetting('maxAttempts', Math.min(15, Math.max(1, num)));
                            }}
                            onBlur={() => {
                                if (!gameSettings.maxAttempts) updateLocalSetting('maxAttempts', 10);
                            }}
                        />
                    </div>
                </div>

                <div className="setting-item-compact">
                    <label className="settings-label settings-label-nudge" title="每轮猜测的限制时间，设为0或留空时关闭">时间限制（秒/轮）</label>
                    <div className="compact-input-container settings-label-nudge" title="数值范围 0, 15-120">
                        <input 
                            className={`compact-input ${!gameSettings.timeLimit ? 'is-disabled' : ''}`}
                            type="text"
                            value={gameSettings.timeLimit || ''}
                            placeholder="∞"
                            onChange={(e) => {
                                const val = e.target.value;
                                if (val === '' || val === '0') {
                                    updateLocalSetting('timeLimit', null);
                                    return;
                                }
                                const num = parseInt(val);
                                if (!isNaN(num)) {
                                    updateLocalSetting('timeLimit', num);
                                }
                            }}
                            onBlur={() => {
                                if (gameSettings.timeLimit) {
                                    if (gameSettings.timeLimit < 15) updateLocalSetting('timeLimit', 15);
                                    if (gameSettings.timeLimit > 120) updateLocalSetting('timeLimit', 120);
                                }
                            }}
                            onFocus={(e) => e.target.select()}
                        />
                    </div>
                </div>
              </div>

              {/* Row 2: Hints */}
              <div className="settings-row compact-row">
                <div className="setting-item-compact">
                    <label className="settings-label settings-label-nudge" title="剩余x轮时显示一次文本提示，从左到右填入从大到小的数值，留空或为0时关闭">文本提示（剩x轮）</label>
                    {[0, 1, 2].map((idx) => (
                        <div key={idx} className="compact-input-container" title={`数值范围 1-${gameSettings.maxAttempts || 15}`}>
                            <input
                                className={`compact-input ${(!hintInputs[idx] || hintInputs[idx] === '0') ? 'is-disabled' : ''}`}
                                type="text"
                                value={hintInputs[idx] || ''}
                                placeholder="-"
                                onChange={e => {
                                  const newInputs = [...hintInputs];
                                  let val = e.target.value;
                                  if (val === '' || val === '0') {
                                    newInputs[idx] = '';
                                  } else {
                                    if (/^\d*$/.test(val)) {
                                        let numVal = Number(val);
                                        const max = gameSettings.maxAttempts || 15;
                                        if (numVal > max) numVal = max;
                                        
                                        val = String(Math.floor(numVal));
                                        if (idx > 0 && newInputs[idx-1] && Number(val) >= Number(newInputs[idx-1])) {
                                            for (let i = idx; i < 3; i++) newInputs[i] = '';
                                        } else {
                                            newInputs[idx] = val;
                                        }
                                    }
                                  }
                                  setHintInputs(newInputs);
                                  
                                  const arr = [];
                                  for (let i = 0; i < 3; i++) {
                                    const n = parseInt(newInputs[i], 10);
                                    if (!isNaN(n) && (i === 0 || n < (arr[i-1] || 999))) {
                                      arr.push(n);
                                    } else {
                                      break;
                                    }
                                  }
                                  updateLocalSetting('useHints', arr);
                                }}
                                onFocus={(e) => e.target.select()}
                            />
                        </div>
                    ))}
                </div>

                <div className="setting-item-compact">
                  <label className="settings-label" title={`剩余x轮时显示图片提示，留空或为0时关闭。数值范围 0-${gameSettings.maxAttempts || 15}`}>图片提示（剩x轮）</label>
                  <div className="compact-input-container" title={`数值范围 0-${gameSettings.maxAttempts || 15}`}>
                    <input 
                      className={`compact-input ${!gameSettings.useImageHint ? 'is-disabled' : ''}`}
                      type="number"
                      min="0"
                      max={gameSettings.maxAttempts || 15}
                      value={gameSettings.useImageHint || ''}
                      placeholder="-"
                      onChange={(e) => {
                        const val = e.target.value;
                        if (val === '' || val === '0') {
                          updateLocalSetting('useImageHint', 0);
                          return;
                        }
                        const num = parseInt(val);
                        const max = gameSettings.maxAttempts || 15;
                        if (!isNaN(num)) updateLocalSetting('useImageHint', Math.min(max, Math.max(0, num)));
                      }}
                      onFocus={(e) => e.target.select()}
                    />
                  </div>
                </div>
              </div>
              </>
              )}
            </div>

            {/* Group 3: Question Scope */}
            <div className="settings-group">
              <div
                className={`group-header collapsible-group-header ${isAnswerSettingsOpen ? 'expanded' : ''}`}
                onClick={() => setIsAnswerSettingsOpen(!isAnswerSettingsOpen)}
              >
                <div className="settings-section-title-row">
                    <h3 className="group-title">答案设置</h3>
                    <div className="group-subtitle">影响和答案角色有关的内容</div>
                </div>
                <Icon
                    name="chevronDown"
                    className={`settings-chevron ${isAnswerSettingsOpen ? 'open' : ''}`}
                />
              </div>

              {isAnswerSettingsOpen && (
              <>
              {/* Row 1: Subject Filter & Related Games */}
              <div className="settings-row compact-row">
                <div className="setting-item-compact gap-16">
                    <label className="settings-label nowrap-label" title="这行选项同时会影响登场作品的信息&#10;比如不想让剧场版计入登场数据，可以只勾选'TV'。&#10;当'使用目录'生效时，这行选项不会影响正确答案的抽取，只会影响表格内显示的信息。">作品筛选</label>
                    <div className="settings-inline-wrap">
                        <select 
                          className="settings-select"
                          value={gameSettings.metaTags[0] || ''}
                          onChange={(e) => {
                            const newMetaTags = [...gameSettings.metaTags];
                            const value = e.target.value;
                            newMetaTags[0] = value;
                            if (exclusiveMetaCategories.includes(value)) {
                              newMetaTags[1] = '';
                              newMetaTags[2] = '';
                            }
                            updateLocalSetting('metaTags', newMetaTags);
                          }}
                        >
                          <option value="全部">全部分类</option>
                          <option value="游戏">游戏</option>
                          <option value="书籍">书籍</option>
                          <option value="三次元">三次元</option>
                          <option value="">全部动画</option>
                          <option value="TV">TV</option>
                          <option value="Galgame">Galgame</option>
                          <option value="WEB">WEB</option>
                          <option value="OVA">OVA</option>
                          <option value="剧场版">剧场版</option>
                          <option value="动态漫画">动态漫画</option>
                          <option value="其他">其他</option>
                        </select>

                        <select 
                          className="settings-select"
                          value={gameSettings.metaTags[1] || ''}
                          disabled={isExclusiveMetaCategory}
                          onChange={(e) => {
                            const newMetaTags = [...gameSettings.metaTags];
                            newMetaTags[1] = e.target.value;
                            updateLocalSetting('metaTags', newMetaTags);
                          }}
                        >
                          <option value="">全部来源</option>
                          <option value="原创">原创</option>
                          <option value="漫画改">漫画改</option>
                          <option value="游戏改">游戏改</option>
                          <option value="小说改">小说改</option>
                        </select>

                        <select 
                          className="settings-select"
                          value={gameSettings.metaTags[2] || ''}
                          disabled={isExclusiveMetaCategory}
                          onChange={(e) => {
                            const newMetaTags = [...gameSettings.metaTags];
                            newMetaTags[2] = e.target.value;
                            updateLocalSetting('metaTags', newMetaTags);
                          }}
                        >
                          <option value="">全部类型</option>
                          <option value="科幻">科幻</option>
                          <option value="喜剧">喜剧</option>
                          <option value="百合">百合</option>
                          <option value="校园">校园</option>
                          <option value="惊悚">惊悚</option>
                          <option value="后宫">后宫</option>
                          <option value="机战">机战</option>
                          <option value="悬疑">悬疑</option>
                          <option value="恋爱">恋爱</option>
                          <option value="奇幻">奇幻</option>
                          <option value="推理">推理</option>
                          <option value="运动">运动</option>
                          <option value="耽美">耽美</option>
                          <option value="音乐">音乐</option>
                          <option value="战斗">战斗</option>
                          <option value="冒险">冒险</option>
                          <option value="萌系">萌系</option>
                          <option value="穿越">穿越</option>
                          <option value="玄幻">玄幻</option>
                          <option value="乙女">乙女</option>
                          <option value="恐怖">恐怖</option>
                          <option value="历史">历史</option>
                          <option value="日常">日常</option>
                          <option value="剧情">剧情</option>
                          <option value="武侠">武侠</option>
                          <option value="美食">美食</option>
                          <option value="职场">职场</option>
                        </select>
                    </div>
                </div>

              </div>

              {/* Row 2: Year Range & Popularity Range */}
              <div className="settings-row compact-row">
                <div className="setting-item-compact gap-16">
                    <label className="settings-label" title="开启目录时不可用">年份范围</label>
                    <div className="settings-inline-tight">
                        <div className="compact-input-container width-78" title="数值范围1800-2038">
                            <input 
                              className="compact-input"
                              type="number" 
                              value={gameSettings.startYear || ''}
                              onChange={(e) => {
                                const newStart = e.target.value === '' ? 1800 : parseInt(e.target.value);
                                const currentEnd = gameSettings.endYear || 2038;
                                let newEnd = currentEnd;
                                if (!isNaN(newStart) && newStart > currentEnd) {
                                  newEnd = Math.min(2038, newStart);
                                }
                                updateLocalSetting('startYear', newStart);
                                if (newEnd !== currentEnd) updateLocalSetting('endYear', newEnd);
                              }}
                              min="1800"
                              max="2038"
                              disabled={gameSettings.useIndex}
                            />
                        </div>
                        <span>-</span>
                        <div className="compact-input-container width-78" title="数值范围1900-2038">
                            <input 
                              className="compact-input"
                              type="number" 
                              value={gameSettings.endYear || ''}
                              onChange={(e) => {
                                const newEnd = e.target.value === '' ? 2038 : parseInt(e.target.value);
                                const currentStart = gameSettings.startYear || 1800;
                                let newStart = currentStart;
                                if (!isNaN(newEnd) && newEnd < currentStart) {
                                  newStart = Math.max(1800, newEnd);
                                }
                                updateLocalSetting('endYear', newEnd);
                                if (newStart !== currentStart) updateLocalSetting('startYear', newStart);
                              }}
                              min="1900"
                              max="2038"
                              disabled={gameSettings.useIndex}
                            />
                        </div>
                    </div>
                </div>

                <div className="setting-item-compact offset-md gap-16">
                    <label className="settings-label" title="使用年榜时会先抽取某一年份，再从中抽取作品。&#10;削弱了新番热度的影响。&#10;利好老二次元！&#10;开启目录时不可用">热度范围</label>
                    <div className="settings-inline-wide">
                        <div className="toggle-text-switch">
                            <span 
                                className={!gameSettings.useSubjectPerYear ? 'active' : ''} 
                                onClick={() => !gameSettings.useIndex && updateLocalSetting('useSubjectPerYear', false)}
                                title={gameSettings.useIndex ? '使用目录时不可切换' : ''}
                                aria-disabled={gameSettings.useIndex}
                            >总榜</span>
                            <span 
                                className={gameSettings.useSubjectPerYear ? 'active' : ''} 
                                onClick={() => !gameSettings.useIndex && updateLocalSetting('useSubjectPerYear', true)}
                                title={gameSettings.useIndex ? '使用目录时不可切换' : ''}
                                aria-disabled={gameSettings.useIndex}
                            >年榜</span>
                        </div>
                        <div className="settings-inline">
                            <div className="compact-input-container width-60" title="前N部；0表示全范围">
                                <input 
                                    className="compact-input"
                                    type="number" 
                                    value={gameSettings.topNSubjects === undefined ? '' : gameSettings.topNSubjects}
                                    onChange={(e) => {
                                        const value = e.target.value === '' ? 0 : Math.max(0, parseInt(e.target.value));
                                        updateLocalSetting('topNSubjects', value);
                                    }}
                                    min="0"
                                    max="1000"
                                    disabled={gameSettings.useIndex}
                                />
                            </div>
                            <span className="settings-count-text">{gameSettings.topNSubjects > 0 ? '部' : '全范围'}</span>
                        </div>
                    </div>
                </div>
              </div>

              {/* Row 3: Character Count, Tag Count */}
              <div className="settings-row compact-row">
                <div className="setting-item-compact gap-10 center-items">
                    <label className="settings-label"  title="作品中至少有多少名角色，设为0或留空时仅包含主角">角色数量</label>
                    <div className="compact-input-container min-width-90" title="数值范围 >=0">
                        <input 
                            className="compact-input"
                            type="number"
                            value={gameSettings.characterNum ?? ''}
                            placeholder="角色数量"
                            onChange={(e) => {
                                const val = e.target.value;
                                if (val === '' || val === '0') {
                                    updateLocalSetting('characterNum', 1);
                                } else {
                                    updateLocalSetting('characterNum', Math.max(1, Math.min(99, parseInt(val))));
                                }
                            }}
                        />
                    </div>
                    <span className="settings-help-text">仅主角</span>
                    <ToggleSwitch 
                        checked={gameSettings.mainCharacterOnly}
                        onChange={(val) => {
                            updateLocalSetting('mainCharacterOnly', val);
                        }}
                    />
                </div>
              {/* </div>
              <div className="settings-row compact-row"> */}
                <div className="setting-item-compact offset-sm gap-16">
                    <label className="settings-label" title="猜测时显示的来自作品和角色的标签数量">标签数量</label>
                    <div className="settings-inline-wide">
                        <div className="settings-inline-wide">
                            <span className="settings-help-text">角色</span>
                            <div className="compact-input-container width-50" title="数值范围1-10">
                                <input 
                                    className="compact-input"
                                    type="number"
                                    value={gameSettings.characterTagNum || ''}
                                    onChange={(e) => updateLocalSetting('characterTagNum', Math.max(0, Math.min(10, parseInt(e.target.value) || 0)))}
                                />
                            </div>
                        </div>
                        <div className="settings-inline-wide">
                            <span className="settings-help-text">作品</span>
                            <div className="compact-input-container width-50" title="数值范围1-10">
                                <input 
                                    className="compact-input"
                                    type="number"
                                    value={gameSettings.subjectTagNum || ''}
                                    onChange={(e) => updateLocalSetting('subjectTagNum', Math.max(0, Math.min(10, parseInt(e.target.value) || 0)))}
                                />
                            </div>
                        </div>
                    </div>
                </div>
              </div>

              {/* Row 4: Catalog & Extra Subjects */}
              <div className="settings-row compact-row center-items">
                <div className="setting-item-compact gap-16">
                    <label className="settings-label"  title="勾选时，正确答案只会从目录（+额外作品）中抽取。&#10;目录id为bangumi.tv/index/目录id">使用目录</label>
                    <div className="settings-inline-start">
                        <div className="compact-input-container">
                            <input 
                                className="compact-input"
                                type="text"
                                value={indexInputValue}
                                placeholder="目录ID"
                                onChange={(e) => setIndexInputValue(e.target.value)}
                            />
                        </div>
                        <button className="action-btn compact-action-btn" onClick={handleImport}>导入</button>
                    </div>
                </div>

                <div className="setting-item-compact offset-lg flex-1 gap-16">
                    <label className="settings-label nowrap-label">额外作品</label>
                    <div className="search-container-compact" ref={searchContainerRef} >
                            <input 
                                className="large-input subject-search-input"
                                type="text"
                                placeholder="搜索作品..."
                                value={searchQuery}
                                onChange={(e) => setSearchQuery(e.target.value)}
                                onKeyDown={(e) => {
                                    if (e.key === 'Enter') handleSearch();
                                }}
                            />
                        {searchResults.length > 0 && (
                            <div className="search-results-list">
                                {searchResults.map((subject) => (
                                <div 
                                    key={subject.id} 
                                    className="search-result-item"
                                    onMouseDown={(e) => {
                                        e.preventDefault();
                                        e.stopPropagation();
                                        handleAddSubject(subject);
                                    }}
                                >
                                    <span className="result-title">{subject.name}</span>
                                    <span className="result-meta">{subject.name_cn || ''} ({subject.type})</span>
                                </div>
                                ))}
                            </div>
                        )}
                    </div>
                </div>
              </div>

              {/* Combined Display Area */}
              {(gameSettings.useIndex || gameSettings.addedSubjects.length > 0) && (
                  <div className="combined-display-area">
                      {gameSettings.useIndex && indexInfo && (
                          <div className="catalog-info">
                            <a
                              href={`https://bangumi.tv/index/${gameSettings.indexId}`}
                              target='_blank'
                              rel='noopener noreferrer'
                            >
                              {indexInfo.title}
                            </a>
                            <span className="catalog-count">共 {indexInfo.total} 部作品</span>
                            <button
                              className="tag-remove-btn"
                              title="移除目录"
                              onClick={() => {
                                setLocalSettings(prev => ({ ...prev, useIndex: false, indexId: '' }));
                                setIndexInputValue('');
                                setIndexInfo(null);
                              }}
                            >×</button>
                          </div>
                      )}
                      
                      {gameSettings.addedSubjects.length > 0 && (
                          <div className="extra-subjects-list">
                              {gameSettings.addedSubjects.map((subject) => (
                                  <div key={subject.id} className="subject-tag-large">
                                      <a href={`https://bangumi.tv/subject/${subject.id}`} target="_blank" rel="noopener noreferrer">{subject.name}</a>
                                      <button 
                                          className="tag-remove-btn"
                                          onClick={() => handleRemoveSubject(subject.id)}
                                      >
                                          ×
                                      </button>
                                  </div>
                              ))}
                          </div>
                      )}
                  </div>
              )}
              </>
              )}
            </div>

          </div>
        </div>

        {isMultiplayer && !hideRestart && (
          <div className="popup-footer-new">
              <div className="footer-left">
                <span className="footer-hint">*设置改动点了才会生效！否则下一把生效</span>
              </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default SettingsPopup;
