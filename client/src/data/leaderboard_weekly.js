// 由服务端 API 动态获取周榜
const API_BASE_URL = import.meta.env.VITE_SERVER_URL || '';

export async function fetchLeaderboardWeekly(limit = 30) {
  try {
    const response = await fetch(`${API_BASE_URL}/api/leaderboard/weekly?limit=${limit}`);
    if (!response.ok) throw new Error('获取周榜失败');
    const data = await response.json();
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
