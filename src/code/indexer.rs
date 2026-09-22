use std::{
    fs::{self, File},
    io::Read,
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

use super::scanner::MAX_FILE_BYTES;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceContent {
    pub content: String,
    pub size_bytes: u64,
    modified: Option<std::time::SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceReadErrorKind {
    TooLarge,
    InvalidUtf8,
    Binary,
    Changed,
    UnsafePath,
}

#[derive(Debug)]
struct SourceReadError {
    kind: SourceReadErrorKind,
    message: String,
}

impl std::fmt::Display for SourceReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SourceReadError {}

pub fn source_read_error_kind(error: &anyhow::Error) -> Option<SourceReadErrorKind> {
    error
        .downcast_ref::<SourceReadError>()
        .map(|error| error.kind)
}

pub fn read_source(root: &Path, path: &Path) -> Result<SourceContent> {
    let canonical = validated_source_path(root, path)?;
    let mut file = File::open(&canonical)
        .with_context(|| format!("No se pudo abrir {}", canonical.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("No se pudo obtener metadata de {}", canonical.display()))?;
    let bytes = read_bounded(&mut file, MAX_FILE_BYTES)?;
    let canonical_after = validated_source_path(root, path)?;
    if canonical_after != canonical {
        return unsafe_path_error(path, "La ruta cambio mientras se leia");
    }
    let after = fs::metadata(&canonical).with_context(|| {
        format!(
            "No se pudo obtener metadata final de {}",
            canonical.display()
        )
    })?;
    if metadata_changed(&before, &after) {
        return Err(SourceReadError {
            kind: SourceReadErrorKind::Changed,
            message: format!("El archivo cambio mientras se leia: {}", path.display()),
        }
        .into());
    }
    decode_source(bytes, &canonical, after.len(), after.modified().ok())
}

pub fn source_unchanged(root: &Path, path: &Path, source: &SourceContent) -> Result<bool> {
    let canonical = validated_source_path(root, path)?;
    let metadata = fs::metadata(&canonical).with_context(|| {
        format!(
            "No se pudo verificar {} antes de publicarlo",
            path.display()
        )
    })?;
    Ok(metadata.len() == source.size_bytes && metadata.modified().ok() == source.modified)
}

fn validated_source_path(root: &Path, path: &Path) -> Result<std::path::PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("No se pudo verificar {} antes de leerlo", path.display()))?;
    if is_link_or_reparse_point(&metadata) {
        return unsafe_path_error(path, "La fuente se convirtio en un enlace");
    }
    if !metadata.is_file() {
        return unsafe_path_error(path, "La fuente ya no es un archivo regular");
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("No se pudo verificar el root autorizado {}", root.display()))?;
    let canonical = path.canonicalize().with_context(|| {
        format!(
            "No se pudo canonicalizar {} antes de leerlo",
            path.display()
        )
    })?;
    if !canonical.starts_with(&canonical_root) {
        return unsafe_path_error(path, "La fuente queda fuera del root autorizado");
    }
    Ok(canonical)
}

fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

fn unsafe_path_error<T>(path: &Path, message: &str) -> Result<T> {
    Err(SourceReadError {
        kind: SourceReadErrorKind::UnsafePath,
        message: format!("{message}: {}", path.display()),
    }
    .into())
}

fn metadata_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() != after.len() || before.modified().ok() != after.modified().ok()
}

fn read_bounded(reader: &mut impl Read, max_file_bytes: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity((max_file_bytes.min(64 * 1024)) as usize);
    reader
        .take(max_file_bytes + 1)
        .read_to_end(&mut bytes)
        .context("No se pudo leer el contenido del archivo")?;
    if bytes.len() as u64 > max_file_bytes {
        return Err(SourceReadError {
            kind: SourceReadErrorKind::TooLarge,
            message: format!("El archivo supera el limite de {max_file_bytes} bytes"),
        }
        .into());
    }
    Ok(bytes)
}

