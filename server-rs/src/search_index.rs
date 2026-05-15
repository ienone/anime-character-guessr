use anyhow::Context;
use serde_json::{Value, json};
use std::path::Path;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Query, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, TantivyDocument};
use tantivy::{Document, Index, IndexReader, ReloadPolicy, Term};

const SUBJECT_SEARCH_INITIAL_CANDIDATES: usize = 256;
const SUBJECT_SEARCH_MAX_CANDIDATES: usize = 12_000;

pub struct TantivySearch {
    characters: TantivyIndex,
    subjects: TantivyIndex,
}

struct TantivyIndex {
    index: Index,
    reader: IndexReader,
    schema: Schema,
    fields: TantivyFields,
}

struct TantivyFields {
    query_fields: Vec<Field>,
    type_filter_field: Option<Field>,
}

impl TantivySearch {
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref();
        Ok(Self {
            characters: TantivyIndex::open(
                &root.join("characters"),
                &[
                    "name",
                    "name_cn",
                    "name_en",
                    "romaji",
                    "aliases",
                    "search_terms",
                ],
            )
            .context("open character Tantivy index")?,
            subjects: TantivyIndex::open(
                &root.join("subjects"),
                &["name", "name_cn", "search_terms"],
            )
            .context("open subject Tantivy index")?,
        })
    }

    pub fn search_characters(
        &self,
        keyword: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<Value>> {
        let docs = self.characters.search(keyword, 500, 0)?;
        let mut docs = docs
            .into_iter()
            .map(|(score, doc)| {
                let bucket = character_relevance_bucket(&doc, keyword);
                let popularity = first_u64(&doc, "popularity").unwrap_or(0);
                (bucket, score, popularity, doc)
            })
            .collect::<Vec<_>>();
        docs.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
        });
        Ok(docs
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(_, _, _, doc)| {
                let id = first_u64(&doc, "id").unwrap_or(0);
                let name = first_str(&doc, "name");
                let name_cn = first_str(&doc, "name_cn");
                let name_en = first_str(&doc, "name_en");
                let romaji = first_str(&doc, "romaji");
                let default_subject_id = first_u64(&doc, "default_subject_id").unwrap_or(0);
                json!({
                    "id": id,
                    "name": name,
                    "nameCn": if name_cn.is_empty() { Value::Null } else { json!(name_cn) },
                    "nameEn": if name_en.is_empty() { Value::Null } else { json!(name_en) },
                    "romaji": if romaji.is_empty() { Value::Null } else { json!(romaji) },
                    "gender": first_str(&doc, "gender"),
                    "images": { "grid": format!("/img/{}.webp", id) },
                    "popularity": first_u64(&doc, "popularity").unwrap_or(0),
                    "defaultSubject": if default_subject_id == 0 {
                        Value::Null
                    } else {
                        json!({
                            "id": default_subject_id,
                            "name": first_str(&doc, "default_subject_name"),
                            "nameCn": first_str(&doc, "default_subject_name_cn"),
                        })
                    },
                })
            })
            .collect())
    }

    pub fn search_subjects(
        &self,
        keyword: &str,
        types: &[i64],
        limit: usize,
    ) -> anyhow::Result<Vec<Value>> {
        if limit == 0 || types.is_empty() {
            return Ok(Vec::new());
        }

        let docs = if self.subjects.fields.type_filter_field.is_some() {
            let candidate_limit = subject_candidate_window(limit);
            self.subjects
                .search_with_u64_filter(keyword, types, candidate_limit, 0)?
        } else {
            self.subjects
                .search_progressively_filtered(keyword, types, limit)?
        };

        let mut ranked = rank_subject_docs(docs, keyword, types);
        ranked.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
        });

        let mut out = Vec::new();
        for (_, _, _, doc) in ranked.into_iter().take(limit) {
            let id = first_u64(&doc, "id").unwrap_or(0);
            let subject_type = first_u64(&doc, "type").unwrap_or(0) as i64;
            let img_url = format!("/img/subject/{}.webp", id);
            out.push(json!({
                "id": id,
                "type": subject_type,
                "date": first_str(&doc, "date"),
                "name": first_str(&doc, "name"),
                "name_cn": first_str(&doc, "name_cn"),
                "images": { "grid": img_url, "medium": img_url, "common": img_url },
            }));
        }
        Ok(out)
    }
}

impl TantivyIndex {
    fn open(path: &Path, query_field_names: &[&str]) -> anyhow::Result<Self> {
        let index = Index::open_in_dir(path)?;
        let schema = index.schema();
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let query_fields = query_field_names
            .iter()
            .map(|name| schema.get_field(name))
            .collect::<Result<Vec<_>, _>>()?;
        let type_filter_field = schema
            .get_field("type")
            .ok()
            .filter(|field| schema.get_field_entry(*field).is_indexed());
        Ok(Self {
            index,
            reader,
            schema,
            fields: TantivyFields {
                query_fields,
                type_filter_field,
            },
        })
    }

    fn search(
        &self,
        keyword: &str,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<(f32, Value)>> {
        let query_text = sanitize_query(keyword);
        if query_text.is_empty() {
            return Ok(Vec::new());
        }
        let mut parser = QueryParser::for_index(&self.index, self.fields.query_fields.clone());
        parser.set_conjunction_by_default();
        let query = parser.parse_query(&query_text)?;
        let searcher = self.reader.searcher();
        let top_docs = searcher.search(
            &query,
            &TopDocs::with_limit(limit)
                .and_offset(offset)
                .order_by_score(),
        )?;
        let mut out = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            out.push((score, serde_json::from_str(&doc.to_json(&self.schema))?));
        }
        Ok(out)
    }

