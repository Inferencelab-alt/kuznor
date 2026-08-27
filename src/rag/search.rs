use crate::{models::Chunk, rag::similarity::cosine_similarity};

pub const MIN_RELEVANCE_SCORE: f32 = 0.20;
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct DocumentOverviewInput {
    pub document_id: i64,
    pub filename: String,
    pub representative_chunks: Vec<ScoredChunk>,
}

pub fn global_literal_terms(query: &str) -> Vec<String> {
    let normalized = normalize_phrase(query);
    let asks_globally = ["alguno", "alguna", "ninguno", "ninguna"]
        .iter()
        .any(|word| normalized.split_whitespace().any(|part| part == *word))
        && (normalized.contains("documento") || normalized.contains("archivo"));
    if !asks_globally {
        return Vec::new();
    }
    let mut terms = query
        .split(|character: char| !character.is_alphanumeric() && character != '@')
        .filter(|part| {
            part.len() >= 2
                && (part.contains('@')
                    || (part.len() >= 4 && part.chars().all(|character| character.is_numeric()))
                    || part
                        .chars()
                        .filter(|character| character.is_alphabetic())
                        .all(|character| character.is_uppercase()))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    terms.sort_by_key(|term| term.to_lowercase());
    terms.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    terms
}

pub fn lexical_matches(chunks: Vec<Chunk>, terms: &[String]) -> Vec<ScoredChunk> {
    if terms.is_empty() {
        return Vec::new();
    }
    let normalized_terms = terms
        .iter()
        .map(|term| term.to_lowercase())
        .collect::<Vec<_>>();
    chunks
        .into_iter()
        .filter(|chunk| {
            let content = chunk.content.to_lowercase();
            normalized_terms.iter().any(|term| content.contains(term))
        })
        .map(|chunk| ScoredChunk { chunk, score: 1.0 })
        .collect()
}
pub fn top_k(chunks: Vec<Chunk>, query: &[f32], query_text: &str, k: usize) -> Vec<ScoredChunk> {
    let query_terms = terms(query_text);
    let mut scored = chunks
        .into_iter()
        .filter(|c| c.embedding.len() == query.len())
        .map(|chunk| {
            let semantic = cosine_similarity(&chunk.embedding, query);
            let metadata_boost = document_name_boost(&chunk.document_name, &query_terms);
            let score = semantic + metadata_boost;
            ScoredChunk { chunk, score }
        })
        .filter(|item| item.score >= MIN_RELEVANCE_SCORE)
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored.truncate(k);
    scored
}

pub fn retrieve(chunks: Vec<Chunk>, query: &[f32], query_text: &str, k: usize) -> Vec<ScoredChunk> {
    if is_document_overview(query_text) {
        overview_chunks(chunks, query, query_text, k)
    } else {
        top_k(chunks, query, query_text, k)
    }
}

pub fn mentioned_documents(chunks: &[Chunk], query: &str) -> Vec<(i64, String)> {
    let normalized_query = normalize_phrase(query);
    let mut documents = Vec::new();
    for chunk in chunks {
        if documents.iter().any(|(id, _)| *id == chunk.document_id) {
            continue;
        }
        let stem = chunk
            .document_name
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(&chunk.document_name);
        let normalized_name = normalize_phrase(stem);
        let name_terms = terms(stem);
        let matching_terms = name_terms
            .iter()
            .filter(|term| {
                normalized_query
                    .split_whitespace()
                    .any(|query_term| query_term == term.as_str())
            })
            .count();
        if normalized_query.contains(&normalized_name) || matching_terms >= name_terms.len().min(2)
        {
            documents.push((chunk.document_id, chunk.document_name.clone()));
        }
    }
    if documents.is_empty() {
        if let Some(page_count) = mentioned_page_count(query) {
            for chunk in chunks {
                if documents.iter().any(|(id, _)| *id == chunk.document_id) {
                    continue;
                }
                let document_pages = chunks
                    .iter()
                    .filter(|candidate| candidate.document_id == chunk.document_id)
                    .filter_map(|candidate| candidate.page_number)
                    .max();
                if document_pages == Some(page_count) {
                    documents.push((chunk.document_id, chunk.document_name.clone()));
                }
            }
        }
    }
    documents
}

pub fn build_library_overview(
    chunks: Vec<Chunk>,
    documents: &[(i64, String)],
    max_per_document: usize,
) -> Vec<DocumentOverviewInput> {
    let mut overview = Vec::with_capacity(documents.len());
    let limit = max_per_document.max(1);
    for (document_id, filename) in documents {
        let mut document_chunks = chunks
            .iter()
            .filter(|chunk| chunk.document_id == *document_id)
            .cloned()
            .collect::<Vec<_>>();
        document_chunks.sort_by_key(|chunk| chunk.chunk_index);
        let mut representative_chunks: Vec<ScoredChunk> = Vec::new();
        let candidate_indices = representative_indices(document_chunks.len(), limit);
        for index in candidate_indices {
            let chunk = document_chunks[index].clone();
            if representative_chunks
                .iter()
                .all(|existing| !substantially_similar(&existing.chunk.content, &chunk.content))
            {
                representative_chunks.push(ScoredChunk { chunk, score: 1.0 });
            }
        }
        overview.push(DocumentOverviewInput {
            document_id: *document_id,
            filename: filename.clone(),
            representative_chunks,
        });
    }
    overview
}

fn representative_indices(total: usize, limit: usize) -> Vec<usize> {
    if total == 0 || limit == 0 {
        return Vec::new();
    }
    let count = total.min(limit);
    if count == 1 {
        return vec![0];
    }
    (0..count)
        .map(|position| position * (total - 1) / (count - 1))
        .collect()
}

fn substantially_similar(left: &str, right: &str) -> bool {
    let normalized_left = normalize_phrase(left);
    let normalized_right = normalize_phrase(right);
    let tokens = |value: &str| {
        normalize_phrase(value)
            .split_whitespace()
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>()
    };
    let left = tokens(left);
    let right = tokens(right);
    if left.is_empty() || right.is_empty() {
        return normalized_left == normalized_right;
    }
    let intersection = left.intersection(&right).count();
    let union = left.union(&right).count();
    intersection * 100 >= union * 85
}

pub fn retrieve_comparison(
    chunks: Vec<Chunk>,
    query: &[f32],
    query_text: &str,
    k: usize,
    documents: &[(i64, String)],
) -> Vec<ScoredChunk> {
    if documents.is_empty() || k == 0 {
        return Vec::new();
    }
    let per_document = k.max(documents.len()).div_ceil(documents.len());
    let mut result = Vec::new();
    for (document_id, _) in documents {
        let document_chunks = chunks
            .iter()
            .filter(|chunk| chunk.document_id == *document_id)
            .cloned()
            .collect();
        let mut selected = top_k(document_chunks, query, query_text, per_document);
        selected.sort_by(|a, b| {
            a.chunk
                .page_number
                .cmp(&b.chunk.page_number)
                .then(a.chunk.chunk_index.cmp(&b.chunk.chunk_index))
        });
        result.extend(selected);
    }
    result
}

pub fn restrict_to_documents(chunks: Vec<Chunk>, documents: &[(i64, String)]) -> Vec<Chunk> {
    let allowed = documents
        .iter()
        .map(|(document_id, _)| *document_id)
        .collect::<std::collections::HashSet<_>>();
    chunks
        .into_iter()
        .filter(|chunk| allowed.contains(&chunk.document_id))
        .collect()
}

pub fn is_document_overview(query: &str) -> bool {
    let normalized = query.to_lowercase();
    [
        "de que trata",
        "de qué trata",
        "resume el documento",
        "resume este documento",
        "que contiene",
        "qué contiene",
        "que temas aborda",
        "qué temas aborda",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

fn overview_chunks(
    chunks: Vec<Chunk>,
    query: &[f32],
    query_text: &str,
    k: usize,
) -> Vec<ScoredChunk> {
    if k == 0 {
        return Vec::new();
    }
    let query_terms = terms(query_text);
    let mut scored = chunks
        .into_iter()
        .filter(|chunk| chunk.embedding.len() == query.len())
        .map(|chunk| {
            let score = cosine_similarity(&chunk.embedding, query)
                + document_name_boost(&chunk.document_name, &query_terms);
            ScoredChunk { chunk, score }
        })
        .collect::<Vec<_>>();
    let mentioned_documents = scored
        .iter()
        .filter(|item| document_name_boost(&item.chunk.document_name, &query_terms) > 0.0)
        .map(|item| item.chunk.document_id)
        .collect::<std::collections::HashSet<_>>();
    let candidate_documents = if !mentioned_documents.is_empty() {
        mentioned_documents
    } else {
        scored
            .iter()
            .filter(|item| item.score >= MIN_RELEVANCE_SCORE)
            .map(|item| item.chunk.document_id)
            .collect()
    };
    scored.retain(|item| candidate_documents.contains(&item.chunk.document_id));
    if scored.is_empty() {
        return Vec::new();
    }

    let mut ordered = scored;
    ordered.sort_by(|a, b| {
        a.chunk
            .document_id
            .cmp(&b.chunk.document_id)
            .then(a.chunk.chunk_index.cmp(&b.chunk.chunk_index))
    });
    let mut selected = Vec::new();
    let mut seen_pages = std::collections::HashSet::new();
    for item in &ordered {
        if selected.len() >= k {
            break;
        }
        if selected
            .iter()
            .all(|chosen: &ScoredChunk| chosen.chunk.document_id != item.chunk.document_id)
        {
            selected.push(item.clone());
            seen_pages.insert((item.chunk.document_id, item.chunk.page_number));
        }
    }
    for item in &ordered {
        if selected.len() >= k {
            break;
        }
        let page = (item.chunk.document_id, item.chunk.page_number);
        if seen_pages.insert(page) {
            selected.push(item.clone());
        }
    }
    ordered.sort_by(|a, b| b.score.total_cmp(&a.score));
    for item in ordered {
        if selected.len() >= k {
            break;
        }
        if selected
            .iter()
            .all(|chosen: &ScoredChunk| chosen.chunk.id != item.chunk.id)
        {
            selected.push(item);
        }
    }
    selected
}

#[cfg(test)]
mod comparison_tests {
    use super::*;

    #[test]
    fn comparison_resolves_two_explicit_documents_and_retrieves_each() {
        let chunks = vec![
            Chunk {
                id: 1,
                document_id: 10,
                document_name: "Cotizacion_Inference_Lab_Redes_y_Reels.pdf".into(),
                chunk_index: 0,
                content: "Costo y alcance del servicio.".into(),
                page_number: Some(1),
                section: None,
                embedding: vec![1.0, 0.0],
            },
            Chunk {
                id: 2,
                document_id: 10,
                document_name: "Cotizacion_Inference_Lab_Redes_y_Reels.pdf".into(),
                chunk_index: 1,
                content: "Plazos y entregables.".into(),
                page_number: Some(4),
                section: None,
                embedding: vec![1.0, 0.0],
            },
            Chunk {
                id: 3,
                document_id: 20,
                document_name: "Examen_simulacion_INEGI.pdf".into(),
                chunk_index: 0,
                content: "Calidad de datos y codificacion.".into(),
                page_number: Some(2),
                section: None,
                embedding: vec![1.0, 0.0],
            },
        ];
        let documents = mentioned_documents(
            &chunks,
            "Compara Cotizacion_Inference_Lab_Redes_y_Reels.pdf con Examen_simulacion_INEGI.pdf",
        );
        assert_eq!(
            documents.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [10, 20]
        );
        let result =
            retrieve_comparison(chunks, &[1.0, 0.0], "compara costo y temas", 4, &documents);
        assert_eq!(
            result
                .iter()
                .map(|item| item.chunk.document_id)
                .collect::<std::collections::HashSet<_>>(),
            [10, 20].into_iter().collect()
        );
    }

    #[test]
    fn explicit_single_document_scope_excludes_every_other_document() {
        let chunks = vec![
            Chunk {
                id: 1,
                document_id: 10,
                document_name: "ventas.pdf".into(),
                chunk_index: 0,
                content: "ventas".into(),
                page_number: Some(1),
                section: None,
                embedding: vec![1.0, 0.0],
            },
            Chunk {
                id: 2,
                document_id: 20,
                document_name: "examen.pdf".into(),
                chunk_index: 0,
                content: "examen".into(),
                page_number: Some(1),
                section: None,
                embedding: vec![1.0, 0.0],
            },
        ];
        let scoped = restrict_to_documents(chunks, &[(10, "ventas.pdf".into())]);
        assert_eq!(scoped.len(), 1);
        assert!(scoped.iter().all(|chunk| chunk.document_id == 10));
    }

    #[test]
    fn library_overview_selects_context_for_every_real_document() {
        let chunks = (1..=3)
            .flat_map(|document_id| {
                (0..2).map(move |chunk_index| Chunk {
                    id: document_id * 10 + chunk_index,
                    document_id,
                    document_name: format!("documento-{document_id}.pdf"),
                    chunk_index: chunk_index as usize,
                    content: format!("contenido {document_id}-{chunk_index}"),
                    page_number: Some(chunk_index as u32 + 1),
                    section: None,
                    embedding: vec![],
                })
            })
            .collect();
        let documents = vec![
            (1, "documento-1.pdf".into()),
            (2, "documento-2.pdf".into()),
            (3, "documento-3.pdf".into()),
        ];
        let result = build_library_overview(chunks, &documents, 2);
        let ids = result
            .iter()
            .map(|item| item.document_id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids, [1, 2, 3].into_iter().collect());
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].filename, "documento-1.pdf");
        assert!(
            result
                .iter()
                .all(|document| document.representative_chunks.len() == 2)
        );
    }

    #[test]
    fn global_literal_search_covers_every_document_in_scope() {
        let chunks = (1..=3)
            .map(|document_id| Chunk {
                id: document_id,
                document_id,
                document_name: format!("doc-{document_id}.pdf"),
                chunk_index: 0,
                content: if document_id == 3 {
                    "RFC ABC010101XX0".into()
                } else {
                    "Sin identificador fiscal".into()
                },
                page_number: Some(1),
                section: None,
                embedding: vec![],
            })
            .collect();
        let terms = global_literal_terms("¿Alguno de los documentos contiene RFC?");
        assert_eq!(terms, ["RFC"]);
        let found = lexical_matches(chunks, &terms);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chunk.document_id, 3);
    }

    #[test]
    fn ordinary_semantic_question_is_not_global_literal_search() {
        assert!(global_literal_terms("¿Que temas de SQL aparecen?").is_empty());
    }

    #[test]
    fn overview_deduplicates_repeated_representative_text() {
        let chunks = vec![
            Chunk {
                id: 1,
                document_id: 1,
                document_name: "A.pdf".into(),
                chunk_index: 0,
                content: "Mismo encabezado".into(),
                page_number: Some(1),
                section: None,
                embedding: vec![],
            },
            Chunk {
                id: 2,
                document_id: 1,
                document_name: "A.pdf".into(),
                chunk_index: 1,
                content: "Mismo encabezado".into(),
                page_number: Some(2),
                section: None,
                embedding: vec![],
            },
        ];
        let overview = build_library_overview(chunks, &[(1, "A.pdf".into())], 2);
        assert_eq!(overview[0].representative_chunks.len(), 1);
    }

    #[test]
    fn overview_covers_beginning_middle_and_end() {
        let chunks = (0..5)
            .map(|chunk_index| Chunk {
                id: chunk_index + 1,
                document_id: 1,
                document_name: "A.pdf".into(),
                chunk_index: chunk_index as usize,
                content: format!("Seccion unica numero {chunk_index}"),
                page_number: Some(chunk_index as u32 + 1),
                section: None,
                embedding: vec![],
            })
            .collect();
        let overview = build_library_overview(chunks, &[(1, "A.pdf".into())], 3);
        assert_eq!(
            overview[0]
                .representative_chunks
                .iter()
                .map(|item| item.chunk.chunk_index)
                .collect::<Vec<_>>(),
            [0, 2, 4]
        );
    }
}

fn terms(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| term.chars().count() >= 3)
        .collect()
}

