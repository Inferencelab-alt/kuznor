use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::{
    ai::client,
    config::Settings,
    models::Chunk,
    rag::embeddings::{EmbeddingProvider, embed_chunks_with_retry},
};

use super::types::CodeChunk;

const MAX_CHUNK_LINES: usize = 80;
const MAX_CHUNK_CHARS: usize = 1_200;
const OVERLAP_LINES: usize = 6;
const EMBEDDING_BATCH_SIZE: usize = 8;
const CODE_EMBEDDING_TIMEOUT: Duration = Duration::from_secs(30);

pub struct CodeLocalProvider {
    pub settings: Settings,
    pub cancelled: Arc<AtomicBool>,
}

impl CodeLocalProvider {
    fn request(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let settings = self.settings.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result =
                client::embeddings_with_timeout(&settings, &inputs, CODE_EMBEDDING_TIMEOUT);
            let _ = sender.send(result);
        });
        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                anyhow::bail!("Indexacion cancelada");
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("El worker de embeddings termino sin respuesta")
                }
            }
        }
    }
}

impl EmbeddingProvider for CodeLocalProvider {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let inputs = texts
            .iter()
            .map(|text| format!("search_document: {text}"))
            .collect::<Vec<_>>();
        self.request(inputs)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let inputs = vec![format!("search_query: {query}")];
        Ok(self.request(inputs)?.remove(0))
    }
}

pub fn read_source(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("No se pudo leer {} como texto UTF-8", path.display()))
}

pub fn content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

pub fn needs_reindex(previous_hash: Option<&str>, current_hash: &str) -> bool {
    previous_hash != Some(current_hash)
}

pub fn chunk_code(
    project_id: i64,
    file_id: i64,
    relative_path: &str,
    extension: &str,
    language: &str,
    content: &str,
) -> Vec<CodeChunk> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        let mut characters = 0;
        while end < lines.len() && end - start < MAX_CHUNK_LINES {
            let next = lines[end].chars().count() + 1;
            if end > start && characters + next > MAX_CHUNK_CHARS {
                break;
            }
            characters += next;
            end += 1;
        }
        if end == start {
            end += 1;
        }
        let text = lines[start..end]
            .iter()
            .enumerate()
            .map(|(offset, line)| format!("{:>5} | {line}", start + offset + 1))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            chunks.push(CodeChunk {
                id: 0,
                project_id,
                file_id,
                relative_path: relative_path.into(),
                extension: extension.into(),
                language: language.into(),
                chunk_index: chunks.len(),
                line_start: start + 1,
                line_end: end,
                content: text,
                embedding: Vec::new(),
            });
        }
        if end == lines.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP_LINES).max(start + 1);
    }
    chunks
}

