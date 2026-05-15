import perfAxios from './perf.js'
import { loadIdToTags } from './idTagsLoader.js'

const SERVER_URL = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '')
const SUBJECT_SEARCH_CACHE_TTL_MS = 60 * 1000
const SUBJECT_SEARCH_CACHE_MAX = 100
const subjectSearchCache = new Map()

function getCachedSubjectSearch(key) {
  const entry = subjectSearchCache.get(key)
  if (!entry || entry.expiresAt <= Date.now()) {
    subjectSearchCache.delete(key)
    return null
  }
  return entry.value
}

function setCachedSubjectSearch(key, value) {
  if (!subjectSearchCache.has(key) && subjectSearchCache.size >= SUBJECT_SEARCH_CACHE_MAX) {
    const oldestKey = subjectSearchCache.keys().next().value
    subjectSearchCache.delete(oldestKey)
  }
  subjectSearchCache.set(key, {
    expiresAt: Date.now() + SUBJECT_SEARCH_CACHE_TTL_MS,
    value,
  })
}

function describeBackendError(error, fallback) {
  if (error?.response) {
    const status = error.response.status
    const data = error.response.data
    const detail = data?.error || data?.message || (typeof data === 'string' ? data : '')
    return `${fallback}：服务器返回 ${status}${detail ? `，${detail}` : ''}`
  }
  if (error?.request) {
    return `${fallback}：无法连接服务器，请确认后端已启动且前端的服务器地址配置正确`
  }
  return `${fallback}：${error?.message || '未知错误'}`
}

// ─── Backend-backed implementations ──────────────────────────────────────────

/**
 * Fetch a complete character payload from our backend (archive.sqlite).
 * Returns the same shape as getRandomCharacter's result.
 */
async function backendGetRandomCharacter(gameSettings) {
  let response
  try {
    response = await perfAxios.post(`${SERVER_URL}/api/game/random`, {
      startYear: gameSettings.startYear,
      endYear: gameSettings.endYear,
      metaTags: gameSettings.metaTags,
      topNSubjects: gameSettings.topNSubjects,
      commonTags: gameSettings.commonTags,
      subjectTagNum: gameSettings.subjectTagNum,
      characterTagNum: gameSettings.characterTagNum,
      mainCharacterOnly: gameSettings.mainCharacterOnly,
      characterNum: gameSettings.characterNum,
      useSubjectPerYear: gameSettings.useSubjectPerYear,
      addedSubjects: gameSettings.addedSubjects,
    })
  } catch (error) {
    throw new Error(describeBackendError(error, '随机出题失败'))
  }
  const char = response.data
  if (!char || char.error) throw new Error(char?.error || '随机出题失败：后端没有返回有效角色')

  // Normalize rawTags: backend returns an object, frontend expects a Map
  const rawTagsObj = char.rawTags || {}
  char.rawTags = new Map(Object.entries(rawTagsObj).map(([k, v]) => [k, v]))
  return char
}

/**
 * Fetch character appearance data from backend for a given character ID.
 * Used during the guess phase.
 */
async function backendGetCharacterAppearances(characterId, gameSettings) {
  let response
  try {
    response = await perfAxios.post(`${SERVER_URL}/api/game/character`, {
      id: characterId,
      settings: {
        startYear: gameSettings.startYear,
        endYear: gameSettings.endYear,
        metaTags: gameSettings.metaTags,
        commonTags: gameSettings.commonTags,
        subjectTagNum: gameSettings.subjectTagNum,
        characterTagNum: gameSettings.characterTagNum,
      },
    })
  } catch (error) {
    throw new Error(describeBackendError(error, '获取角色登场信息失败'))
  }
  const char = response.data
  if (!char || char.error) throw new Error(char?.error || '获取角色登场信息失败：后端没有返回有效角色')

  const rawTagsObj = char.rawTags || {}
  char.rawTags = new Map(Object.entries(rawTagsObj).map(([k, v]) => [k, v]))
  return {
    appearances: char.appearances || [],
    appearanceIds: char.appearanceIds || [],
    latestAppearance: char.latestAppearance ?? -1,
    earliestAppearance: char.earliestAppearance ?? -1,
    highestRating: char.highestRating ?? -1,
    rawTags: char.rawTags,
    metaTags: char.metaTags || [],
    animeVAs: char.animeVAs || [],
    popularity: char.popularity,
    gender: char.gender,
    image: char.image,
    imageGrid: char.imageGrid,
    nameCn: char.nameCn,
    nameEn: char.nameEn,
    summary: char.summary,
  }
}

