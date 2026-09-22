use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::{
    code::types::{CodeChunk, CodeFile, CodeProject, CodeProjectStatus, ScanReport},
    models::{Chat, Chunk, Document, Library, Message, Profile, Source},
};

fn valid_library_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("El nombre de la biblioteca no puede estar vacio");
    }
    Ok(name)
}

const CURRENT_SCHEMA_VERSION: i32 = 1;

const CREATE_TABLES_V1: &str = r#"
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE libraries (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL
    );
    CREATE TABLE chats (
        id INTEGER PRIMARY KEY, title TEXT NOT NULL, profile TEXT NOT NULL,
        library_id INTEGER REFERENCES libraries(id) ON DELETE SET NULL,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL
    );
    CREATE TABLE messages (
        id INTEGER PRIMARY KEY, chat_id INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
        role TEXT NOT NULL, content TEXT NOT NULL, sources_json TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL
    );
    CREATE TABLE documents (
        id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        name TEXT NOT NULL, original_path TEXT NOT NULL, hash TEXT NOT NULL,
        file_type TEXT NOT NULL, status TEXT NOT NULL, error TEXT, created_at TEXT NOT NULL,
        UNIQUE(library_id, hash)
    );
    CREATE TABLE document_chunks (
        id INTEGER PRIMARY KEY, document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
        chunk_index INTEGER NOT NULL, content TEXT NOT NULL, page_number INTEGER,
        section TEXT, embedding BLOB NOT NULL, UNIQUE(document_id, chunk_index)
    );
    CREATE TABLE code_projects (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL UNIQUE,
        status TEXT NOT NULL DEFAULT 'listo', last_opened TEXT NOT NULL
    );
    CREATE TABLE code_files (
        id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES code_projects(id) ON DELETE CASCADE,
        relative_path TEXT NOT NULL, hash TEXT NOT NULL, extension TEXT NOT NULL,
        language TEXT NOT NULL, size_bytes INTEGER NOT NULL, error TEXT,
        indexed_at TEXT NOT NULL, UNIQUE(project_id, relative_path)
    );
    CREATE TABLE code_chunks (
        id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
        chunk_index INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
        content TEXT NOT NULL, embedding BLOB NOT NULL, UNIQUE(file_id, chunk_index)
    );
"#;