fn normalize_phrase(text: &str) -> String {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn mentioned_page_count(query: &str) -> Option<u32> {
    let words = normalize_phrase(query)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    words.windows(2).find_map(|pair| {
        matches!(
            pair[1].as_str(),
            "pagina" | "paginas" | "página" | "páginas"
        )
        .then(|| pair[0].parse().ok())
        .flatten()
    })
}

fn document_name_boost(document_name: &str, query_terms: &[String]) -> f32 {
    let name_terms = terms(document_name);
    let matches = query_terms
        .iter()
        .filter(|term| name_terms.iter().any(|name_term| name_term == *term))
        .count();
    match matches {
        0 => 0.0,
        1 => 0.22,
        _ => 0.22 + (matches.saturating_sub(1) as f32 * 0.06).min(0.18),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn top_k_is_sorted() {
        let mk = |id, e| Chunk {
            id,
            document_id: 1,
            document_name: "d".into(),
            chunk_index: (id - 1) as usize,
            content: String::new(),
            page_number: None,
            section: None,
            embedding: e,
        };
        let r = top_k(
            vec![mk(1, vec![0., 1.]), mk(2, vec![1., 0.])],
            &[1., 0.],
            "consulta",
            1,
        );
        assert_eq!(r[0].chunk.id, 2);
    }

    #[test]
    fn top_k_is_a_maximum_and_filters_irrelevant_chunks() {
        let mk = |id, score| Chunk {
            id,
            document_id: 1,
            document_name: format!("doc-{id}.txt"),
            chunk_index: id as usize,
            content: String::new(),
            page_number: None,
            section: None,
            embedding: vec![score, (1.0 - score * score).max(0.0).sqrt()],
        };
        let result = top_k(
            vec![mk(1, 0.99), mk(2, 0.98), mk(3, 0.01)],
            &[1.0, 0.0],
            "consulta",
            5,
        );
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|item| item.chunk.id != 3));
    }

    #[test]
    fn document_name_can_rescue_a_relevant_named_document() {
        let chunk = Chunk {
            id: 1,
            document_id: 1,
            document_name: "Examen_simulacion_INEGI.pdf".into(),
            chunk_index: 0,
            content: "SIMULACION DE EXAMEN".into(),
            page_number: Some(2),
            section: None,
            embedding: vec![0.0, 1.0],
        };
        let result = top_k(vec![chunk], &[1.0, 0.0], "examen INEGI", 4);
        assert_eq!(result.len(), 1);
        assert!(result[0].score >= MIN_RELEVANCE_SCORE);
    }

    #[test]
    fn overview_selects_first_and_representative_pages() {
        let make = |id: i64, page: u32, content: &str| Chunk {
            id,
            document_id: 1,
            document_name: "Examen_simulacion_INEGI.pdf".into(),
            chunk_index: (id - 1) as usize,
            content: content.into(),
            page_number: Some(page),
            section: None,
            embedding: vec![1.0, 0.0],
        };
        let result = retrieve(
            vec![
                make(1, 1, "SIMULACION DE EXAMEN"),
                make(2, 4, "Calidad y codificacion de datos"),
                make(3, 9, "Bases de datos y SQL conceptual"),
            ],
            &[0.0, 1.0],
            "¿De que trata el examen?",
            4,
        );
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].chunk.chunk_index, 0);
        assert!(result.iter().any(|item| item.chunk.page_number == Some(9)));
    }

    #[test]
    fn overview_without_evidence_returns_no_chunks() {
        let chunk = Chunk {
            id: 1,
            document_id: 1,
            document_name: "manual.txt".into(),
            chunk_index: 0,
            content: "contenido unrelated".into(),
            page_number: Some(1),
            section: None,
            embedding: vec![0.0, 1.0],
        };
        assert!(retrieve(vec![chunk], &[1.0, 0.0], "¿De que trata el examen?", 4).is_empty());
    }
}