// BGM index proxied through our server (index mode still depends on BGM)
async function serverGetIndexInfo(indexId) {
  const response = await perfAxios.get(`${SERVER_URL}/api/bgm/index-info`, { params: { indexId } })
  return response.data
}

async function serverFetchIndexSubjects(indexId, offset, limit) {
  const response = await perfAxios.get(`${SERVER_URL}/api/bgm/index-subjects`, {
    params: { indexId, offset, limit }
  })
  return response.data
}


async function getCharacterAppearances(characterId, gameSettings) {
  return await backendGetCharacterAppearances(characterId, gameSettings)
}

async function getCharacterDetails(characterId) {
  try {
    const response = await perfAxios.get(`${SERVER_URL}/api/archive/characters/${characterId}`)
    const data = response.data
    if (!data) throw new Error('No character details found')

    const gender = typeof data.gender === 'string' &&
      (data.gender === 'male' || data.gender === 'female')
      ? data.gender
      : '?'

    return {
      name: data.name,
      nameCn: data.nameCn ?? null,
      nameEn: data.nameEn ?? null,
      gender,
      image: data.image,
      imageGrid: data.imageGrid,
      summary: data.summary || '',
      popularity: data.popularity ?? 0
    }
  } catch (error) {
    console.error('Error fetching character details:', error);
    throw error;
  }
}

async function getCharactersBySubjectId(subjectId) {
  const response = await perfAxios.get(`${SERVER_URL}/api/archive/subjects/${subjectId}/characters`)

  if (!response.data || !response.data.length) {
    console.error('作品没有角色：'+subjectId);
    throw new Error('选到了作品，但数据库中没有角色，调整范围或重试');
  }

  const filteredCharacters = response.data.filter(character => 
    character.relation === '主角' || character.relation === '配角'
  );

  if (filteredCharacters.length === 0) {
    console.error('作品没有主角/配角？'+subjectId);
    throw new Error('选到了作品，但数据库中没有角色，调整范围或重试');
  }

  return filteredCharacters;
}

function normalizeAddedSubjects(addedSubjects) {
  if (!Array.isArray(addedSubjects)) return []
  return addedSubjects
    .map(subject => {
      if (typeof subject === 'number') return { id: subject }
      if (typeof subject === 'string' && subject.trim()) {
        const id = Number(subject)
        return Number.isFinite(id) ? { id } : null
      }
      if (subject && typeof subject === 'object' && subject.id) return subject
      return null
    })
    .filter(Boolean)
}

async function getRandomCharacter(gameSettings) {
  // No compatibility paths:
  // - Non-index modes: always served by our backend (archive.sqlite)
  // - Index mode: still requires BGM index lists, but only via server proxy routes
  if (!gameSettings.useIndex) {
    return await backendGetRandomCharacter(gameSettings)
  }

  if (!gameSettings.indexId) {
    throw new Error('Index 模式缺少 indexId')
  }

  const batchSize = 10
  const indexInfo = await getIndexInfo(gameSettings.indexId)
  const addedSubjects = normalizeAddedSubjects(gameSettings.addedSubjects)
  const total = indexInfo.total + addedSubjects.length
  let randomOffset = Math.floor(Math.random() * total)
  let subject

  if (randomOffset >= indexInfo.total) {
    randomOffset = randomOffset - indexInfo.total
    subject = addedSubjects[randomOffset]
  } else {
    const batchOffset = Math.floor(randomOffset / batchSize) * batchSize
    const indexInBatch = randomOffset % batchSize
    const response = { data: await serverFetchIndexSubjects(gameSettings.indexId, batchOffset, batchSize) }
    if (!response.data || !response.data.data || response.data.data.length === 0) {
      throw new Error('范围为空')
    }
    subject = response.data.data[Math.min(indexInBatch, response.data.data.length - 1)]
  }

  const characters = await getCharactersBySubjectId(subject.id)
  const filteredCharacters = gameSettings.mainCharacterOnly
    ? characters.filter(character => character.relation === '主角')
    : characters.filter(character => character.relation === '主角' || character.relation === '配角').slice(0, gameSettings.characterNum)

  if (filteredCharacters.length === 0) {
    throw new Error('选到了作品，但数据库中没有角色，请在设置里重试')
  }

  const selectedCharacter = filteredCharacters[Math.floor(Math.random() * filteredCharacters.length)]
  const characterDetails = await getCharacterDetails(selectedCharacter.id)
  const appearances = await getCharacterAppearances(selectedCharacter.id, gameSettings)

  return {
    ...selectedCharacter,
    ...characterDetails,
    ...appearances
  }
}

