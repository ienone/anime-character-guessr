import axios from "axios";

const DB_SERVER_URL = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '');

if (!DB_SERVER_URL) {
  throw new Error('VITE_DB_SERVER_URL environment variable is not defined');
}

export async function submitAnswerCharacterCount(characterId, characterName) {
  try {
    const response = await axios.post(`${DB_SERVER_URL}/api/answer-character-count`, {
      characterId,
      characterName,
    });
    return response.data;
  } catch (error) {
    console.error('Error submitting character answer count:', error);
  }
}

export async function submitGuessCharacterCount(characterId, characterName) {
  try {
    const response = await axios.post(`${DB_SERVER_URL}/api/guess-character-count`, {
      characterId,
      characterName,
    });
    return response.data;
  } catch (error) {
    console.error('Error submitting character guess count:', error);
  }
}


