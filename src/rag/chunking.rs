use crate::{documents::ParsedDocument, models::Chunk};

pub fn chunk_document(document: &ParsedDocument, size: usize, overlap: usize) -> Vec<Chunk> {
    let target = size.max(2);
    let overlap = overlap.min(target.saturating_sub(1));
    let mut result = Vec::new();
    for section in &document.sections {
        let words = section.text.split_whitespace().collect::<Vec<_>>();
        let mut start = 0;
        while start < words.len() {
            let end = (start + target).min(words.len());
            let content = words[start..end].join(" ");
            if !content.trim().is_empty() {
                result.push(Chunk {
                    id: 0,
                    document_id: 0,
                    document_name: document.title.clone(),
                    chunk_index: result.len(),
                    content,
                    page_number: section.page_number,
                    section: section.title.clone(),
                    embedding: vec![],
                });
            }
            if end == words.len() {
                break;
            }
            start = end - overlap;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::ParsedSection;
    #[test]
    fn chunks_and_overlaps() {
        let doc = ParsedDocument {
            title: "a".into(),
            sections: vec![ParsedSection {
                text: (0..20)
                    .map(|i| format!("w{i}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                page_number: None,
                title: None,
            }],
            metadata: Default::default(),
        };
        let c = chunk_document(&doc, 10, 3);
        assert_eq!(c.len(), 3);
        assert!(c[0].content.ends_with("w9"));
        assert!(c[1].content.starts_with("w7"));
    }
}
