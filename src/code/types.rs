use std::path::PathBuf;

pub const PROJECT_READY: &str = "ready";
pub const PROJECT_PARTIAL: &str = "partial";
pub const PROJECT_FAILED: &str = "failed";
pub const PROJECT_INDEXING: &str = "indexing";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeProjectStatus {
    Indexing,
    Ready,
    Partial,
    Failed,
}

impl CodeProjectStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => PROJECT_INDEXING,
            Self::Ready => PROJECT_READY,
            Self::Partial => PROJECT_PARTIAL,
            Self::Failed => PROJECT_FAILED,
        }
    }

    pub fn parse(status: &str) -> Option<Self> {
        match status {
            PROJECT_INDEXING | "escaneando" => Some(Self::Indexing),
            PROJECT_READY | "listo" => Some(Self::Ready),
            PROJECT_PARTIAL | "cancelled" => Some(Self::Partial),
            PROJECT_FAILED => Some(Self::Failed),
            _ => None,
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (_, Self::Indexing) => true,
            (Self::Indexing, Self::Ready | Self::Partial | Self::Failed) => true,
            (Self::Ready, Self::Partial | Self::Failed) => true,
            (Self::Partial, Self::Failed) | (Self::Failed, Self::Failed) => true,
            _ => false,
        }
    }
}

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
    pub completeness: ScanCompleteness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanCompleteness {
    Complete,
    Partial(Vec<ScanPartialReason>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanPartialReason {
    FileLimit,
    TotalBytesLimit,
    DepthLimit,
    FileSizeLimit,
    TraversalError,
    ReadError,
    SourceChanged,
    EmbeddingError,
    Cancelled,
    SingleFileScope,
}

impl ScanReport {
    pub fn complete() -> Self {
        Self {
            files: Vec::new(),
            skipped: 0,
            errors: Vec::new(),
            completeness: ScanCompleteness::Complete,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.completeness, ScanCompleteness::Complete)
    }

    pub fn mark_partial(&mut self, reason: ScanPartialReason) {
        match &mut self.completeness {
            ScanCompleteness::Complete => {
                self.completeness = ScanCompleteness::Partial(vec![reason])
            }
            ScanCompleteness::Partial(reasons) if !reasons.contains(&reason) => {
                reasons.push(reason)
            }
            ScanCompleteness::Partial(_) => {}
        }
    }
}
