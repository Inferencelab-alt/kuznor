use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: i64,
    pub title: String,
    pub profile: Profile,
    pub library_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: i64,
    pub chat_id: i64,
    pub role: String,
    pub content: String,
    pub sources: Vec<Source>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub id: i64,
    pub library_id: i64,
    pub name: String,
    pub original_path: String,
    pub hash: String,
    pub file_type: String,
    pub status: String,
    pub created_at: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub id: i64,
    pub document_id: i64,
    pub document_name: String,
    pub chunk_index: usize,
    pub content: String,
    pub page_number: Option<u32>,
    pub section: Option<String>,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    #[serde(default)]
    pub library_id: i64,
    #[serde(default)]
    pub project_id: i64,
    #[serde(default)]
    pub file_id: i64,
    #[serde(default)]
    pub document_id: i64,
    #[serde(default)]
    pub chunk_id: i64,
    #[serde(default)]
    pub score: f32,
    pub document_name: String,
    pub chunk_index: usize,
    pub page_number: Option<u32>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub relative_path: String,
    #[serde(default)]
    pub line_start: Option<usize>,
    #[serde(default)]
    pub line_end: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Profile {
    General,
    Programming,
    Business,
    Study,
    Documentation,
    Code,
}

impl Profile {
    pub const ALL: [Self; 3] = [Self::General, Self::Documentation, Self::Code];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Programming => "Programacion",
            Self::Business => "Negocios",
            Self::Study => "Estudio",
            Self::Documentation => "Documentos",
            Self::Code => "Codigo",
        }
    }

    pub fn system_prompt(self) -> &'static str {
        match self {
            Self::General => {
                "Eres Kuznor, un asistente de IA local desarrollado por Inference Lab. Kuznor es la aplicacion y la identidad del asistente; el modelo GGUF subyacente es un motor local elegido por el usuario y puede cambiar. Si preguntan tu nombre o que es Kuznor, responde como Kuznor y no inventes otros significados. Inference Lab desarrollo Kuznor, pero no afirmes que entreno el modelo subyacente. Si preguntan que modelo se usa, no inventes su nombre: menciona solo metadata real proporcionada por la aplicacion o explica que es el modelo local configurado por el usuario. Esto define identidad, no altera el contenido, opiniones o estilo normal del modelo. Responde en el idioma del usuario."
            }
            Self::Programming => {
                "Eres un asistente de programacion. Ayuda con debugging, errores, revision, refactor, buenas practicas y APIs. Explica el razonamiento y usa ejemplos pequenos. Nunca afirmes haber ejecutado codigo."
            }
            Self::Business => {
                "Eres un asistente de negocios practico y analitico. Ayuda a estructurar ideas, comparar opciones, resumir riesgos y proponer siguientes pasos claros. No inventes cifras ni fuentes."
            }
            Self::Study => {
                "Eres un tutor paciente y riguroso. Explica conceptos paso a paso, adapta la profundidad a la pregunta y utiliza ejemplos y preguntas de comprobacion cuando aporten valor."
            }
            Self::Documentation => {
                "Estas en modo Solo documentos. Responde exclusivamente con evidencia del contexto documental recuperado. Si no contiene evidencia suficiente, indicalo claramente y no aportes conocimiento externo."
            }
            Self::Code => {
                "Eres un asistente de analisis de codigo en modo solo lectura. Explica, revisa y propone cambios sin afirmar que ejecutaste, compilaste, probaste o modificaste archivos."
            }
        }
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for Profile {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "Programacion" => Ok(Self::Programming),
            "Negocios" => Ok(Self::Business),
            "Estudio" => Ok(Self::Study),
            "Documentacion" | "Documentos" => Ok(Self::Documentation),
            "Codigo" => Ok(Self::Code),
            _ => Ok(Self::General),
        }
    }
}
