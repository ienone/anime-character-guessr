import { useRef, useState, useEffect, useCallback } from 'react';
import axios from 'axios';
import { searchSubjects, getCharactersBySubjectId, getCharacterDetails } from '../utils/bangumi';
import Image from './Image';
import '../styles/search.css';
import { submitGuessCharacterCount } from '../utils/db';

const SERVER_URL = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '')

function SearchBar({ onCharacterSelect, isGuessing, gameEnd, subjectSearch, finishInit = true }) {
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState([]);
  const [isSearching, setIsSearching] = useState(false);
  const [isLoadingMore, setIsLoadingMore] = useState(false);
  const [offset, setOffset] = useState(0);
  const [hasMore, setHasMore] = useState(true);
  const [searchMode, setSearchMode] = useState('character'); // 'character' or 'subject'
  const [selectedSubject, setSelectedSubject] = useState(null);
  const [subjectCharacters, setSubjectCharacters] = useState([]);
  const [selectedItemIndex, setSelectedItemIndex] = useState(-1); // 当前键盘选中的项目索引
  const [isLoadingNewResults, setIsLoadingNewResults] = useState(false); // 标记是否正在加载更多结果
  const [failedImages, setFailedImages] = useState(() => new Set());
  
  // DOM引用
  const searchContainerRef = useRef(null);
  const searchInputRef = useRef(null);
  const searchDropdownRef = useRef(null);
  const selectedItemRef = useRef(null);
  const characterSearchAbortRef = useRef(null);
  const subjectSearchAbortRef = useRef(null);
  const searchRequestSeqRef = useRef(0);
  
  const INITIAL_LIMIT = 10;
  const MORE_LIMIT = 5;
  const SUBJECT_CHARACTER_LIMIT = 40;

  const formatSubjectCharacters = useCallback(async (subject, characters) => {
    const detailResults = await Promise.allSettled(characters.map(character =>
      getCharacterDetails(character.id)
    ));
    return characters.map((character, index) => {
      const detailResult = detailResults[index];
      const details = detailResult.status === 'fulfilled' ? detailResult.value : {};
      return {
        id: character.id,
        image: character.images?.grid || details.imageGrid || details.image || null,
        name: character.name,
        nameCn: details.nameCn || null,
        gender: details.gender || '?',
        popularity: details.popularity ?? 0,
        defaultSubject: subject
          ? {
            id: subject.id,
            name: subject.name,
            nameCn: subject.name_cn || subject.nameCn || ''
          }
          : null
      };
    });
  }, []);

  const performCharacterSearch = useCallback(async (query, reset = false, requestedOffset = 0) => {
    if (!query || !finishInit) return;

    const currentLimit = reset ? INITIAL_LIMIT : MORE_LIMIT;
    const currentOffset = reset ? 0 : requestedOffset;
    const loadingState = reset ? setIsSearching : setIsLoadingMore;
    const requestSeq = ++searchRequestSeqRef.current;

    if (reset) {
      characterSearchAbortRef.current?.abort();
    }
    const controller = new AbortController();
    characterSearchAbortRef.current = controller;
    
    loadingState(true);
    try {
      const response = await axios.get(`${SERVER_URL}/api/archive/search/characters`, {
        signal: controller.signal,
        params: {
          keyword: query,
          limit: currentLimit,
          offset: currentOffset
        }
      })
      
      const newResults = response.data.data.map(character => ({
        id: character.id,
        image: character.images?.grid || null,
        name: character.name,
        nameCn: character.nameCn || character.name,
        nameEn: character.nameEn || character.romaji || character.name,
        gender: character.gender || '?',
        popularity: character.popularity ?? 0,
        defaultSubject: character.defaultSubject || null
      }));

      if (requestSeq !== searchRequestSeqRef.current) return;

      if (reset) {
        setSearchResults(newResults);
        setOffset(INITIAL_LIMIT);
      } else {
        setSearchResults(prev => [...prev, ...newResults]);
        setOffset(currentOffset + MORE_LIMIT);
      }
      
      setHasMore(newResults.length === currentLimit && currentOffset + currentLimit <= 100);
    } catch (error) {
      if (error.code === 'ERR_CANCELED') return;
      console.error('Search failed:', error);
      if (reset) {
        setSearchResults([]);
      }
    } finally {
      if (requestSeq === searchRequestSeqRef.current) {
        loadingState(false);
      }
    }
  }, [finishInit]);

  const handleSearch = useCallback(async (reset = false) => {
    const query = searchQuery.trim();
    if (!query || !finishInit) return;
    await performCharacterSearch(query, reset, offset);
  }, [searchQuery, finishInit, offset, performCharacterSearch]);

  const performSubjectSearch = useCallback(async (query) => {
    if (!query || !finishInit) return;
    const requestSeq = ++searchRequestSeqRef.current;
    subjectSearchAbortRef.current?.abort();
    const controller = new AbortController();
    subjectSearchAbortRef.current = controller;
    setIsSearching(true);
    try {
      const results = await searchSubjects(query, { signal: controller.signal });
      if (requestSeq !== searchRequestSeqRef.current) return;
      setSearchResults(results);
      setFailedImages(new Set());
      setHasMore(false);
    } catch (error) {
      if (error.code === 'ERR_CANCELED') return;
      console.error('Subject search failed:', error);
      setSearchResults([]);
    } finally {
      if (requestSeq === searchRequestSeqRef.current) {
        setIsSearching(false);
      }
    }
  }, [finishInit]);

  const handleSubjectSearch = useCallback(async () => {
    const query = searchQuery.trim();
    if (!query || !finishInit) return;
    await performSubjectSearch(query);
  }, [searchQuery, finishInit, performSubjectSearch]);

  const handleSubjectSelect = useCallback(async (subject) => {
    setIsSearching(true);
    setSelectedSubject(subject);
    try {
      const characters = await getCharactersBySubjectId(subject.id);
      const visibleCharacters = characters.slice(0, SUBJECT_CHARACTER_LIMIT);
      const formattedCharacters = await formatSubjectCharacters(subject, visibleCharacters);
      setSubjectCharacters(characters);
      setSearchResults(formattedCharacters);
      setFailedImages(new Set());
      setHasMore(characters.length > visibleCharacters.length);
    } catch (error) {
      console.error('Failed to fetch characters:', error);
      setSearchResults([]);
    } finally {
      setIsSearching(false);
    }
  }, [formatSubjectCharacters]);

  const handleLoadMoreSubjectCharacters = useCallback(async () => {
    if (!selectedSubject || isLoadingMore) return;
    const nextCharacters = subjectCharacters.slice(
      searchResults.length,
      searchResults.length + SUBJECT_CHARACTER_LIMIT
    );
    if (nextCharacters.length === 0) {
      setHasMore(false);
      return;
    }

    setIsLoadingMore(true);
    try {
      const formattedCharacters = await formatSubjectCharacters(selectedSubject, nextCharacters);
      setSearchResults(prev => [...prev, ...formattedCharacters]);
      setHasMore(searchResults.length + nextCharacters.length < subjectCharacters.length);
    } catch (error) {
      console.error('Failed to fetch more subject characters:', error);
    } finally {
      setIsLoadingMore(false);
    }
  }, [formatSubjectCharacters, isLoadingMore, searchResults.length, selectedSubject, subjectCharacters]);

  const handleLoadMore = useCallback(() => {
    if (searchMode === 'subject' && selectedSubject) {
      handleLoadMoreSubjectCharacters();
    } else if (searchMode === 'character') {
      handleSearch(false);
    }
  }, [handleLoadMoreSubjectCharacters, searchMode, selectedSubject, handleSearch]);

  const handleCharacterSelect = useCallback((character) => {
    if (!finishInit) return;
    submitGuessCharacterCount(character.id, character.nameCn || character.name);
    onCharacterSelect(character);
    setSearchQuery('');
    setSearchResults([]);
    setFailedImages(new Set());
    setOffset(0);
    setHasMore(true);
    setSelectedSubject(null);
    setSubjectCharacters([]);
    setSearchMode('character');
  }, [finishInit, onCharacterSelect]);

  // Handle click outside to close dropdown
  useEffect(() => {
    function handleClickOutside(event) {
      if (searchContainerRef.current && !searchContainerRef.current.contains(event.target)) {
        setSearchResults([]);
        setFailedImages(new Set());
        setOffset(0);
        setHasMore(true);
        setSelectedSubject(null);
        setSubjectCharacters([]);
      }
    }

    document.addEventListener('mousedown', handleClickOutside);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, []);

  // 快捷键聚焦搜索框（按空格键）
  useEffect(() => {
    function handleKeyDown(e) {
      // 当用户按下空格键且不在输入框中时，聚焦到搜索输入框
      if (e.key === ' ' && document.activeElement.tagName !== 'INPUT' && 
          document.activeElement.tagName !== 'TEXTAREA' && !isGuessing && !gameEnd && finishInit) {
        e.preventDefault();
        searchInputRef.current.focus();
      }
    }
    
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [isGuessing, gameEnd, finishInit]);

  // 自动滚动，确保选中项在视图中可见
  useEffect(() => {
    if (selectedItemIndex >= 0 && selectedItemRef.current) {
      selectedItemRef.current.scrollIntoView({
        behavior: 'smooth', 
        block: 'nearest'
      });
    }
  }, [selectedItemIndex]);

  // 键盘导航处理
  useEffect(() => {
    function handleKeyboardNavigation(e) {
      // 只在搜索结果存在且搜索框聚焦时处理键盘导航
      if (searchResults.length === 0 || document.activeElement !== searchInputRef.current) {
        return;
      }

      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          setSelectedItemIndex(prevIndex => {
            const maxIndex = hasMore && (searchMode === 'character' || selectedSubject) ?
              searchResults.length : searchResults.length - 1;
            // 不再循环到顶部，如果已经到底部就保持在底部
            return prevIndex < maxIndex ? prevIndex + 1 : maxIndex;
          });
          break;
        case 'ArrowUp':
          e.preventDefault();
          setSelectedItemIndex(prevIndex => 
            // 不再循环到底部，如果已经到顶部就保持在顶部
            prevIndex > 0 ? prevIndex - 1 : 0);
          break;
        case 'Enter':
          e.preventDefault();
          if (selectedItemIndex === -1) {
            return;
          }
          
          if (searchMode === 'subject' && !selectedSubject) {
            // 如果在作品搜索模式且选择的是作品
            if (selectedItemIndex < searchResults.length) {
              handleSubjectSelect(searchResults[selectedItemIndex]);
            }
          } else if (selectedItemIndex === searchResults.length && hasMore) {
            // 如果选择的是"加载更多"
            setIsLoadingNewResults(true); // 标记正在加载更多结果
            handleLoadMore();
          } else if (selectedItemIndex < searchResults.length) {
            // 如果选择的是角色
            handleCharacterSelect(searchResults[selectedItemIndex]);
          }
          break;
        default:
          break;
      }
    }

    document.addEventListener('keydown', handleKeyboardNavigation);
    return () => {
      document.removeEventListener('keydown', handleKeyboardNavigation);
    };
  }, [searchResults, selectedItemIndex, searchMode, hasMore, selectedSubject, handleSubjectSelect, handleLoadMore, handleCharacterSelect]);

  // 当搜索结果变化时，处理选中索引的重置或保持
  useEffect(() => {
    // 如果是加载更多的情况，将选中索引设置到新加载内容的第一项
    if (isLoadingNewResults) {
      const previousLength = selectedItemIndex; // 之前选中的是"加载更多"，其索引等于之前结果的长度
      setSelectedItemIndex(previousLength); // 设置到新内容的第一项
      setIsLoadingNewResults(false);
    } else {
      // 正常情况下重置选中索引
      setSelectedItemIndex(-1);
    }
  }, [searchResults, isLoadingNewResults, selectedItemIndex]);

  // Reset pagination when search query changes
  useEffect(() => {
    searchRequestSeqRef.current++;
    characterSearchAbortRef.current?.abort();
    subjectSearchAbortRef.current?.abort();
    setIsSearching(false);
    setIsLoadingMore(false);
    setOffset(0);
    setHasMore(true);
    setSearchResults([]);
    setFailedImages(new Set());
    setSelectedSubject(null);
    setSubjectCharacters([]);
  }, [searchQuery]);

  // Force character search mode when subjectSearch is false
  useEffect(() => {
    if (!subjectSearch && searchMode === 'subject') {
      setSearchMode('character');
      setSearchResults([]);
      setFailedImages(new Set());
      setOffset(0);
      setHasMore(true);
      setSelectedSubject(null);
      setSubjectCharacters([]);
      setIsSearching(false);
      setIsLoadingMore(false);
      searchRequestSeqRef.current++;
      characterSearchAbortRef.current?.abort();
      subjectSearchAbortRef.current?.abort();
    }
  }, [subjectSearch, searchMode]);

  // Debounced search function for character search only
  useEffect(() => {
    if (searchMode !== 'character') return;
    
    const timeoutId = setTimeout(() => {
      const query = searchQuery.trim();
      if (query) {
        setOffset(0);
        setHasMore(true);
        performCharacterSearch(query, true, 0);
      } else {
        setSearchResults([]);
        setFailedImages(new Set());
        setOffset(0);
        setHasMore(true);
      }
    }, 500);

    return () => clearTimeout(timeoutId);
  }, [searchQuery, searchMode, performCharacterSearch]);

  const markImageFailed = useCallback((key) => {
    setFailedImages(prev => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
  }, []);

  const renderResultImage = (src, alt, key) => {
    if (!src || failedImages.has(key)) {
      return (
        <div className="result-character-icon no-image">
          无图片
        </div>
      );
    }

    return (
      <Image
        src={src}
        alt={alt}
        className="result-character-icon"
        fallbackSrc=""
        cachedOnly
        maxRetries={1}
        retryDelay={500}
        onLoadError={() => markImageFailed(key)}
      />
    );
  };

  const renderSearchResults = () => {
    if (searchResults.length === 0) {
      if (!isSearching) return null;
      return (
        <div className="search-dropdown" ref={searchDropdownRef}>
          <div className="search-loading">
            {searchMode === 'subject' && selectedSubject ? '加载角色中...' : '搜索中...'}
          </div>
        </div>
      );
    }

    if (searchMode === 'subject' && !selectedSubject) {
      return (
        <div className="search-dropdown" ref={searchDropdownRef}>
          {isSearching ? (
            <div className="search-loading">搜索中...</div>
          ) : (
            searchResults.map((subject, index) => (
              <div
                key={subject.id}
                className={`search-result-item ${selectedItemIndex === index ? 'selected' : ''}`}
                onClick={() => handleSubjectSelect(subject)}
                ref={selectedItemIndex === index ? selectedItemRef : null}
              >
                {renderResultImage(subject.image, subject.name, `subject:${subject.id}`)}
                <div className="result-character-info">
                  <div className="result-character-name">{subject.name}</div>
                  <div className="result-character-name-cn">{subject.name_cn}</div>
                  <div className="result-subject-type">{subject.type}</div>
                </div>
              </div>
            ))
          )}
        </div>
      );
    }

    return (
      <div className="search-dropdown" ref={searchDropdownRef}>
        {selectedSubject && (
          <div className="selected-subject-header">
            <span>{selectedSubject.name_cn || selectedSubject.name}</span>
            <button 
              className="back-to-subjects"
              onClick={() => {
                setSelectedSubject(null);
                setSubjectCharacters([]);
                handleSubjectSearch();
              }}
            >
              返回
            </button>
          </div>
        )}
        {isSearching ? (
          <div className="search-loading">加载角色中...</div>
        ) : (
          <>
            {searchResults.map((character, index) => (
              <div
                key={character.id}
                className={`search-result-item ${selectedItemIndex === index ? 'selected' : ''}`}
                onClick={() => handleCharacterSelect(character)}
                ref={selectedItemIndex === index ? selectedItemRef : null}
              >
                {renderResultImage(character.image, character.name, `character:${character.id}`)}
                <div className="result-character-info">
                  <div className="result-character-name">{character.name}</div>
                  <div className="result-character-name-cn">{character.nameCn}</div>
                  {character.defaultSubject && (
                    <div className="result-character-subject">
                      {character.defaultSubject.nameCn || character.defaultSubject.name}
                    </div>
                  )}
                </div>
              </div>
            ))}
            {hasMore && (searchMode === 'character' || selectedSubject) && (
              <div 
                className={`search-result-item load-more ${selectedItemIndex === searchResults.length ? 'selected' : ''}`}
                onClick={handleLoadMore}
                ref={selectedItemIndex === searchResults.length ? selectedItemRef : null}
              >
                {isLoadingMore ? '加载中...' : selectedSubject ? '更多角色' : '更多'}
              </div>
            )}
          </>
        )}
      </div>
    );
  };

  return (
    <div className="search-section">
      <div className="search-box">
        <div className="search-input-container" ref={searchContainerRef}>
          <input
            type="text"
            className="search-input"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            disabled={isGuessing || gameEnd || !finishInit}
            placeholder={searchMode === 'character' ? "搜索想猜的角色..." : "搜索想猜的作品..."}
            ref={searchInputRef}
          />
          {renderSearchResults()}
        </div>
        <button 
          className={`search-button ${searchMode === 'character' ? 'active' : ''}`}
          onClick={() => {
            setSearchMode('character');
            if (searchQuery.trim()) handleSearch(true);
          }}
          disabled={!searchQuery.trim() || isSearching || isGuessing || gameEnd || !finishInit}
        >
          {isSearching && searchMode === 'character' ? '在搜了...' : isGuessing ? '在猜了...' : '搜角色'}
        </button>
        {subjectSearch && (
          <button 
            className={`search-button ${searchMode === 'subject' ? 'active' : ''}`}
            onClick={() => {
              const query = searchQuery.trim();
              setSearchMode('subject');
              if (query) performSubjectSearch(query);
            }}
            disabled={!searchQuery.trim() || isSearching || isGuessing || gameEnd || !finishInit}
          >
            {isSearching && searchMode === 'subject' ? '在搜了...' : '搜作品'}
          </button>
        )}
      </div>
    </div>
  );
}

export default SearchBar; 