async function designateCharacter(characterId, gameSettings) {
  try {
    // Get additional character details
    const characterDetails = await getCharacterDetails(characterId);

    // Get character appearances
    const appearances = await getCharacterAppearances(characterId, gameSettings);

    return {
      id: characterId,
      ...characterDetails,
      ...appearances
    };
  } catch (error) {
    console.error('Error getting random character:', error);
    throw error;
  }
}

async function generateFeedback(guess, answerCharacter, gameSettings) {
  const result = {};

  result.gender = {
    guess: guess.gender,
    feedback: guess.gender === answerCharacter.gender ? 'yes' : 'no'
  };

  const popularityDiff = guess.popularity - answerCharacter.popularity;
  const fivePercent = answerCharacter.popularity * 0.05;
  const twentyPercent = answerCharacter.popularity * 0.2;
  let popularityFeedback;
  if (Math.abs(popularityDiff) <= fivePercent) {
    popularityFeedback = '=';
  } else if (popularityDiff > 0) {
    popularityFeedback = popularityDiff <= twentyPercent ? '+' : '++';
  } else {
    popularityFeedback = popularityDiff >= -twentyPercent ? '-' : '--';
  }
  result.popularity = {
    guess: guess.popularity,
    feedback: popularityFeedback
  };

  // Handle rating comparison
  const ratingDiff = guess.highestRating - answerCharacter.highestRating;
  let ratingFeedback;
  if (guess.highestRating === -1 || answerCharacter.highestRating === -1) {
    ratingFeedback = '?';
  } else if (Math.abs(ratingDiff) <= 0.3) {
    ratingFeedback = '=';
  } else if (ratingDiff > 0) {
    ratingFeedback = ratingDiff <= 1 ? '+' : '++';
  } else {
    ratingFeedback = ratingDiff >= -1 ? '-' : '--';
  }
  result.rating = {
    guess: guess.highestRating,
    feedback: ratingFeedback
  };

  const sharedAppearances = guess.appearances.filter(appearance => answerCharacter.appearances.includes(appearance));
  result.shared_appearances = {
    first: sharedAppearances[0] || '',
    count: sharedAppearances.length
  };

  // Compare total number of appearances
  const appearanceDiff = guess.appearances.length - answerCharacter.appearances.length;
  let appearancesFeedback;
  if (appearanceDiff === 0) {
    appearancesFeedback = '=';
  } else if (appearanceDiff > 0) {
    appearancesFeedback = appearanceDiff <= 2 ? '+' : '++';
  } else {
    appearancesFeedback = appearanceDiff >= -2 ? '-' : '--';
  }
  result.appearancesCount = {
    guess: guess.appearances.length,
    feedback: appearancesFeedback
  };

  if (gameSettings.commonTags){
    const guessSubjectTags = Array.from(guess.rawTags.keys());
    const answerSubjectTags = Array.from(answerCharacter.rawTags.keys());
    const answerSubjectTagsSet = new Set(answerSubjectTags);
    const sharedSubjectTags = guessSubjectTags.filter(tag => answerSubjectTagsSet.has(tag)).slice(0, gameSettings.subjectTagNum);
    const subjectTags = [...sharedSubjectTags];
    for (const tag of guessSubjectTags) {
      if (subjectTags.length >= gameSettings.subjectTagNum) break;
      if (!answerSubjectTagsSet.has(tag)) {
        subjectTags.push(tag);
      }
    }

    const idToTags = await loadIdToTags();
    const guessCharacterTags = idToTags?.[guess.id] || [];
    const answerCharacterTags = idToTags?.[answerCharacter.id] || [];
    const answerCharacterTagsSet = new Set(answerCharacterTags);
    const sharedCharacterTags = guessCharacterTags.filter(tag => answerCharacterTagsSet.has(tag)).slice(0, gameSettings.characterTagNum);
    const characterTags = [...sharedCharacterTags];
    for (const tag of guessCharacterTags) {
      if (characterTags.length >= gameSettings.characterTagNum) break;
      if (!answerCharacterTagsSet.has(tag)) {
        characterTags.push(tag);
      }
    }
    const guessCVTags = guess.animeVAs? guess.animeVAs : [];
    const answerCVTags = answerCharacter.animeVAs? answerCharacter.animeVAs : [];
    const sharedCVTags = guessCVTags.filter(tag => answerCVTags.includes(tag));

    const finalGuessTagsSet = new Set([...subjectTags, ...characterTags, ...guessCVTags]);
    const finalSharedTagsSet = new Set([...sharedSubjectTags, ...sharedCharacterTags, ...sharedCVTags]);
    result.metaTags = {
      guess: Array.from(finalGuessTagsSet),
      shared: Array.from(finalSharedTagsSet)
    };
  }
  else{
    // Advice from EST-NINE
    const answerMetaTagsSet = new Set(answerCharacter.metaTags);
    const sharedMetaTags = guess.metaTags.filter(tag => answerMetaTagsSet.has(tag));

    result.metaTags = {
      guess: guess.metaTags,
      shared: sharedMetaTags
    };
  }

  if (guess.latestAppearance === -1 || answerCharacter.latestAppearance === -1) {
    result.latestAppearance = {
      guess: guess.latestAppearance === -1 ? '?' : guess.latestAppearance,
      feedback: guess.latestAppearance === -1 && answerCharacter.latestAppearance === -1 ? '=' : '?'
    };
  } else {
    const yearDiff = guess.latestAppearance - answerCharacter.latestAppearance;
    let yearFeedback;
    if (yearDiff === 0) {
      yearFeedback = '=';
    } else if (yearDiff > 0) {
      yearFeedback = yearDiff <= 2 ? '+' : '++';
    } else {
      yearFeedback = yearDiff >= -2 ? '-' : '--';
    }
    result.latestAppearance = {
      guess: guess.latestAppearance,
      feedback: yearFeedback
    };
  }

  if (guess.earliestAppearance === -1 || answerCharacter.earliestAppearance === -1) {
    result.earliestAppearance = {
      guess: guess.earliestAppearance,
      feedback: guess.earliestAppearance === -1 && answerCharacter.earliestAppearance === -1 ? '=' : '?'
    };
  } else {
    const yearDiff = guess.earliestAppearance - answerCharacter.earliestAppearance;
    let yearFeedback;
    if (yearDiff === 0) {
      yearFeedback = '=';
    } else if (yearDiff > 0) {
      yearFeedback = yearDiff <= 2 ? '+' : '++';
    } else {
      yearFeedback = yearDiff >= -2 ? '-' : '--';
    }
    result.earliestAppearance = {
      guess: guess.earliestAppearance,
      feedback: yearFeedback
    };
  }
  return result;
}