pub fn embed_code_chunks_cancellable<P: EmbeddingProvider>(
    provider: &P,
    chunks: Vec<CodeChunk>,
    cancelled: &AtomicBool,
) -> Result<Vec<CodeChunk>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }
    let project_id = chunks[0].project_id;
    let file_id = chunks[0].file_id;
    let relative_path = chunks[0].relative_path.clone();
    let extension = chunks[0].extension.clone();
    let language = chunks[0].language.clone();
    let temporary: Vec<Chunk> = chunks
        .into_iter()
        .map(|chunk| Chunk {
            id: 0,
            document_id: file_id,
            document_name: chunk.relative_path,
            chunk_index: chunk.chunk_index,
            content: chunk.content,
            page_number: Some(chunk.line_start as u32),
            section: Some(chunk.line_end.to_string()),
            embedding: Vec::new(),
        })
        .collect();
    let mut embedded = Vec::new();
    for batch in temporary.chunks(EMBEDDING_BATCH_SIZE) {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Indexacion cancelada");
        }
        embedded.extend(embed_chunks_with_retry(provider, batch.to_vec())?);
    }
    Ok(embedded
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| CodeChunk {
            id: 0,
            project_id,
            file_id,
            relative_path: relative_path.clone(),
            extension: extension.clone(),
            language: language.clone(),
            chunk_index: index,
            line_start: chunk.page_number.unwrap_or(1) as usize,
            line_end: chunk
                .section
                .as_deref()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1),
            content: chunk.content,
            embedding: chunk.embedding,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TestProvider;

    impl EmbeddingProvider for TestProvider {
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&self, _query: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0, 0.0])
        }
    }

    struct CancellingProvider<'a> {
        cancelled: &'a AtomicBool,
        calls: &'a AtomicUsize,
    }

    impl EmbeddingProvider for CancellingProvider<'_> {
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.cancelled.store(true, Ordering::Relaxed);
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }

        fn embed_query(&self, _query: &str) -> Result<Vec<f32>> {
            unreachable!()
        }
    }

    #[test]
    fn chunks_code_and_preserves_line_ranges() {
        let source = (1..=220)
            .map(|line| format!("let value_{line} = {line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_code(1, 2, "src/main.rs", "rs", "rust", &source);
        assert!(chunks.len() > 2);
        assert_eq!(chunks[0].line_start, 1);
        assert!(chunks[0].content.contains("    1 | let value_1"));
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.line_end >= chunk.line_start)
        );
    }

    #[test]
    fn hashes_drive_incremental_reindexing() {
        let hash = content_hash("fn main() {}");
        assert!(!needs_reindex(Some(&hash), &hash));
        assert!(needs_reindex(Some(&hash), &content_hash("fn changed() {}")));
        assert!(needs_reindex(None, &hash));
    }

    #[test]
    fn indexes_code_chunks_with_embeddings() {
        let chunks = chunk_code(
            1,
            2,
            "src/lib.rs",
            "rs",
            "rust",
            "pub fn answer() -> i32 {\n    42\n}",
        );
        let indexed =
            embed_code_chunks_cancellable(&TestProvider, chunks, &AtomicBool::new(false)).unwrap();
        assert!(!indexed.is_empty());
        assert!(indexed.iter().all(|chunk| chunk.embedding == [1.0, 0.0]));
        assert_eq!(indexed[0].relative_path, "src/lib.rs");
    }

    #[test]
    fn code_analysis_pipeline_does_not_modify_the_source_file() {
        let mut file = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile_in("target")
            .unwrap();
        std::io::Write::write_all(&mut file, b"fn original() -> u8 { 7 }").unwrap();
        let before = std::fs::read(file.path()).unwrap();
        let content = read_source(file.path()).unwrap();
        let _ = content_hash(&content);
        let _ = chunk_code(1, 1, "src/lib.rs", "rs", "rust", &content);
        assert_eq!(std::fs::read(file.path()).unwrap(), before);
    }

    #[test]
    fn unreadable_source_does_not_prevent_the_next_file_from_being_indexed() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let invalid = directory.path().join("01_invalid.rs");
        let valid = directory.path().join("02_valid.rs");
        std::fs::write(&invalid, [0xff, 0xfe, 0xfd]).unwrap();
        std::fs::write(&valid, "fn valid() -> bool { true }").unwrap();
        let mut errors = Vec::new();
        let mut indexed = Vec::new();

        for path in [&invalid, &valid] {
            match read_source(path) {
                Ok(content) => indexed.extend(chunk_code(
                    1,
                    1,
                    path.file_name().unwrap().to_str().unwrap(),
                    "rs",
                    "rust",
                    &content,
                )),
                Err(error) => errors.push(error.to_string()),
            }
        }

        assert_eq!(errors.len(), 1);
        assert_eq!(indexed.len(), 1);
        assert!(indexed[0].content.contains("fn valid"));
    }

    #[test]
    fn cancellation_stops_code_embedding_before_the_next_batch() {
        let cancelled = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let provider = CancellingProvider {
            cancelled: &cancelled,
            calls: &calls,
        };
        let chunks = (0..(EMBEDDING_BATCH_SIZE + 1))
            .map(|index| CodeChunk {
                id: 0,
                project_id: 1,
                file_id: 2,
                relative_path: "src/main.rs".into(),
                extension: "rs".into(),
                language: "rust".into(),
                chunk_index: index,
                line_start: index + 1,
                line_end: index + 1,
                content: format!("fn value_{index}() {{}}"),
                embedding: Vec::new(),
            })
            .collect();

        let error = embed_code_chunks_cancellable(&provider, chunks, &cancelled).unwrap_err();

        assert!(error.to_string().contains("Indexacion cancelada"));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
