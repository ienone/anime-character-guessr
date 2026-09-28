// 由服务端API动态获取排行榜
const API_BASE_URL = import.meta.env.VITE_SERVER_URL || '';

export async function fetchLeaderboardCharacters(limit = 30) {
  try {
    const response = await fetch(`${API_BASE_URL}/api/leaderboard/characters?limit=${limit}`);
    if (!response.ok) throw new Error('获取排行榜失败');
    // 数据格式：[{ _id, count, characterName }]
    const data = await response.json();
    // 转换为前端需要的格式
    return data.map((item, idx) => ({
      rank: idx + 1,
      name: item.characterName || '',
      nameCn: item.characterName || '',
      image: item.image || '',
      link: `https://bgm.tv/character/${item._id}`,
      count: item.count
    }));
  } catch (e) {
    console.error(e);
    return [];
  }
}
