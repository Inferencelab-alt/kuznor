use std::path::PathBuf;

pub const PROJECT_READY: &str = "ready";
pub const PROJECT_CANCELLED: &str = "cancelled";
pub const PROJECT_FAILED: &str = "failed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeProject {
    pub id: i64,
    pub name: String,
    pub root_path: String,
    pub status: String,
    pub last_opened: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeFile {
    pub id: i64,
    pub project_id: i64,
    pub relative_path: String,
    pub hash: String,
    pub extension: String,
    pub language: String,
    pub size_bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeChunk {
    pub id: i64,
    pub project_id: i64,
    pub file_id: i64,
    pub relative_path: String,
    pub extension: String,
    pub language: String,
    pub chunk_index: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub content: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedCodeFile {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub extension: String,
    pub language: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub files: Vec<ScannedCodeFile>,
    pub skipped: usize,
    pub errors: Vec<String>,
}