async function getIndexInfo(indexId) {
  try {
    return await serverGetIndexInfo(indexId)
  } catch (error) {
    if (error.response?.status === 404) {
      throw new Error('Index not found');
    }
    console.error('Error fetching index information:', error);
    throw error;
  }
}

async function searchSubjects(keyword, config = {}) {
  try {
    const trimmed = keyword.trim()
    const cacheKey = `2,4|10|${trimmed}`
    const cached = getCachedSubjectSearch(cacheKey)
    if (cached) return cached

    const response = await perfAxios.get(`${SERVER_URL}/api/archive/search/subjects`, {
      ...config,
      params: { keyword: trimmed, type: '2,4', limit: 10, ...(config.params || {}) }
    })

    if (!response.data || !response.data.data) {
      return [];
    }

    const results = response.data.data.map(subject => ({
      id: subject.id,
      name: subject.name,
      name_cn: subject.name_cn,
      image: subject.images?.grid || subject.images?.medium || '',
      date: subject.date,
      type: subject.type==2 ? '动漫' : '游戏'
    }));
    setCachedSubjectSearch(cacheKey, results)
    return results
  } catch (error) {
    if (error.code === 'ERR_CANCELED') {
      throw error
    }
    console.error('Error searching subjects:', error);
    return [];
  }
}

export {
  getRandomCharacter,
  designateCharacter,
  getCharacterAppearances,
  getCharactersBySubjectId,
  getCharacterDetails,
  generateFeedback,
  getIndexInfo,
  searchSubjects
}; 