fn decode_source(
    bytes: Vec<u8>,
    path: &Path,
    size_bytes: u64,
    modified: Option<std::time::SystemTime>,
) -> Result<SourceContent> {
    if bytes.contains(&0) {
        return Err(SourceReadError {
            kind: SourceReadErrorKind::Binary,
            message: format!(
                "El archivo contiene bytes NUL y se trato como binario: {}",
                path.display()
            ),
        }
        .into());
    }
    let content = String::from_utf8(bytes).map_err(|_| SourceReadError {
        kind: SourceReadErrorKind::InvalidUtf8,
        message: format!(
            "El archivo no contiene texto UTF-8 valido: {}",
            path.display()
        ),
    })?;
    Ok(SourceContent {
        content: content
            .strip_prefix('\u{feff}')
            .unwrap_or(&content)
            .to_owned(),
        size_bytes,
        modified,
    })
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

    fn read_test_source(path: &Path) -> Result<SourceContent> {
        read_source(path.parent().unwrap(), path)
    }

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
        let content = read_test_source(file.path()).unwrap();
        let _ = content_hash(&content.content);
        let _ = chunk_code(1, 1, "src/lib.rs", "rs", "rust", &content.content);
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
            match read_test_source(path) {
                Ok(content) => indexed.extend(chunk_code(
                    1,
                    1,
                    path.file_name().unwrap().to_str().unwrap(),
                    "rs",
                    "rust",
                    &content.content,
                )),
                Err(error) => errors.push(error.to_string()),
            }
        }

        assert_eq!(errors.len(), 1);
        assert_eq!(indexed.len(), 1);
        assert!(indexed[0].content.contains("fn valid"));
    }

    #[test]
    fn normalizes_utf8_bom_and_accepts_empty_files_without_chunks() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let bom = directory.path().join("bom.rs");
        let empty = directory.path().join("empty.rs");
        std::fs::write(&bom, b"\xef\xbb\xbffn bom() {}").unwrap();
        std::fs::write(&empty, []).unwrap();

        let bom_source = read_test_source(&bom).unwrap();
        let empty_source = read_test_source(&empty).unwrap();

        assert_eq!(bom_source.content, "fn bom() {}");
        assert!(chunk_code(1, 1, "empty.rs", "rs", "rust", &empty_source.content).is_empty());
    }

    #[test]
    fn rejects_invalid_utf8_and_binary_nul_without_guessing_an_encoding() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let invalid = directory.path().join("invalid.rs");
        let binary = directory.path().join("binary.rs");
        std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
        std::fs::write(&binary, b"fn binary() {}\0").unwrap();

        let invalid_error = read_test_source(&invalid).unwrap_err();
        let binary_error = read_test_source(&binary).unwrap_err();

        assert_eq!(
            source_read_error_kind(&invalid_error),
            Some(SourceReadErrorKind::InvalidUtf8)
        );
        assert_eq!(
            source_read_error_kind(&binary_error),
            Some(SourceReadErrorKind::Binary)
        );
    }

    #[test]
    fn bounded_read_rejects_files_that_grow_after_scanning() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let source = directory.path().join("growing.rs");
        std::fs::write(&source, "fn initially_small() {}").unwrap();
        let scanned_size = std::fs::metadata(&source).unwrap().len();
        let file = File::options().write(true).open(&source).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();

        let error = read_test_source(&source).unwrap_err();

        assert!(scanned_size < MAX_FILE_BYTES);
        assert_eq!(
            source_read_error_kind(&error),
            Some(SourceReadErrorKind::TooLarge)
        );
    }

    #[test]
    fn detects_source_changes_before_publication() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("changed.rs");
        std::fs::write(&path, "fn before() {}").unwrap();
        let source = read_test_source(&path).unwrap();
        std::fs::write(&path, "fn after_change_is_longer() {}").unwrap();

        assert!(!source_unchanged(path.parent().unwrap(), &path, &source).unwrap());
    }

    #[test]
    fn rejects_a_source_outside_the_authorized_root_before_opening() {
        let root = tempfile::tempdir_in("target").unwrap();
        let outside = tempfile::tempdir_in("target").unwrap();
        let source = outside.path().join("outside.rs");
        std::fs::write(&source, "fn outside() {}").unwrap();

        let error = read_source(root.path(), &source).unwrap_err();

        assert_eq!(
            source_read_error_kind(&error),
            Some(SourceReadErrorKind::UnsafePath)
        );
        assert!(error.to_string().contains("root autorizado"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_source_replaced_by_an_external_symlink_before_opening() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir_in("target").unwrap();
        let outside = tempfile::tempdir_in("target").unwrap();
        let source = root.path().join("main.rs");
        let external = outside.path().join("external.rs");
        std::fs::write(&source, "fn original() {}").unwrap();
        std::fs::write(&external, "fn external_secret() {}").unwrap();
        std::fs::remove_file(&source).unwrap();
        symlink(&external, &source).unwrap();

        let error = read_source(root.path(), &source).unwrap_err();

        assert_eq!(
            source_read_error_kind(&error),
            Some(SourceReadErrorKind::UnsafePath)
        );
        assert!(error.to_string().contains("enlace"));
    }

    #[test]
    fn read_errors_are_contextual_and_bounded_reader_stops_at_the_limit() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "fallo de lectura inyectado",
                ))
            }
        }

        let error = read_bounded(&mut FailingReader, 8).unwrap_err();
        assert!(error.to_string().contains("contenido"));

        let mut oversized = std::io::Cursor::new(vec![b'x'; 9]);
        let error = read_bounded(&mut oversized, 8).unwrap_err();
        assert_eq!(
            source_read_error_kind(&error),
            Some(SourceReadErrorKind::TooLarge)
        );
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