    fn search_with_u64_filter(
        &self,
        keyword: &str,
        filter_values: &[i64],
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<(f32, Value)>> {
        let query_text = sanitize_query(keyword);
        if query_text.is_empty() {
            return Ok(Vec::new());
        }

        let mut parser = QueryParser::for_index(&self.index, self.fields.query_fields.clone());
        parser.set_conjunction_by_default();
        let text_query = parser.parse_query(&query_text)?;
        let Some(type_field) = self.fields.type_filter_field else {
            return self.search(keyword, limit, offset);
        };

        let type_queries = filter_values
            .iter()
            .copied()
            .filter(|value| *value >= 0)
            .map(|value| {
                let term = Term::from_field_u64(type_field, value as u64);
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>
            })
            .collect::<Vec<_>>();
        if type_queries.is_empty() {
            return Ok(Vec::new());
        }

        let type_query: Box<dyn Query> = if type_queries.len() == 1 {
            type_queries.into_iter().next().unwrap()
        } else {
            Box::new(BooleanQuery::union(type_queries))
        };
        let query = BooleanQuery::intersection(vec![text_query, type_query]);
        self.collect_docs(&query, limit, offset)
    }

    fn search_progressively_filtered(
        &self,
        keyword: &str,
        types: &[i64],
        limit: usize,
    ) -> anyhow::Result<Vec<(f32, Value)>> {
        let mut candidate_limit = subject_candidate_window(limit);
        loop {
            let docs = self.search(keyword, candidate_limit, 0)?;
            let matched = docs
                .iter()
                .filter(|(_, doc)| {
                    let subject_type = first_u64(doc, "type").unwrap_or(0) as i64;
                    types.contains(&subject_type)
                })
                .count();
            let exhausted =
                docs.len() < candidate_limit || candidate_limit >= SUBJECT_SEARCH_MAX_CANDIDATES;
            if matched >= limit || exhausted {
                return Ok(docs);
            }
            candidate_limit = (candidate_limit * 3).min(SUBJECT_SEARCH_MAX_CANDIDATES);
        }
    }

    fn collect_docs(
        &self,
        query: &dyn Query,
        limit: usize,
        offset: usize,
    ) -> anyhow::Result<Vec<(f32, Value)>> {
        let searcher = self.reader.searcher();
        let top_docs = searcher.search(
            query,
            &TopDocs::with_limit(limit)
                .and_offset(offset)
                .order_by_score(),
        )?;
        let mut out = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            out.push((score, serde_json::from_str(&doc.to_json(&self.schema))?));
        }
        Ok(out)
    }
}

fn subject_candidate_window(limit: usize) -> usize {
    (limit.saturating_mul(20))
        .clamp(SUBJECT_SEARCH_INITIAL_CANDIDATES, 600)
        .min(SUBJECT_SEARCH_MAX_CANDIDATES)
}

fn rank_subject_docs(
    docs: Vec<(f32, Value)>,
    keyword: &str,
    types: &[i64],
) -> Vec<(u8, f32, u64, Value)> {
    docs.into_iter()
        .filter_map(|(score, doc)| {
            let subject_type = first_u64(&doc, "type").unwrap_or(0) as i64;
            if !types.contains(&subject_type) {
                return None;
            }
            let bucket = subject_relevance_bucket(&doc, keyword);
            let popularity = first_u64(&doc, "popularity").unwrap_or(0);
            Some((bucket, score, popularity, doc))
        })
        .collect()
}

fn character_relevance_bucket(doc: &Value, keyword: &str) -> u8 {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return 9;
    }
    let name = first_str(doc, "name");
    let name_cn = first_str(doc, "name_cn");
    let aliases = first_str(doc, "aliases");
    let name_en = first_str(doc, "name_en");
    let romaji = first_str(doc, "romaji");
    if name == keyword || name_cn == keyword {
        0
    } else if name.starts_with(keyword) || name_cn.starts_with(keyword) {
        1
    } else if aliases.split_whitespace().any(|alias| alias == keyword) {
        2
    } else if aliases.contains(keyword) {
        3
    } else if name_en.starts_with(keyword) || romaji.starts_with(keyword) {
        4
    } else {
        5
    }
}

fn subject_relevance_bucket(doc: &Value, keyword: &str) -> u8 {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return 9;
    }
    let name = first_str(doc, "name");
    let name_cn = first_str(doc, "name_cn");
    if name == keyword || name_cn == keyword {
        0
    } else if name.starts_with(keyword) || name_cn.starts_with(keyword) {
        1
    } else if name.contains(keyword) || name_cn.contains(keyword) {
        2
    } else {
        3
    }
}

fn sanitize_query(keyword: &str) -> String {
    keyword
        .trim()
        .chars()
        .filter(|ch| {
            !matches!(
                ch,
                '"' | '\'' | ':' | '^' | '(' | ')' | '{' | '}' | '[' | ']'
            )
        })
        .collect::<String>()
}

fn first_str(doc: &Value, field: &str) -> String {
    doc.get(field)
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn first_u64(doc: &Value, field: &str) -> Option<u64> {
    doc.get(field)
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_u64)
}