const CREATE_INDEXES_V1: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, id);
    CREATE INDEX IF NOT EXISTS idx_documents_library ON documents(library_id);
    CREATE INDEX IF NOT EXISTS idx_chunks_document ON document_chunks(document_id);
    CREATE INDEX IF NOT EXISTS idx_code_files_project ON code_files(project_id);
    CREATE INDEX IF NOT EXISTS idx_code_chunks_file ON code_chunks(file_id);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseState {
    Empty,
    LegacyV0,
    CurrentV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preflight {
    version: i32,
    state: DatabaseState,
}

#[derive(Debug, Clone)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Self {
            path: path.as_ref().to_path_buf(),
        };
        db.initialize()?;
        Ok(db)
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("No se pudo abrir SQLite: {}", self.path.display()))?;
        Self::configure_connection(&connection)?;
        Ok(connection)
    }

    fn configure_connection(connection: &Connection) -> Result<()> {
        Self::configure_initialization_connection(connection)?;
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(())
    }

    fn configure_initialization_connection(connection: &Connection) -> Result<()> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(())
    }

    fn initialize(&self) -> Result<()> {
        let preflight = if self.path.exists() {
            let connection =
                Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .with_context(|| {
                        format!(
                            "No se pudo abrir SQLite para verificar la base local: {}",
                            self.path.display()
                        )
                    })?;
            Self::preflight(&connection)?
        } else {
            let connection = self.initialization_connection()?;
            let preflight = Self::preflight(&connection)?;
            Self::initialize_preflighted(connection, preflight)?;
            return Ok(());
        };

        let connection = self.initialization_connection()?;
        let current = Self::classify_database(&connection)?;
        if current != preflight {
            anyhow::bail!(
                "La base de datos local cambio mientras Kuznor la verificaba; cierre otras instancias e intente de nuevo"
            );
        }
        Self::initialize_preflighted(connection, preflight)
    }

    fn initialization_connection(&self) -> Result<Connection> {
        Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| {
            format!(
                "No se pudo abrir SQLite para inicializar la base local: {}",
                self.path.display()
            )
        })
    }

    fn initialize_preflighted(mut connection: Connection, preflight: Preflight) -> Result<()> {
        Self::configure_initialization_connection(&connection)?;
        match preflight.state {
            DatabaseState::CurrentV1 => Self::enable_wal(&connection),
            DatabaseState::Empty | DatabaseState::LegacyV0 => {
                Self::migrate_zero_to_one(&mut connection, preflight.state)?;
                Self::run_quick_check(&connection)?;
                Self::run_foreign_key_check(&connection)?;
                Self::enable_wal(&connection)
            }
        }
    }

    fn enable_wal(connection: &Connection) -> Result<()> {
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(())
    }

    fn preflight(connection: &Connection) -> Result<Preflight> {
        Self::run_quick_check(connection)?;
        let preflight = Self::classify_database(connection)?;
        if preflight.state == DatabaseState::CurrentV1 {
            Self::run_foreign_key_check(connection)?;
        }
        Ok(preflight)
    }

    fn classify_database(connection: &Connection) -> Result<Preflight> {
        let version = Self::schema_version(connection)?;
        let state = match version {
            0 if Self::is_empty_database(connection)? => DatabaseState::Empty,
            0 => {
                validate_schema(connection, false).context(
                    "La base local con user_version=0 no coincide con el esquema legacy v0.1 esperado",
                )?;
                DatabaseState::LegacyV0
            }
            CURRENT_SCHEMA_VERSION => {
                validate_schema(connection, true)
                    .context("La base local no coincide con el esquema Kuznor v1 esperado")?;
                DatabaseState::CurrentV1
            }
            version if version > CURRENT_SCHEMA_VERSION => anyhow::bail!(
                "La base de datos local usa el esquema {version}, mas reciente que el esquema {} soportado por esta version de Kuznor. Actualice Kuznor; la base no fue modificada.",
                CURRENT_SCHEMA_VERSION
            ),
            version => anyhow::bail!(
                "La base de datos local tiene user_version={version}, que Kuznor no reconoce. La base no fue modificada."
            ),
        };
        Ok(Preflight { version, state })
    }

    fn migrate_zero_to_one(connection: &mut Connection, state: DatabaseState) -> Result<()> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match state {
            DatabaseState::Empty => transaction.execute_batch(CREATE_TABLES_V1)?,
            DatabaseState::LegacyV0 => transaction.execute_batch(CREATE_INDEXES_V1)?,
            DatabaseState::CurrentV1 => {
                anyhow::bail!("Migracion 0 a 1 solicitada para una base ya actualizada")
            }
        }
        if state == DatabaseState::Empty {
            transaction.execute_batch(CREATE_INDEXES_V1)?;
        }
        validate_schema(&transaction, true)?;
        Self::run_foreign_key_check(&transaction)?;
        Self::run_quick_check(&transaction)?;
        transaction.execute_batch(&format!("PRAGMA user_version={CURRENT_SCHEMA_VERSION};"))?;
        transaction.commit()?;
        Ok(())
    }

    fn schema_version(connection: &Connection) -> Result<i32> {
        connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .context("No se pudo leer PRAGMA user_version de la base local")
    }

    fn is_empty_database(connection: &Connection) -> Result<bool> {
        let object_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type IN ('table','index','trigger','view') AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        Ok(object_count == 0)
    }

    fn run_quick_check(connection: &Connection) -> Result<()> {
        let mut statement = connection.prepare("PRAGMA quick_check")?;
        let results = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if results
            .iter()
            .all(|result| result.eq_ignore_ascii_case("ok"))
        {
            Ok(())
        } else {
            anyhow::bail!(
                "La base de datos local no supero PRAGMA quick_check: {}. El archivo fue preservado.",
                results.join("; ")
            );
        }
    }

    fn run_foreign_key_check(connection: &Connection) -> Result<()> {
        let issue = connection
            .query_row("PRAGMA foreign_key_check", [], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?;
        if let Some((table, row_id, parent)) = issue {
            anyhow::bail!(
                "La base de datos local tiene una relacion invalida: tabla {table}, fila {:?}, tabla padre {parent}. El archivo fue preservado.",
                row_id
            );
        }
        Ok(())
    }

    pub fn load_settings(&self) -> Result<HashMap<String, String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_setting(&self, key: &str, value: &str) -> Result<()> {
        self.connection()?.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn create_chat(&self, title: &str, profile: Profile) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO chats(title,profile,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            params![title, profile.label(), now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_chats(&self) -> Result<Vec<Chat>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,title,profile,library_id,created_at,updated_at FROM chats ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(Chat {
                id: r.get(0)?,
                title: r.get(1)?,
                profile: Profile::from_str(&r.get::<_, String>(2)?).unwrap_or(Profile::General),
                library_id: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_chats_for_profile(&self, profile: Profile) -> Result<Vec<Chat>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id,title,profile,library_id,created_at,updated_at FROM chats WHERE profile=?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([profile.label()], |r| {
            Ok(Chat {
                id: r.get(0)?,
                title: r.get(1)?,
                profile: Profile::from_str(&r.get::<_, String>(2)?).unwrap_or(Profile::General),
                library_id: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_chat(
        &self,
        id: i64,
        title: &str,
        profile: Profile,
        library_id: Option<i64>,
    ) -> Result<()> {
        self.connection()?.execute(
            "UPDATE chats SET title=?1,profile=?2,library_id=?3,updated_at=?4 WHERE id=?5",
            params![
                title,
                profile.label(),
                library_id,
                Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn delete_chat(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM chats WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn clear_chat(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM messages WHERE chat_id=?1", [id])?;
        Ok(())
    }

    pub fn add_message(
        &self,
        chat_id: i64,
        role: &str,
        content: &str,
        sources: &[Source],
    ) -> Result<i64> {
        let conn = self.connection()?;
        conn.execute("INSERT INTO messages(chat_id,role,content,sources_json,created_at) VALUES(?1,?2,?3,?4,?5)", params![chat_id, role, content, serde_json::to_string(sources)?, Utc::now().to_rfc3339()])?;
        conn.execute(
            "UPDATE chats SET updated_at=?1 WHERE id=?2",
            params![Utc::now().to_rfc3339(), chat_id],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn messages(&self, chat_id: i64) -> Result<Vec<Message>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,chat_id,role,content,sources_json,created_at FROM messages WHERE chat_id=?1 ORDER BY id")?;
        let rows = stmt.query_map([chat_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (id, chat_id, role, content, sources_json, created_at) = row?;
            let sources = serde_json::from_str(&sources_json).with_context(|| {
                format!("sources_json invalido en el mensaje {id} del chat {chat_id}")
            })?;
            messages.push(Message {
                id,
                chat_id,
                role,
                content,
                sources,
                created_at,
            });
        }
        Ok(messages)
    }

    pub fn create_library(&self, name: &str) -> Result<i64> {
        let name = valid_library_name(name)?;
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO libraries(name,created_at) VALUES(?1,?2)",
            params![name, Utc::now().to_rfc3339()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_libraries(&self) -> Result<Vec<Library>> {
        let conn = self.connection()?;
        let mut stmt =
            conn.prepare("SELECT id,name,created_at FROM libraries ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Library {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_library(&self, id: i64, name: &str) -> Result<()> {
        let name = valid_library_name(name)?;
        self.connection()?.execute(
            "UPDATE libraries SET name=?1 WHERE id=?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn delete_library(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM libraries WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn document_by_hash(&self, library_id: i64, hash: &str) -> Result<Option<Document>> {
        let conn = self.connection()?;
        conn.query_row("SELECT id,library_id,name,original_path,hash,file_type,status,created_at,error FROM documents WHERE library_id=?1 AND hash=?2", params![library_id,hash], document_row).optional().map_err(Into::into)
    }

    pub fn create_document(
        &self,
        library_id: i64,
        name: &str,
        path: &str,
        hash: &str,
        kind: &str,
    ) -> Result<i64> {
        let conn = self.connection()?;
        conn.execute("INSERT INTO documents(library_id,name,original_path,hash,file_type,status,created_at) VALUES(?1,?2,?3,?4,?5,'indexando',?6)",params![library_id,name,path,hash,kind,Utc::now().to_rfc3339()])?;
        Ok(conn.last_insert_rowid())
    }

    pub fn replace_document(&self, id: i64, path: &str, name: &str, kind: &str) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM document_chunks WHERE document_id=?1", [id])?;
        tx.execute("UPDATE documents SET original_path=?1,name=?2,file_type=?3,status='indexando',error=NULL WHERE id=?4",params![path,name,kind,id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_documents(&self, library_id: i64) -> Result<Vec<Document>> {
        let conn = self.connection()?;
        let mut stmt=conn.prepare("SELECT id,library_id,name,original_path,hash,file_type,status,created_at,error FROM documents WHERE library_id=?1 ORDER BY name")?;
        let rows = stmt.query_map([library_id], document_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_document(&self, id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM documents WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn set_document_status(&self, id: i64, status: &str, error: Option<&str>) -> Result<()> {
        self.connection()?.execute(
            "UPDATE documents SET status=?1,error=?2 WHERE id=?3",
            params![status, error, id],
        )?;
        Ok(())
    }

    pub fn save_chunks(&self, document_id: i64, chunks: &[Chunk]) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM document_chunks WHERE document_id=?1",
            [document_id],
        )?;
        {
            let mut stmt=tx.prepare("INSERT INTO document_chunks(document_id,chunk_index,content,page_number,section,embedding) VALUES(?1,?2,?3,?4,?5,?6)")?;
            for chunk in chunks {
                stmt.execute(params![
                    document_id,
                    chunk.chunk_index as i64,
                    chunk.content,
                    chunk.page_number,
                    chunk.section,
                    embedding_to_bytes(&chunk.embedding)
                ])?;
            }
        }
        tx.execute(
            "UPDATE documents SET status='listo',error=NULL WHERE id=?1",
            [document_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn library_chunks(&self, library_id: i64) -> Result<Vec<Chunk>> {
        let conn = self.connection()?;
        let mut stmt=conn.prepare("SELECT c.id,c.document_id,d.name,c.chunk_index,c.content,c.page_number,c.section,c.embedding FROM document_chunks c JOIN documents d ON d.id=c.document_id WHERE d.library_id=?1 AND d.status='listo'")?;
        let rows = stmt.query_map([library_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Vec<u8>>(7)?,
            ))
        })?;
        let mut chunks = Vec::new();
        for row in rows {
            let (id, document_id, document_name, chunk_index, content, page_number, section, bytes) =
                row?;
            let embedding = bytes_to_embedding(&bytes).with_context(|| {
                format!("Embedding invalido en el chunk {id} del documento {document_id}")
            })?;
            chunks.push(Chunk {
                id,
                document_id,
                document_name,
                chunk_index: chunk_index as usize,
                content,
                page_number: page_number.map(|page| page as u32),
                section,
                embedding,
            });
        }
        Ok(chunks)
    }

    pub fn upsert_code_project(&self, name: &str, root_path: &str) -> Result<i64> {
        let conn = self.connection()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO code_projects(name,root_path,status,last_opened) VALUES(?1,?2,?3,?4) ON CONFLICT(root_path) DO UPDATE SET name=excluded.name,status=excluded.status,last_opened=excluded.last_opened",
            params![name, root_path, CodeProjectStatus::Indexing.as_str(), now],
        )?;
        conn.query_row(
            "SELECT id FROM code_projects WHERE root_path=?1",
            [root_path],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn code_project_id_by_root_path(&self, root_path: &str) -> Result<Option<i64>> {
        self.connection()?
            .query_row(
                "SELECT id FROM code_projects WHERE root_path=?1",
                [root_path],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_code_projects(&self) -> Result<Vec<CodeProject>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id,name,root_path,status,last_opened FROM code_projects ORDER BY last_opened DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CodeProject {
                id: row.get(0)?,
                name: row.get(1)?,
                root_path: row.get(2)?,
                status: row.get(3)?,
                last_opened: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn remove_code_project(&self, project_id: i64) -> Result<()> {
        self.connection()?
            .execute("DELETE FROM code_projects WHERE id=?1", [project_id])?;
        Ok(())
    }

    pub fn transition_code_project_status(
        &self,
        project_id: i64,
        next: CodeProjectStatus,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current: String = transaction
            .query_row(
                "SELECT status FROM code_projects WHERE id=?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
            .context("El proyecto de codigo ya no existe")?;
        let current = CodeProjectStatus::parse(&current)
            .with_context(|| format!("Estado de proyecto de codigo no reconocido: {current}"))?;
        if !current.can_transition_to(next) {
            anyhow::bail!(
                "Transicion de estado de codigo no permitida: {} -> {}",
                current.as_str(),
                next.as_str()
            );
        }
        transaction.execute(
            "UPDATE code_projects SET status=?1,last_opened=?2 WHERE id=?3",
            params![next.as_str(), Utc::now().to_rfc3339(), project_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn mark_interrupted_code_projects(&self) -> Result<usize> {
        Ok(self.connection()?.execute(
            "UPDATE code_projects SET status='partial' WHERE status IN ('indexing','escaneando')",
            [],
        )?)
    }

    pub fn list_code_files(&self, project_id: i64) -> Result<Vec<CodeFile>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT id,project_id,relative_path,hash,extension,language,size_bytes,error FROM code_files WHERE project_id=?1 ORDER BY relative_path COLLATE NOCASE",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(CodeFile {
                id: row.get(0)?,
                project_id: row.get(1)?,
                relative_path: row.get(2)?,
                hash: row.get(3)?,
                extension: row.get(4)?,
                language: row.get(5)?,
                size_bytes: row.get::<_, i64>(6)? as u64,
                error: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn replace_code_file_index(
        &self,
        project_id: i64,
        relative_path: &str,
        hash: &str,
        extension: &str,
        language: &str,
        size_bytes: u64,
        chunks: &[CodeChunk],
    ) -> Result<i64> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO code_files(project_id,relative_path,hash,extension,language,size_bytes,error,indexed_at) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7) ON CONFLICT(project_id,relative_path) DO UPDATE SET hash=excluded.hash,extension=excluded.extension,language=excluded.language,size_bytes=excluded.size_bytes,error=NULL,indexed_at=excluded.indexed_at",
            params![project_id, relative_path, hash, extension, language, size_bytes as i64, Utc::now().to_rfc3339()],
        )?;
        let file_id = transaction.query_row(
            "SELECT id FROM code_files WHERE project_id=?1 AND relative_path=?2",
            params![project_id, relative_path],
            |row| row.get(0),
        )?;
        transaction.execute("DELETE FROM code_chunks WHERE file_id=?1", [file_id])?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO code_chunks(file_id,chunk_index,line_start,line_end,content,embedding) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for chunk in chunks {
                statement.execute(params![
                    file_id,
                    chunk.chunk_index as i64,
                    chunk.line_start as i64,
                    chunk.line_end as i64,
                    chunk.content,
                    embedding_to_bytes(&chunk.embedding),
                ])?;
            }
        }
        transaction.commit()?;
        Ok(file_id)
    }

    pub fn delete_missing_code_files(
        &self,
        project_id: i64,
        report: &ScanReport,
        confirmed_missing: &[String],
    ) -> Result<()> {
        if !report.is_complete() {
            anyhow::bail!(
                "Kuznor no puede eliminar archivos del indice tras un scan parcial o fallido"
            );
        }
        for relative_path in confirmed_missing {
            if report
                .files
                .iter()
                .any(|file| file.relative_path == *relative_path)
            {
                anyhow::bail!(
                    "Kuznor no puede eliminar un archivo observado durante el mismo scan"
                );
            }
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for relative_path in confirmed_missing {
            transaction.execute(
                "DELETE FROM code_files WHERE project_id=?1 AND relative_path=?2",
                params![project_id, relative_path],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn code_chunks(&self, project_id: i64) -> Result<Vec<CodeChunk>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT c.id,f.project_id,f.id,f.relative_path,f.extension,f.language,c.chunk_index,c.line_start,c.line_end,c.content,c.embedding FROM code_chunks c JOIN code_files f ON f.id=c.file_id JOIN code_projects p ON p.id=f.project_id WHERE f.project_id=?1 AND f.error IS NULL AND p.status IN ('ready','listo') ORDER BY f.relative_path,c.chunk_index",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Vec<u8>>(10)?,
            ))
        })?;
        let mut chunks = Vec::new();
        for row in rows {
            let (
                id,
                project_id,
                file_id,
                relative_path,
                extension,
                language,
                chunk_index,
                line_start,
                line_end,
                content,
                bytes,
            ) = row?;
            let embedding = bytes_to_embedding(&bytes).with_context(|| {
                format!("Embedding invalido en el chunk de codigo {id} del archivo {file_id}")
            })?;
            chunks.push(CodeChunk {
                id,
                project_id,
                file_id,
                relative_path,
                extension,
                language,
                chunk_index: chunk_index as usize,
                line_start: line_start as usize,
                line_end: line_end as usize,
                content,
                embedding,
            });
        }
        Ok(chunks)
    }
}

fn validate_schema(connection: &Connection, require_indexes: bool) -> Result<()> {
    for (table, columns) in [
        ("settings", &["key", "value"][..]),
        ("libraries", &["id", "name", "created_at"][..]),
        (
            "chats",
            &[
                "id",
                "title",
                "profile",
                "library_id",
                "created_at",
                "updated_at",
            ][..],
        ),
        (
            "messages",
            &[
                "id",
                "chat_id",
                "role",
                "content",
                "sources_json",
                "created_at",
            ][..],
        ),
        (
            "documents",
            &[
                "id",
                "library_id",
                "name",
                "original_path",
                "hash",
                "file_type",
                "status",
                "error",
                "created_at",
            ][..],
        ),
        (
            "document_chunks",
            &[
                "id",
                "document_id",
                "chunk_index",
                "content",
                "page_number",
                "section",
                "embedding",
            ][..],
        ),
        (
            "code_projects",
            &["id", "name", "root_path", "status", "last_opened"][..],
        ),
        (
            "code_files",
            &[
                "id",
                "project_id",
                "relative_path",
                "hash",
                "extension",
                "language",
                "size_bytes",
                "error",
                "indexed_at",
            ][..],
        ),
        (
            "code_chunks",
            &[
                "id",
                "file_id",
                "chunk_index",
                "line_start",
                "line_end",
                "content",
                "embedding",
            ][..],
        ),
    ] {
        validate_table_columns(connection, table, columns)?;
    }

    for (table, column) in [
        ("settings", "key"),
        ("libraries", "id"),
        ("chats", "id"),
        ("messages", "id"),
        ("documents", "id"),
        ("document_chunks", "id"),
        ("code_projects", "id"),
        ("code_files", "id"),
        ("code_chunks", "id"),
    ] {
        validate_primary_key(connection, table, column)?;
    }

    for (table, columns) in [
        ("libraries", &["name"][..]),
        ("documents", &["library_id", "hash"][..]),
        ("document_chunks", &["document_id", "chunk_index"][..]),
        ("code_projects", &["root_path"][..]),
        ("code_files", &["project_id", "relative_path"][..]),
        ("code_chunks", &["file_id", "chunk_index"][..]),
    ] {
        validate_unique_index(connection, table, columns)?;
    }

    for (table, column, parent_table, parent_column, on_delete) in [
        ("chats", "library_id", "libraries", "id", "SET NULL"),
        ("messages", "chat_id", "chats", "id", "CASCADE"),
        ("documents", "library_id", "libraries", "id", "CASCADE"),
        (
            "document_chunks",
            "document_id",
            "documents",
            "id",
            "CASCADE",
        ),
        ("code_files", "project_id", "code_projects", "id", "CASCADE"),
        ("code_chunks", "file_id", "code_files", "id", "CASCADE"),
    ] {
        validate_foreign_key(
            connection,
            table,
            column,
            parent_table,
            parent_column,
            on_delete,
        )?;
    }

    if require_indexes {
        for (index, table, columns) in [
            ("idx_messages_chat", "messages", &["chat_id", "id"][..]),
            ("idx_documents_library", "documents", &["library_id"][..]),
            (
                "idx_chunks_document",
                "document_chunks",
                &["document_id"][..],
            ),
            ("idx_code_files_project", "code_files", &["project_id"][..]),
            ("idx_code_chunks_file", "code_chunks", &["file_id"][..]),
        ] {
            validate_named_index(connection, index, table, columns)?;
        }
    }
    Ok(())
}

fn validate_table_columns(
    connection: &Connection,
    table: &str,
    expected_columns: &[&str],
) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?;
    if !exists {
        anyhow::bail!("Falta la tabla requerida {table}");
    }
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for expected in expected_columns {
        if !columns.iter().any(|column| column == expected) {
            anyhow::bail!("Falta la columna requerida {table}.{expected}");
        }
    }
    Ok(())
}

fn validate_primary_key(connection: &Connection, table: &str, expected_column: &str) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let primary_key_columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)?))
        })?
        .filter_map(|row| match row {
            Ok((column, position)) if position > 0 => Some(Ok((column, position))),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if primary_key_columns != [(expected_column.to_owned(), 1)] {
        anyhow::bail!("La clave primaria de {table} no coincide con el esquema esperado");
    }
    Ok(())
}

fn validate_unique_index(
    connection: &Connection,
    table: &str,
    expected_columns: &[&str],
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA index_list({table})"))?;
    let indexes = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (index, is_unique) in indexes {
        if is_unique == 0 {
            continue;
        }
        if index_columns(connection, &index)? == expected_columns {
            return Ok(());
        }
    }
    anyhow::bail!(
        "Falta la restriccion UNIQUE requerida en {}({})",
        table,
        expected_columns.join(", ")
    )
}

fn validate_foreign_key(
    connection: &Connection,
    table: &str,
    column: &str,
    parent_table: &str,
    parent_column: &str,
    on_delete: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let keys = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if keys.iter().any(|(parent, from, to, delete_action)| {
        parent == parent_table
            && from == column
            && to == parent_column
            && delete_action.eq_ignore_ascii_case(on_delete)
    }) {
        Ok(())
    } else {
        anyhow::bail!(
            "Falta la relacion requerida {table}.{column} -> {parent_table}.{parent_column} ON DELETE {on_delete}"
        )
    }
}

fn validate_named_index(
    connection: &Connection,
    index: &str,
    table: &str,
    expected_columns: &[&str],
) -> Result<()> {
    let indexed_table: Option<String> = connection
        .query_row(
            "SELECT tbl_name FROM sqlite_schema WHERE type='index' AND name=?1",
            [index],
            |row| row.get(0),
        )
        .optional()?;
    if indexed_table.as_deref() != Some(table)
        || index_columns(connection, index)? != expected_columns
    {
        anyhow::bail!(
            "Falta o es incompatible el indice requerido {index} sobre {}({})",
            table,
            expected_columns.join(", ")
        );
    }
    Ok(())
}

fn index_columns(connection: &Connection, index: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA index_info({index})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(2))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn document_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    Ok(Document {
        id: r.get(0)?,
        library_id: r.get(1)?,
        name: r.get(2)?,
        original_path: r.get(3)?,
        hash: r.get(4)?,
        file_type: r.get(5)?,
        status: r.get(6)?,
        created_at: r.get(7)?,
        error: r.get(8)?,
    })
}

pub fn embedding_to_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}
pub fn bytes_to_embedding(bytes: &[u8]) -> Result<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        anyhow::bail!("Embedding serializado invalido");
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_version(path: &Path) -> i32 {
        Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    }

    fn create_legacy_v0(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection.execute_batch(CREATE_TABLES_V1).unwrap();
        connection.execute_batch("PRAGMA user_version=0;").unwrap();
        connection
    }

    fn schema_object_exists(path: &Path, kind: &str, name: &str) -> bool {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
                [kind, name],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn scan_report(paths: &[&str], complete: bool) -> ScanReport {
        let mut report = ScanReport::complete();
        report.files = paths
            .iter()
            .map(|relative_path| crate::code::types::ScannedCodeFile {
                absolute_path: PathBuf::from(relative_path),
                relative_path: (*relative_path).into(),
                extension: "rs".into(),
                language: "rust".into(),
                size_bytes: 1,
            })
            .collect();
        if !complete {
            report.mark_partial(crate::code::types::ScanPartialReason::FileLimit);
        }
        report
    }

    fn code_chunk(project_id: i64, content: &str) -> CodeChunk {
        CodeChunk {
            id: 0,
            project_id,
            file_id: 0,
            relative_path: "main.rs".into(),
            extension: "rs".into(),
            language: "rust".into(),
            chunk_index: 0,
            line_start: 1,
            line_end: 1,
            content: content.into(),
            embedding: vec![1.0, 0.0],
        }
    }

    #[test]
    fn complete_scan_prunes_only_the_deleted_file() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("complete-prune.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/a").unwrap();
        database
            .replace_code_file_index(
                project,
                "kept.rs",
                "kept",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "kept")],
            )
            .unwrap();
        database
            .replace_code_file_index(
                project,
                "deleted.rs",
                "deleted",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "deleted")],
            )
            .unwrap();

        database
            .delete_missing_code_files(
                project,
                &scan_report(&["kept.rs"], true),
                &["deleted.rs".into()],
            )
            .unwrap();

        assert_eq!(
            database
                .list_code_files(project)
                .unwrap()
                .iter()
                .map(|file| file.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["kept.rs"]
        );
    }

    #[test]
    fn partial_scan_cannot_prune_existing_files_or_other_projects() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("partial-prune.db")).unwrap();
        let first = database.upsert_code_project("A", "C:/a").unwrap();
        let second = database.upsert_code_project("B", "C:/b").unwrap();
        for (project, path) in [(first, "a.rs"), (second, "b.rs")] {
            database
                .replace_code_file_index(
                    project,
                    path,
                    path,
                    "rs",
                    "rust",
                    1,
                    &[code_chunk(project, path)],
                )
                .unwrap();
        }
        database
            .replace_code_file_index(
                first,
                "unobserved.rs",
                "old-unobserved",
                "rs",
                "rust",
                1,
                &[code_chunk(first, "unobserved")],
            )
            .unwrap();
        database
            .replace_code_file_index(
                first,
                "a.rs",
                "new-a",
                "rs",
                "rust",
                1,
                &[code_chunk(first, "updated")],
            )
            .unwrap();
        database
            .replace_code_file_index(
                first,
                "new.rs",
                "new-file",
                "rs",
                "rust",
                1,
                &[code_chunk(first, "new")],
            )
            .unwrap();

        let error = database
            .delete_missing_code_files(
                first,
                &scan_report(&["a.rs", "new.rs"], false),
                &["unobserved.rs".into()],
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("scan parcial"));
        let first_files = database.list_code_files(first).unwrap();
        assert_eq!(first_files.len(), 3);
        assert!(
            first_files
                .iter()
                .any(|file| file.relative_path == "unobserved.rs")
        );
        assert!(
            first_files
                .iter()
                .any(|file| file.relative_path == "new.rs")
        );
        assert_eq!(
            first_files
                .iter()
                .find(|file| file.relative_path == "a.rs")
                .unwrap()
                .hash,
            "new-a"
        );
        assert_eq!(database.list_code_files(second).unwrap().len(), 1);
    }

    #[test]
    fn complete_scan_cannot_prune_an_observed_file_even_if_the_caller_requests_it() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("observed-prune.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/a").unwrap();
        database
            .replace_code_file_index(
                project,
                "main.rs",
                "hash",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "main")],
            )
            .unwrap();

        let error = database
            .delete_missing_code_files(
                project,
                &scan_report(&["main.rs"], true),
                &["main.rs".into()],
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("observado"));
        assert_eq!(database.list_code_files(project).unwrap().len(), 1);
    }

    #[test]
    fn failed_replacement_rolls_back_and_preserves_the_previous_file_index() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("atomic-replace.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/a").unwrap();
        database
            .replace_code_file_index(
                project,
                "main.rs",
                "old-hash",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "old")],
            )
            .unwrap();
        let mut duplicate = code_chunk(project, "duplicate");
        duplicate.chunk_index = 0;

        assert!(
            database
                .replace_code_file_index(
                    project,
                    "main.rs",
                    "new-hash",
                    "rs",
                    "rust",
                    1,
                    &[code_chunk(project, "new"), duplicate],
                )
                .is_err()
        );

        assert_eq!(
            database.list_code_files(project).unwrap()[0].hash,
            "old-hash"
        );
        database
            .transition_code_project_status(project, CodeProjectStatus::Ready)
            .unwrap();
        assert_eq!(database.code_chunks(project).unwrap()[0].content, "old");
    }

    #[test]
    fn failed_project_keeps_the_previous_index_records() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("failed-project.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/a").unwrap();
        database
            .replace_code_file_index(
                project,
                "main.rs",
                "old-hash",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "old")],
            )
            .unwrap();

        database
            .transition_code_project_status(project, CodeProjectStatus::Failed)
            .unwrap();

        assert_eq!(
            database.list_code_files(project).unwrap()[0].hash,
            "old-hash"
        );
        let chunk_count: i64 = database
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM code_chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(chunk_count, 1);
        assert_eq!(
            database.list_code_projects().unwrap()[0].status,
            crate::code::types::PROJECT_FAILED
        );
        assert!(
            database
                .transition_code_project_status(project, CodeProjectStatus::Ready)
                .is_err()
        );
    }

    #[test]
    fn unavailable_refresh_marks_ready_project_failed_without_erasing_its_index() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("missing-root.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/missing").unwrap();
        database
            .replace_code_file_index(
                project,
                "main.rs",
                "old-hash",
                "rs",
                "rust",
                1,
                &[code_chunk(project, "old")],
            )
            .unwrap();
        database
            .transition_code_project_status(project, CodeProjectStatus::Ready)
            .unwrap();

        database
            .transition_code_project_status(project, CodeProjectStatus::Failed)
            .unwrap();

        assert_eq!(
            database.list_code_files(project).unwrap()[0].hash,
            "old-hash"
        );
        assert_eq!(
            database.list_code_projects().unwrap()[0].status,
            crate::code::types::PROJECT_FAILED
        );
        assert!(database.code_chunks(project).unwrap().is_empty());
    }

    #[test]
    fn project_statuses_preserve_complete_partial_and_failed_outcomes() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let database = Database::open(directory.path().join("project-statuses.db")).unwrap();
        let project = database.upsert_code_project("A", "C:/a").unwrap();

        database
            .transition_code_project_status(project, CodeProjectStatus::Ready)
            .unwrap();
        assert_eq!(
            database.list_code_projects().unwrap()[0].status,
            crate::code::types::PROJECT_READY
        );
        assert!(
            database
                .transition_code_project_status(project, CodeProjectStatus::Partial)
                .is_ok()
        );
        assert_eq!(
            database.list_code_projects().unwrap()[0].status,
            crate::code::types::PROJECT_PARTIAL
        );
        assert!(
            database
                .transition_code_project_status(project, CodeProjectStatus::Ready)
                .is_err()
        );
        database
            .transition_code_project_status(project, CodeProjectStatus::Failed)
            .unwrap();
        assert_eq!(
            database.list_code_projects().unwrap()[0].status,
            crate::code::types::PROJECT_FAILED
        );
    }

    #[test]
    fn new_database_creates_schema_version_one() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("new.db");

        Database::open(&path).unwrap();

        assert_eq!(schema_version(&path), CURRENT_SCHEMA_VERSION);
        assert!(schema_object_exists(&path, "index", "idx_messages_chat"));
    }

    #[test]
    fn legacy_v0_migration_preserves_chat_document_and_code_data() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("legacy.db");
        let connection = create_legacy_v0(&path);
        connection
            .execute_batch(
                "
                INSERT INTO libraries(id,name,created_at) VALUES(1,'Trabajo','2026-01-01T00:00:00Z');
                INSERT INTO chats(id,title,profile,library_id,created_at,updated_at)
                    VALUES(1,'Conversacion','general',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');
                INSERT INTO messages(id,chat_id,role,content,sources_json,created_at)
                    VALUES(1,1,'user','hola','[]','2026-01-01T00:00:00Z');
                INSERT INTO documents(id,library_id,name,original_path,hash,file_type,status,created_at)
                    VALUES(1,1,'manual.txt','C:/manual.txt','hash','txt','listo','2026-01-01T00:00:00Z');
                INSERT INTO document_chunks(id,document_id,chunk_index,content,embedding)
                    VALUES(1,1,0,'contenido',X'0000803F');
                INSERT INTO code_projects(id,name,root_path,status,last_opened)
                    VALUES(1,'Proyecto','C:/proyecto','listo','2026-01-01T00:00:00Z');
                INSERT INTO code_files(id,project_id,relative_path,hash,extension,language,size_bytes,indexed_at)
                    VALUES(1,1,'src/main.rs','hash','rs','rust',10,'2026-01-01T00:00:00Z');
                INSERT INTO code_chunks(id,file_id,chunk_index,line_start,line_end,content,embedding)
                    VALUES(1,1,0,1,1,'fn main() {}',X'0000803F');
                ",
            )
            .unwrap();
        drop(connection);

        let database = Database::open(&path).unwrap();

        assert_eq!(schema_version(&path), CURRENT_SCHEMA_VERSION);
        assert_eq!(database.messages(1).unwrap()[0].content, "hola");
        assert_eq!(database.list_libraries().unwrap()[0].name, "Trabajo");
        assert_eq!(database.library_chunks(1).unwrap()[0].content, "contenido");
        assert_eq!(database.code_chunks(1).unwrap()[0].content, "fn main() {}");
    }

    #[test]
    fn legacy_v0_migration_creates_missing_safe_indexes() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("legacy-indexes.db");
        drop(create_legacy_v0(&path));

        Database::open(&path).unwrap();

        assert_eq!(schema_version(&path), CURRENT_SCHEMA_VERSION);
        for index in [
            "idx_messages_chat",
            "idx_documents_library",
            "idx_chunks_document",
            "idx_code_files_project",
            "idx_code_chunks_file",
        ] {
            assert!(schema_object_exists(&path, "index", index));
        }
    }

    #[test]
    fn opening_current_schema_is_idempotent() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("current.db");
        let database = Database::open(&path).unwrap();
        let chat_id = database
            .create_chat("Persistente", Profile::General)
            .unwrap();
        drop(database);

        let reopened = Database::open(&path).unwrap();

        assert_eq!(schema_version(&path), CURRENT_SCHEMA_VERSION);
        assert_eq!(reopened.list_chats().unwrap()[0].id, chat_id);
    }

    #[test]
    fn future_schema_is_rejected_without_modifying_it() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("future.db");
        Database::open(&path).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version=2;")
            .unwrap();

        let error = format!("{:#}", Database::open(&path).unwrap_err());

        assert!(error.contains("mas reciente"));
        assert_eq!(schema_version(&path), 2);
        assert!(schema_object_exists(&path, "index", "idx_messages_chat"));
    }

    #[test]
    fn partial_legacy_database_is_rejected_without_schema_repair() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("partial.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
            .unwrap();
        drop(connection);

        let error = format!("{:#}", Database::open(&path).unwrap_err());

        assert!(error.contains("esquema legacy v0.1"));
        assert!(!schema_object_exists(&path, "table", "libraries"));
        assert_eq!(schema_version(&path), 0);
    }

    #[test]
    fn legacy_database_missing_required_column_is_rejected() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("missing-column.db");
        let connection = create_legacy_v0(&path);
        connection
            .execute_batch("PRAGMA foreign_keys=OFF;")
            .unwrap();
        connection
            .execute_batch("ALTER TABLE messages DROP COLUMN sources_json;")
            .unwrap();
        drop(connection);

        let error = format!("{:#}", Database::open(&path).unwrap_err());

        assert!(error.contains("messages.sources_json"));
        assert_eq!(schema_version(&path), 0);
    }

    #[test]
    fn failed_migration_rolls_back_and_can_be_retried() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("rollback.db");
        let connection = create_legacy_v0(&path);
        connection
            .execute_batch("CREATE TABLE idx_documents_library (value TEXT);")
            .unwrap();
        drop(connection);

        assert!(Database::open(&path).is_err());
        assert_eq!(schema_version(&path), 0);
        assert!(!schema_object_exists(&path, "index", "idx_messages_chat"));

        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE idx_documents_library;")
            .unwrap();
        Database::open(&path).unwrap();
        assert_eq!(schema_version(&path), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_database_is_rejected_without_modifying_bytes() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("corrupt.db");
        let bytes = b"esto no es una base sqlite";
        std::fs::write(&path, bytes).unwrap();

        assert!(Database::open(&path).is_err());

        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn invalid_foreign_keys_block_legacy_migration_without_version_change() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("invalid-fk.db");
        let connection = create_legacy_v0(&path);
        connection
            .execute_batch("PRAGMA foreign_keys=OFF;")
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO chats(id,title,profile,library_id,created_at,updated_at)
                 VALUES(1,'Huerfano','general',999,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        drop(connection);

        let error = Database::open(&path).unwrap_err().to_string();

        assert!(error.contains("relacion invalida"));
        assert_eq!(schema_version(&path), 0);
        assert!(!schema_object_exists(&path, "index", "idx_messages_chat"));
    }

    #[test]
    fn invalid_sources_json_returns_contextual_error() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("bad-sources.db");
        let database = Database::open(&path).unwrap();
        let chat_id = database.create_chat("Chat", Profile::General).unwrap();
        database.add_message(chat_id, "user", "hola", &[]).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE messages SET sources_json='{invalido' WHERE chat_id=?1",
                [chat_id],
            )
            .unwrap();

        let error = database.messages(chat_id).unwrap_err().to_string();

        assert!(error.contains("sources_json invalido"));
        assert!(error.contains(&format!("chat {chat_id}")));
    }

    #[test]
    fn invalid_document_embedding_returns_contextual_error() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("bad-document-embedding.db");
        let database = Database::open(&path).unwrap();
        let library = database.create_library("Biblioteca").unwrap();
        let document = database
            .create_document(library, "manual.txt", "manual.txt", "hash", "txt")
            .unwrap();
        database.save_chunks(document, &[]).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO document_chunks(document_id,chunk_index,content,embedding) VALUES(?1,0,'dato',X'01')",
                [document],
            )
            .unwrap();

        let error = database.library_chunks(library).unwrap_err().to_string();

        assert!(error.contains("Embedding invalido"));
        assert!(error.contains(&format!("documento {document}")));
    }

    #[test]
    fn invalid_code_embedding_returns_contextual_error() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("bad-code-embedding.db");
        let database = Database::open(&path).unwrap();
        let project = database
            .upsert_code_project("Proyecto", "C:/proyecto")
            .unwrap();
        let file = database
            .replace_code_file_index(project, "main.rs", "hash", "rs", "rust", 1, &[])
            .unwrap();
        database
            .transition_code_project_status(project, CodeProjectStatus::Ready)
            .unwrap();
        Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO code_chunks(file_id,chunk_index,line_start,line_end,content,embedding) VALUES(?1,0,1,1,'dato',X'01')",
                [file],
            )
            .unwrap();

        let error = database.code_chunks(project).unwrap_err().to_string();

        assert!(error.contains("Embedding invalido"));
        assert!(error.contains(&format!("archivo {file}")));
    }

    #[test]
    fn sqlite_and_messages_persist() {
        let d = tempfile::tempdir_in("target").unwrap();
        let path = d.path().join("a.db");
        let db = Database::open(&path).unwrap();
        let id = db.create_chat("Uno", Profile::General).unwrap();
        db.add_message(id, "user", "hola", &[]).unwrap();
        drop(db);
        let reopened = Database::open(path).unwrap();
        assert_eq!(reopened.messages(id).unwrap()[0].content, "hola");
    }

    #[test]
    fn chats_are_isolated_by_persisted_profile() {
        let dir = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(dir.path().join("modes.db")).unwrap();
        let general = db.create_chat("General", Profile::General).unwrap();
        let documents = db
            .create_chat("Documentos", Profile::Documentation)
            .unwrap();
        let code = db.create_chat("Codigo", Profile::Code).unwrap();
        assert_eq!(
            db.list_chats_for_profile(Profile::General)
                .unwrap()
                .iter()
                .map(|chat| chat.id)
                .collect::<Vec<_>>(),
            [general]
        );
        assert_eq!(
            db.list_chats_for_profile(Profile::Documentation)
                .unwrap()
                .iter()
                .map(|chat| chat.id)
                .collect::<Vec<_>>(),
            [documents]
        );
        assert_eq!(
            db.list_chats_for_profile(Profile::Code)
                .unwrap()
                .iter()
                .map(|chat| chat.id)
                .collect::<Vec<_>>(),
            [code]
        );
    }

    #[test]
    fn chats_and_libraries_have_independent_lifecycles() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(directory.path().join("independent-entities.db")).unwrap();

        let library_id = db.create_library("Trabajo").unwrap();
        assert!(db.list_chats().unwrap().is_empty());

        let first_chat = db
            .create_chat("Primera consulta", Profile::Documentation)
            .unwrap();
        let second_chat = db
            .create_chat("Segunda consulta", Profile::Documentation)
            .unwrap();
        assert_eq!(db.list_libraries().unwrap().len(), 1);
        assert!(
            db.list_chats()
                .unwrap()
                .iter()
                .all(|chat| chat.library_id.is_none())
        );

        // The legacy context column permits many chats to reference one library; it is not ownership.
        db.update_chat(
            first_chat,
            "Primera consulta",
            Profile::Documentation,
            Some(library_id),
        )
        .unwrap();
        db.update_chat(
            second_chat,
            "Segunda consulta",
            Profile::Documentation,
            Some(library_id),
        )
        .unwrap();

        db.delete_chat(first_chat).unwrap();
        assert_eq!(db.list_libraries().unwrap()[0].id, library_id);

        db.delete_library(library_id).unwrap();
        let remaining_chats = db.list_chats().unwrap();
        assert_eq!(remaining_chats.len(), 1);
        assert_eq!(remaining_chats[0].id, second_chat);
        assert_eq!(remaining_chats[0].library_id, None);
    }
    #[test]
    fn embedding_serialization_round_trip() {
        let v = vec![1.0, -2.5, 0.25];
        assert_eq!(bytes_to_embedding(&embedding_to_bytes(&v)).unwrap(), v);
    }
    #[test]
    fn old_sources_without_preview_remain_compatible() {
        let sources: Vec<Source> = serde_json::from_str(
            r#"[{"document_name":"manual.md","chunk_index":2,"page_number":null}]"#,
        )
        .unwrap();
        assert!(sources[0].preview.is_empty());
        assert_eq!(sources[0].library_id, 0);
    }

    #[test]
    fn library_names_are_trimmed_and_empty_names_are_rejected() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let path = directory.path().join("library-names.db");
        let db = Database::open(&path).unwrap();
        assert!(db.create_library("").is_err());
        assert!(db.create_library("  \t\n ").is_err());
        let id = db.create_library("  Tumama  ").unwrap();
        let document_id = db
            .create_document(id, "manual.txt", "manual.txt", "hash", "txt")
            .unwrap();
        db.save_chunks(
            document_id,
            &[Chunk {
                id: 0,
                document_id,
                document_name: "manual.txt".into(),
                chunk_index: 0,
                content: "Contenido persistente".into(),
                page_number: None,
                section: None,
                embedding: vec![1.0, 0.0],
            }],
        )
        .unwrap();
        assert_eq!(db.list_libraries().unwrap()[0].name, "Tumama");
        assert!(db.rename_library(id, "   ").is_err());
        db.rename_library(id, "  Trabajo ").unwrap();
        let duplicate = db.create_library("Otra").unwrap();
        assert!(db.rename_library(duplicate, "Trabajo").is_err());
        drop(db);

        let reopened = Database::open(path).unwrap();
        let renamed = reopened
            .list_libraries()
            .unwrap()
            .into_iter()
            .find(|library| library.id == id)
            .unwrap();
        assert_eq!(renamed.name, "Trabajo");
        assert_eq!(reopened.list_documents(id).unwrap()[0].id, document_id);
        assert_eq!(reopened.library_chunks(id).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_library_removes_only_internal_records() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let original = directory.path().join("original.txt");
        std::fs::write(&original, "contenido del usuario").unwrap();
        let db = Database::open(directory.path().join("library-delete.db")).unwrap();
        let library_id = db.create_library("Temporal").unwrap();
        db.create_document(
            library_id,
            "original.txt",
            &original.to_string_lossy(),
            "hash",
            "txt",
        )
        .unwrap();

        db.delete_library(library_id).unwrap();

        assert!(db.list_libraries().unwrap().is_empty());
        assert!(original.exists());
    }

    #[test]
    fn library_chunks_do_not_mix_libraries() {
        let d = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(d.path().join("libraries.db")).unwrap();
        let first = db.create_library("Primera").unwrap();
        let second = db.create_library("Segunda").unwrap();
        let first_document = db
            .create_document(first, "primera.txt", "primera.txt", "hash-1", "txt")
            .unwrap();
        let second_document = db
            .create_document(second, "segunda.txt", "segunda.txt", "hash-2", "txt")
            .unwrap();
        let make_chunk = |document_id: i64, name: &str| Chunk {
            id: 0,
            document_id,
            document_name: name.into(),
            chunk_index: 0,
            content: name.into(),
            page_number: Some(1),
            section: None,
            embedding: vec![1.0, 0.0],
        };
        db.save_chunks(first_document, &[make_chunk(first_document, "primera.txt")])
            .unwrap();
        db.save_chunks(
            second_document,
            &[make_chunk(second_document, "segunda.txt")],
        )
        .unwrap();

        let chunks = db.library_chunks(first).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].document_name, "primera.txt");
    }

    #[test]
    fn switching_a_document_chat_from_sql_to_travel_cannot_retrieve_sql() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(directory.path().join("library-switch.db")).unwrap();
        let sql_library = db.create_library("SQL").unwrap();
        let travel_library = db.create_library("Viajes").unwrap();
        let sql_document = db
            .create_document(sql_library, "manual.pdf", "sql.pdf", "sql-hash", "pdf")
            .unwrap();
        let travel_document = db
            .create_document(
                travel_library,
                "manual.pdf",
                "travel.pdf",
                "travel-hash",
                "pdf",
            )
            .unwrap();
        let chunk = |document_id, content: &str, embedding| Chunk {
            id: 0,
            document_id,
            document_name: "manual.pdf".into(),
            chunk_index: 0,
            content: content.into(),
            page_number: Some(1),
            section: None,
            embedding,
        };
        db.save_chunks(
            sql_document,
            &[chunk(
                sql_document,
                "primary keys duplicates SELECT WHERE",
                vec![1.0, 0.0],
            )],
        )
        .unwrap();
        db.save_chunks(
            travel_document,
            &[chunk(
                travel_document,
                "destinos turismo sitio web WhatsApp",
                vec![0.0, 1.0],
            )],
        )
        .unwrap();
        let chat = db.create_chat("Consulta", Profile::Documentation).unwrap();
        db.update_chat(chat, "Consulta", Profile::Documentation, Some(sql_library))
            .unwrap();
        db.update_chat(
            chat,
            "Consulta",
            Profile::Documentation,
            Some(travel_library),
        )
        .unwrap();

        let current_chunks = db.library_chunks(travel_library).unwrap();
        let found = crate::rag::search::retrieve(
            current_chunks.clone(),
            &[1.0, 0.0],
            "llaves primarias y SQL",
            4,
        );

        assert!(found.is_empty());
        assert!(
            current_chunks
                .iter()
                .all(|chunk| chunk.document_id == travel_document)
        );
        let persisted = db.list_chats_for_profile(Profile::Documentation).unwrap();
        assert_eq!(persisted[0].library_id, Some(travel_library));

        db.update_chat(chat, "Consulta", Profile::Documentation, Some(sql_library))
            .unwrap();
        let sql_again = crate::rag::search::retrieve(
            db.library_chunks(sql_library).unwrap(),
            &[1.0, 0.0],
            "llaves primarias y SQL",
            4,
        );
        assert_eq!(sql_again.len(), 1);
        assert_eq!(sql_again[0].chunk.document_id, sql_document);
    }

    #[test]
    fn code_projects_persist_and_keep_chunks_isolated() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(directory.path().join("code.db")).unwrap();
        let first = db.upsert_code_project("Uno", "C:/projects/uno").unwrap();
        let second = db.upsert_code_project("Dos", "C:/projects/dos").unwrap();
        assert_eq!(
            db.upsert_code_project("Uno", "C:/projects/uno").unwrap(),
            first
        );
        let make_chunk = |project_id, file_id, path: &str| CodeChunk {
            id: 0,
            project_id,
            file_id,
            relative_path: path.into(),
            extension: "txt".into(),
            language: "text".into(),
            chunk_index: 0,
            line_start: 1,
            line_end: 2,
            content: path.into(),
            embedding: vec![1.0, 0.0],
        };
        let _first_file = db
            .replace_code_file_index(
                first,
                "src/main.rs",
                "hash-1",
                "rs",
                "rust",
                10,
                &[make_chunk(first, 0, "src/main.rs")],
            )
            .unwrap();
        let _second_file = db
            .replace_code_file_index(
                second,
                "app.py",
                "hash-2",
                "py",
                "python",
                10,
                &[make_chunk(second, 0, "app.py")],
            )
            .unwrap();
        db.transition_code_project_status(first, CodeProjectStatus::Ready)
            .unwrap();
        db.transition_code_project_status(second, CodeProjectStatus::Ready)
            .unwrap();

        let first_chunks = db.code_chunks(first).unwrap();
        assert_eq!(first_chunks.len(), 1);
        assert_eq!(first_chunks[0].relative_path, "src/main.rs");
        assert!(first_chunks.iter().all(|chunk| chunk.project_id == first));
        assert_eq!(db.list_code_projects().unwrap().len(), 2);
    }

    #[test]
    fn interrupted_or_cancelled_code_projects_are_never_searchable_as_ready() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let db = Database::open(directory.path().join("partial-code.db")).unwrap();
        let project = db
            .upsert_code_project("Parcial", "C:/projects/parcial")
            .unwrap();
        let _file = db
            .replace_code_file_index(
                project,
                "src/main.rs",
                "hash",
                "rs",
                "rust",
                10,
                &[CodeChunk {
                    id: 0,
                    project_id: project,
                    file_id: 0,
                    relative_path: "src/main.rs".into(),
                    extension: "rs".into(),
                    language: "rust".into(),
                    chunk_index: 0,
                    line_start: 1,
                    line_end: 1,
                    content: "fn main() {}".into(),
                    embedding: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        assert_eq!(db.mark_interrupted_code_projects().unwrap(), 1);
        assert!(db.code_chunks(project).unwrap().is_empty());
        assert!(
            db.transition_code_project_status(project, CodeProjectStatus::Ready)
                .is_err()
        );
        assert!(db.code_chunks(project).unwrap().is_empty());
        db.upsert_code_project("Parcial", "C:/projects/parcial")
            .unwrap();
        db.transition_code_project_status(project, CodeProjectStatus::Ready)
            .unwrap();
        assert_eq!(db.code_chunks(project).unwrap().len(), 1);
    }

    #[test]
    fn removing_code_project_deletes_only_kuznor_records() {
        let directory = tempfile::tempdir_in("target").unwrap();
        let original_project = directory.path().join("original-project");
        std::fs::create_dir(&original_project).unwrap();
        let original_file = original_project.join("main.rs");
        std::fs::write(&original_file, "fn main() {}\n").unwrap();

        let db = Database::open(directory.path().join("remove-code.db")).unwrap();
        let project = db
            .upsert_code_project("Original", &original_project.to_string_lossy())
            .unwrap();
        let _file = db
            .replace_code_file_index(
                project,
                "main.rs",
                "hash",
                "rs",
                "rust",
                13,
                &[CodeChunk {
                    id: 0,
                    project_id: project,
                    file_id: 0,
                    relative_path: "main.rs".into(),
                    extension: "rs".into(),
                    language: "rust".into(),
                    chunk_index: 0,
                    line_start: 1,
                    line_end: 1,
                    content: "fn main() {}".into(),
                    embedding: vec![1.0],
                }],
            )
            .unwrap();

        db.remove_code_project(project).unwrap();

        assert!(db.list_code_projects().unwrap().is_empty());
        assert!(db.list_code_files(project).unwrap().is_empty());
        assert!(db.code_chunks(project).unwrap().is_empty());
        assert!(original_file.exists());
        assert_eq!(
            std::fs::read_to_string(original_file).unwrap(),
            "fn main() {}\n"
        );
    }
}
