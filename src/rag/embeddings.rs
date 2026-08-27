use anyhow::{Result, anyhow};

use crate::{ai::client, config::Settings, models::Chunk};

pub const SAFE_EMBEDDING_TOKENS: usize = 420;

pub trait EmbeddingProvider {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn embed_query(&self, query: &str) -> Result<Vec<f32>>;
}

pub struct NomicLocalProvider<'a> {
    pub settings: &'a Settings,
}

impl EmbeddingProvider for NomicLocalProvider<'_> {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs = texts
            .iter()
            .map(|text| format!("search_document: {text}"))
            .collect::<Vec<_>>();
        client::embeddings(self.settings, &inputs)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let inputs = vec![format!("search_query: {query}")];
        Ok(client::embeddings(self.settings, &inputs)?.remove(0))
    }
}

pub fn prepare_embedding_chunks(chunks: Vec<Chunk>) -> Vec<Chunk> {
    chunks
        .into_iter()
        .flat_map(|chunk| split_chunk_for_embedding(chunk, SAFE_EMBEDDING_TOKENS))
        .collect()
}

pub fn embed_chunks_with_retry<P: EmbeddingProvider>(
    provider: &P,
    chunks: Vec<Chunk>,
) -> Result<Vec<Chunk>> {
    let chunks = chunks
        .into_iter()
        .flat_map(|chunk| split_chunk_for_embedding(chunk, SAFE_EMBEDDING_TOKENS))
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let inputs = chunks
        .iter()
        .map(|chunk| chunk.content.clone())
        .collect::<Vec<_>>();
    match provider.embed_documents(&inputs) {
        Ok(vectors) => {
            if vectors.len() != chunks.len() {
                return Err(anyhow!(
                    "Se esperaban {} embeddings y llegaron {}",
                    chunks.len(),
                    vectors.len()
                ));
            }
            Ok(chunks
                .into_iter()
                .zip(vectors)
                .map(|(mut chunk, vector)| {
                    chunk.embedding = vector;
                    chunk
                })
                .collect())
        }
        Err(error) if input_tokens_are_too_large(&error) => {
            if chunks.len() == 1 {
                let chunk = chunks.into_iter().next().unwrap();
                let parts = split_chunk_for_embedding(
                    chunk.clone(),
                    estimated_tokens(&chunk.content).div_ceil(2),
                );
                if parts.len() <= 1 {
                    return Err(error);
                }
                return embed_chunks_with_retry(provider, parts);
            }
            let midpoint = chunks.len().div_ceil(2);
            let (left, right) = chunks.split_at(midpoint);
            let mut embedded = embed_chunks_with_retry(provider, left.to_vec())?;
            embedded.extend(embed_chunks_with_retry(provider, right.to_vec())?);
            Ok(embedded)
        }
        Err(error) => Err(error),
    }
}

fn input_tokens_are_too_large(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("input")
        && message.contains("token")
        && (message.contains("too large")
            || message.contains("maximum")
            || message.contains("limit"))
}

fn split_chunk_for_embedding(chunk: Chunk, token_budget: usize) -> Vec<Chunk> {
    let parts = split_text_for_embedding(&chunk.content, token_budget);
    parts
        .into_iter()
        .map(|content| Chunk {
            content,
            ..chunk.clone()
        })
        .collect()
}

fn split_text_for_embedding(text: &str, token_budget: usize) -> Vec<String> {
    let budget = token_budget.max(1);
    let mut parts = Vec::new();
    let mut current = Vec::new();
    let mut current_tokens = 0;
    for word in text.split_whitespace() {
        let word_tokens = estimated_tokens(word);
        if !current.is_empty() && current_tokens + word_tokens > budget {
            parts.push(current.join(" "));
            current.clear();
            current_tokens = 0;
        }
        if word_tokens > budget {
            if !current.is_empty() {
                parts.push(current.join(" "));
                current.clear();
                current_tokens = 0;
            }
            let mut chars = String::new();
            for character in word.chars() {
                chars.push(character);
                if estimated_tokens(&chars) >= budget {
                    parts.push(std::mem::take(&mut chars));
                }
            }
            if !chars.is_empty() {
                parts.push(chars);
            }
        } else {
            current.push(word);
            current_tokens += word_tokens;
        }
    }
    if !current.is_empty() {
        parts.push(current.join(" "));
    }
    if parts.is_empty() && !text.trim().is_empty() {
        parts.push(text.trim().to_owned());
    }
    parts
}

fn estimated_tokens(text: &str) -> usize {
    let characters = text.chars().count();
    let words = text.split_whitespace().count();
    ((characters + 2) / 3).max((words * 3).div_ceil(2)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(content: &str) -> Chunk {
        Chunk {
            id: 7,
            document_id: 3,
            document_name: "manual.pdf".into(),
            chunk_index: 4,
            content: content.into(),
            page_number: Some(8),
            section: Some("Seccion".into()),
            embedding: vec![],
        }
    }

    #[test]
    fn nomic_prefixes_are_distinct() {
        let document = format!("search_document: {}", "dato");
        let query = format!("search_query: {}", "dato");
        assert_ne!(document, query);
    }

    #[test]
    fn long_chunks_are_split_before_embedding() {
        let chunks = prepare_embedding_chunks(vec![chunk(&"palabra ".repeat(1_200))]);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|item| estimated_tokens(&item.content) <= SAFE_EMBEDDING_TOKENS)
        );
        assert!(
            chunks
                .iter()
                .all(|item| item.document_id == 3 && item.page_number == Some(8))
        );
        assert_eq!(chunks[0].chunk_index, 4);
    }

    struct RejectingProvider;

    impl EmbeddingProvider for RejectingProvider {
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            if texts.iter().any(|text| estimated_tokens(text) > 8) {
                anyhow::bail!("input tokens is too large");
            }
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&self, _query: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }
    }

    #[test]
    fn token_limit_error_is_retried_with_smaller_chunks() {
        let result =
            embed_chunks_with_retry(&RejectingProvider, vec![chunk(&"palabra ".repeat(20))])
                .unwrap();
        assert!(result.len() > 1);
        assert!(result.iter().all(|item| item.embedding == vec![1.0, 0.0]));
        assert!(
            result
                .iter()
                .all(|item| item.document_id == 3 && item.page_number == Some(8))
        );
    }
}
