let idToTagsPromise = null;

export function loadIdToTags() {
  if (!idToTagsPromise) {
    const url = `${import.meta.env.BASE_URL || '/'}data/id_tags.json`;
    idToTagsPromise = fetch(url).then((response) => {
      if (!response.ok) {
        throw new Error(`Failed to load id tags: ${response.status}`);
      }
      return response.json();
    });
  }
  return idToTagsPromise;
}
