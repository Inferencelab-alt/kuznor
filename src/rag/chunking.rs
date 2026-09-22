use std::{collections::VecDeque, sync::atomic::AtomicBool, time::Instant};

use anyhow::Result;

use crate::{documents, documents::ParsedDocument, models::Chunk};

#[cfg(test)]
pub fn chunk_document(document: &ParsedDocument, size: usize, overlap: usize) -> Vec<Chunk> {
    let cancelled = AtomicBool::new(false);
    chunk_document_limited(
        document,
        size,
        overlap,
        usize::MAX,
        usize::MAX,
        &cancelled,
        Instant::now() + documents::DOCUMENT_PROCESSING_BUDGET,
    )
    .unwrap_or_default()
}

pub fn chunk_document_limited(
    document: &ParsedDocument,
    size: usize,
    overlap: usize,
    max_chunk_bytes: usize,
    max_chunks: usize,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<Chunk>> {
    let target = size.max(2);
    let overlap = overlap.min(target.saturating_sub(1));
    let mut result = Vec::new();
    for section in &document.sections {
        let mut words = VecDeque::<String>::new();
        for word in section.text.split_whitespace() {
            documents::ensure_active(cancelled, deadline)?;
            if word.len() > max_chunk_bytes {
                anyhow::bail!(
                    "Una palabra excede el limite de {} bytes por fragmento.",
                    max_chunk_bytes
                );
            }
            let existing_bytes = words.iter().map(|item| item.len()).sum::<usize>();
            let separator_bytes = usize::from(!words.is_empty());
            if !words.is_empty()
                && (words.len() >= target
                    || existing_bytes.saturating_add(separator_bytes + word.len())
                        > max_chunk_bytes)
            {
                push_chunk(&mut result, &words, section, max_chunks)?;
                retain_overlap(&mut words, overlap);
            }
            if !words.is_empty()
                && words.iter().map(|item| item.len()).sum::<usize>() + 1 + word.len()
                    > max_chunk_bytes
            {
                push_chunk(&mut result, &words, section, max_chunks)?;
                retain_overlap(&mut words, overlap);
            }
            words.push_back(word.to_owned());
            if words.len() >= target {
                push_chunk(&mut result, &words, section, max_chunks)?;
                retain_overlap(&mut words, overlap);
            }
        }
        if !words.is_empty() {
            push_chunk(&mut result, &words, section, max_chunks)?;
        }
    }
    Ok(result)
}

fn retain_overlap(words: &mut VecDeque<String>, overlap: usize) {
    while words.len() > overlap {
        words.pop_front();
    }
}

fn push_chunk(
    result: &mut Vec<Chunk>,
    words: &VecDeque<String>,
    section: &crate::documents::ParsedSection,
    max_chunks: usize,
) -> Result<()> {
    if result.len() >= max_chunks {
        anyhow::bail!(
            "El documento supera el limite de {} fragmentos.",
            max_chunks
        );
    }
    let content = words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if !content.trim().is_empty() {
        result.push(Chunk {
            id: 0,
            document_id: 0,
            document_name: String::new(),
            chunk_index: result.len(),
            content,
            page_number: section.page_number,
            section: section.title.clone(),
            embedding: vec![],
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::ParsedSection;

    fn document(text: String) -> ParsedDocument {
        ParsedDocument {
            title: "a".into(),
            sections: vec![ParsedSection {
                text,
                page_number: None,
                title: None,
            }],
            metadata: Default::default(),
        }
    }

    #[test]
    fn chunks_and_overlaps() {
        let doc = document(
            (0..20)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        let c = chunk_document(&doc, 10, 3);
        assert_eq!(c.len(), 3);
        assert!(c[0].content.ends_with("w9"));
        assert!(c[1].content.starts_with("w7"));
    }

    #[test]
    fn rejects_extremely_long_word() {
        let cancelled = AtomicBool::new(false);
        let error = chunk_document_limited(
            &document("x".repeat(100)),
            10,
            2,
            32,
            10,
            &cancelled,
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("palabra"));
    }
}
