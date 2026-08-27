use crate::rag::similarity::cosine_similarity;

use super::types::{CodeChunk, CodeFile};

const MIN_CODE_RELEVANCE: f32 = 0.18;

#[derive(Debug, Clone)]
pub struct ScoredCodeChunk {
    pub chunk: CodeChunk,
    pub score: f32,
}

pub fn targets_selected_file(query: &str) -> bool {
    let query = query.to_lowercase();
    [
        "archivo seleccionado",
        "este archivo",
        "esta funcion",
        "esta función",
        "este codigo",
        "este código",
        "esta clase",
        "refactoriza esta",
    ]
    .iter()
    .any(|phrase| query.contains(phrase))
}

pub fn search_code_scoped(
    chunks: Vec<CodeChunk>,
    query_embedding: &[f32],
    query_text: &str,
    max_results: usize,
    allowed_files: Option<&[i64]>,
) -> Vec<ScoredCodeChunk> {
    let query = query_text.to_ascii_lowercase();
    let mut scored = chunks
        .into_iter()
        .filter(|chunk| allowed_files.is_none_or(|file_ids| file_ids.contains(&chunk.file_id)))
        .filter(|chunk| chunk.embedding.len() == query_embedding.len())
        .map(|chunk| {
            let semantic = cosine_similarity(&chunk.embedding, query_embedding);
            let path_boost = chunk
                .relative_path
                .split(|character: char| !character.is_alphanumeric())
                .filter(|part| part.len() >= 3)
                .any(|part| query.contains(&part.to_ascii_lowercase()))
                .then_some(0.18)
                .unwrap_or(0.0);
            ScoredCodeChunk {
                chunk,
                score: semantic + path_boost,
            }
        })
        .filter(|item| allowed_files.is_some() || item.score >= MIN_CODE_RELEVANCE)
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.score.total_cmp(&left.score));
    scored.truncate(max_results);
    scored
}

pub fn resolve_file_scope(
    files: &[CodeFile],
    query: &str,
    selected_file: Option<i64>,
) -> Option<Vec<i64>> {
    if targets_selected_file(query) {
        return Some(selected_file.into_iter().collect());
    }
    let normalized_query = query.replace('\\', "/").to_ascii_lowercase();
    let exact_paths = files
        .iter()
        .filter(|file| normalized_query.contains(&file.relative_path.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    let exact_names = exact_paths
        .iter()
        .filter_map(|file| file.relative_path.rsplit('/').next())
        .map(str::to_ascii_lowercase)
        .collect::<std::collections::HashSet<_>>();
    let mut resolved = exact_paths.iter().map(|file| file.id).collect::<Vec<_>>();

    let mut by_name = std::collections::HashMap::<String, Vec<&CodeFile>>::new();
    for file in files {
        let name = file
            .relative_path
            .rsplit('/')
            .next()
            .unwrap_or(&file.relative_path)
            .to_ascii_lowercase();
        if normalized_query.contains(&name) {
            by_name.entry(name).or_default().push(file);
        }
    }
    if by_name.is_empty() && resolved.is_empty() {
        return None;
    }
    for (name, matches) in &by_name {
        if exact_names.contains(name) {
            continue;
        }
        if matches.len() == 1 {
            resolved.push(matches[0].id);
        } else if let Some(selected) =
            selected_file.filter(|selected| matches.iter().any(|file| file.id == *selected))
        {
            resolved.push(selected);
        }
    }
    resolved.sort_unstable();
    resolved.dedup();
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(project_id: i64, file_id: i64, embedding: Vec<f32>) -> CodeChunk {
        CodeChunk {
            id: file_id,
            project_id,
            file_id,
            relative_path: format!("src/file_{file_id}.rs"),
            extension: "rs".into(),
            language: "rust".into(),
            chunk_index: 0,
            line_start: 1,
            line_end: 3,
            content: "fn example() {}".into(),
            embedding,
        }
    }

    fn file(id: i64, path: &str) -> CodeFile {
        CodeFile {
            id,
            project_id: 1,
            relative_path: path.into(),
            hash: String::new(),
            extension: path
                .rsplit_once('.')
                .map(|(_, ext)| ext)
                .unwrap_or("")
                .into(),
            language: String::new(),
            size_bytes: 1,
            error: None,
        }
    }

    #[test]
    fn semantic_search_is_scoped_to_selected_file() {
        let result = search_code_scoped(
            vec![chunk(1, 10, vec![1.0, 0.0]), chunk(1, 11, vec![1.0, 0.0])],
            &[1.0, 0.0],
            "funcion",
            4,
            Some(&[11]),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chunk.file_id, 11);
    }

    #[test]
    fn search_never_crosses_the_chunks_it_receives() {
        let project_one = vec![chunk(1, 10, vec![1.0, 0.0])];
        let result = search_code_scoped(project_one, &[1.0, 0.0], "codigo", 4, None);
        assert!(result.iter().all(|item| item.chunk.project_id == 1));
    }

    #[test]
    fn exact_selected_path_does_not_match_generated_config_or_duplicate_names() {
        let files = vec![
            file(1, "src/config.py"),
            file(2, "generated/config.json"),
            file(3, "other/config.py"),
        ];
        assert_eq!(
            resolve_file_scope(&files, "Explica src/config.py", Some(1)),
            Some(vec![1])
        );
        assert_eq!(
            resolve_file_scope(&files, "Explica config.py", Some(1)),
            Some(vec![1])
        );
    }

    #[test]
    fn explicit_two_files_scope_contains_only_both_ids() {
        let files = vec![
            file(1, "src/config.py"),
            file(2, "src/auth_dependencies.py"),
            file(3, "generated/config.json"),
        ];
        assert_eq!(
            resolve_file_scope(
                &files,
                "Compara config.py con auth_dependencies.py",
                Some(1)
            ),
            Some(vec![1, 2])
        );
        assert_eq!(
            resolve_file_scope(
                &files,
                "Compara src/config.py con auth_dependencies.py",
                Some(1)
            ),
            Some(vec![1, 2])
        );
    }

    #[test]
    fn selected_file_scope_is_explicit() {
        assert!(targets_selected_file("Explica el archivo seleccionado"));
        assert!(targets_selected_file("¿Que hace esta funcion?"));
        assert!(!targets_selected_file("¿Donde se conecta con SQLite?"));
    }

    #[test]
    fn changing_selected_file_removes_every_chunk_from_the_previous_file() {
        let mut invoice = chunk(1, 10, vec![1.0, 0.0]);
        invoice.relative_path = "src/file_a.py".into();
        invoice.content = "def calculate_invoice(): pass".into();
        let mut logging = chunk(1, 20, vec![1.0, 0.0]);
        logging.relative_path = "src/file_b.py".into();
        logging.content = "LOG_LEVEL = 'INFO'".into();
        let result = search_code_scoped(
            vec![invoice, logging],
            &[1.0, 0.0],
            "Explica este archivo",
            4,
            Some(&[20]),
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chunk.file_id, 20);
        assert!(result[0].chunk.content.contains("LOG_LEVEL"));
        assert!(!result[0].chunk.content.contains("calculate_invoice"));
    }
}
