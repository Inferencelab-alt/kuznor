use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    code::types::{CodeChunk, CodeFile, CodeProject},
    models::{Chat, Chunk, Document, Library, Message, Profile, Source},
};

fn valid_library_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("El nombre de la biblioteca no puede estar vacio");
    }
    Ok(name)
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
        db.migrate()?;
        Ok(db)
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("No se pudo abrir SQLite: {}", self.path.display()))?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
        Ok(connection)
    }

    fn migrate(&self) -> Result<()> {
        self.connection()?.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS libraries (
                id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chats (
                id INTEGER PRIMARY KEY, title TEXT NOT NULL, profile TEXT NOT NULL,
                library_id INTEGER REFERENCES libraries(id) ON DELETE SET NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY, chat_id INTEGER NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
                role TEXT NOT NULL, content TEXT NOT NULL, sources_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS documents (
                id INTEGER PRIMARY KEY, library_id INTEGER NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
                name TEXT NOT NULL, original_path TEXT NOT NULL, hash TEXT NOT NULL,
                file_type TEXT NOT NULL, status TEXT NOT NULL, error TEXT, created_at TEXT NOT NULL,
                UNIQUE(library_id, hash)
            );
            CREATE TABLE IF NOT EXISTS document_chunks (
                id INTEGER PRIMARY KEY, document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                chunk_index INTEGER NOT NULL, content TEXT NOT NULL, page_number INTEGER,
                section TEXT, embedding BLOB NOT NULL, UNIQUE(document_id, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, id);
            CREATE INDEX IF NOT EXISTS idx_documents_library ON documents(library_id);
            CREATE INDEX IF NOT EXISTS idx_chunks_document ON document_chunks(document_id);
            CREATE TABLE IF NOT EXISTS code_projects (
                id INTEGER PRIMARY KEY, name TEXT NOT NULL, root_path TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'listo', last_opened TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS code_files (
                id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL REFERENCES code_projects(id) ON DELETE CASCADE,
                relative_path TEXT NOT NULL, hash TEXT NOT NULL, extension TEXT NOT NULL,
                language TEXT NOT NULL, size_bytes INTEGER NOT NULL, error TEXT,
                indexed_at TEXT NOT NULL, UNIQUE(project_id, relative_path)
            );
            CREATE TABLE IF NOT EXISTS code_chunks (
                id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL REFERENCES code_files(id) ON DELETE CASCADE,
                chunk_index INTEGER NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
                content TEXT NOT NULL, embedding BLOB NOT NULL, UNIQUE(file_id, chunk_index)
            );
            CREATE INDEX IF NOT EXISTS idx_code_files_project ON code_files(project_id);
            CREATE INDEX IF NOT EXISTS idx_code_chunks_file ON code_chunks(file_id);
        "#)?;
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
            let json: String = r.get(4)?;
            Ok(Message {
                id: r.get(0)?,
                chat_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                sources: serde_json::from_str(&json).unwrap_or_default(),
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
            let bytes: Vec<u8> = r.get(7)?;
            Ok(Chunk {
                id: r.get(0)?,
                document_id: r.get(1)?,
                document_name: r.get(2)?,
                chunk_index: r.get::<_, i64>(3)? as usize,
                content: r.get(4)?,
                page_number: r.get(5)?,
                section: r.get(6)?,
                embedding: bytes_to_embedding(&bytes).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_code_project(&self, name: &str, root_path: &str) -> Result<i64> {
        let conn = self.connection()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO code_projects(name,root_path,status,last_opened) VALUES(?1,?2,'indexing',?3) ON CONFLICT(root_path) DO UPDATE SET name=excluded.name,status='indexing',last_opened=excluded.last_opened",
            params![name, root_path, now],
        )?;
        conn.query_row(
            "SELECT id FROM code_projects WHERE root_path=?1",
            [root_path],
            |row| row.get(0),
        )
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

    pub fn set_code_project_status(&self, project_id: i64, status: &str) -> Result<()> {
        self.connection()?.execute(
            "UPDATE code_projects SET status=?1,last_opened=?2 WHERE id=?3",
            params![status, Utc::now().to_rfc3339(), project_id],
        )?;
        Ok(())
    }

    pub fn mark_interrupted_code_projects(&self) -> Result<usize> {
        Ok(self.connection()?.execute(
            "UPDATE code_projects SET status='incomplete' WHERE status IN ('indexing','escaneando')",
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

    pub fn upsert_code_file(
        &self,
        project_id: i64,
        relative_path: &str,
        hash: &str,
        extension: &str,
        language: &str,
        size_bytes: u64,
        error: Option<&str>,
    ) -> Result<i64> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO code_files(project_id,relative_path,hash,extension,language,size_bytes,error,indexed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,relative_path) DO UPDATE SET hash=excluded.hash,extension=excluded.extension,language=excluded.language,size_bytes=excluded.size_bytes,error=excluded.error,indexed_at=excluded.indexed_at",
            params![project_id, relative_path, hash, extension, language, size_bytes as i64, error, Utc::now().to_rfc3339()],
        )?;
        conn.query_row(
            "SELECT id FROM code_files WHERE project_id=?1 AND relative_path=?2",
            params![project_id, relative_path],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn save_code_chunks(&self, file_id: i64, chunks: &[CodeChunk]) -> Result<()> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction()?;
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
        Ok(())
    }

    pub fn delete_missing_code_files(
        &self,
        project_id: i64,
        current_paths: &[String],
    ) -> Result<()> {
        for file in self.list_code_files(project_id)? {
            if !current_paths.iter().any(|path| path == &file.relative_path) {
                self.connection()?
                    .execute("DELETE FROM code_files WHERE id=?1", [file.id])?;
            }
        }
        Ok(())
    }

    pub fn code_chunks(&self, project_id: i64) -> Result<Vec<CodeChunk>> {
        let conn = self.connection()?;
        let mut statement = conn.prepare(
            "SELECT c.id,f.project_id,f.id,f.relative_path,f.extension,f.language,c.chunk_index,c.line_start,c.line_end,c.content,c.embedding FROM code_chunks c JOIN code_files f ON f.id=c.file_id JOIN code_projects p ON p.id=f.project_id WHERE f.project_id=?1 AND f.error IS NULL AND p.status IN ('ready','listo') ORDER BY f.relative_path,c.chunk_index",
        )?;
        let rows = statement.query_map([project_id], |row| {
            let bytes: Vec<u8> = row.get(10)?;
            Ok(CodeChunk {
                id: row.get(0)?,
                project_id: row.get(1)?,
                file_id: row.get(2)?,
                relative_path: row.get(3)?,
                extension: row.get(4)?,
                language: row.get(5)?,
                chunk_index: row.get::<_, i64>(6)? as usize,
                line_start: row.get::<_, i64>(7)? as usize,
                line_end: row.get::<_, i64>(8)? as usize,
                content: row.get(9)?,
                embedding: bytes_to_embedding(&bytes).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
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
        let first_file = db
            .upsert_code_file(first, "src/main.rs", "hash-1", "rs", "rust", 10, None)
            .unwrap();
        let second_file = db
            .upsert_code_file(second, "app.py", "hash-2", "py", "python", 10, None)
            .unwrap();
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
        db.save_code_chunks(first_file, &[make_chunk(first, first_file, "src/main.rs")])
            .unwrap();
        db.save_code_chunks(second_file, &[make_chunk(second, second_file, "app.py")])
            .unwrap();
        db.set_code_project_status(first, "ready").unwrap();
        db.set_code_project_status(second, "ready").unwrap();

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
        let file = db
            .upsert_code_file(project, "src/main.rs", "hash", "rs", "rust", 10, None)
            .unwrap();
        db.save_code_chunks(
            file,
            &[CodeChunk {
                id: 0,
                project_id: project,
                file_id: file,
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
        db.set_code_project_status(project, "cancelled").unwrap();
        assert!(db.code_chunks(project).unwrap().is_empty());
        db.set_code_project_status(project, "ready").unwrap();
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
        let file = db
            .upsert_code_file(project, "main.rs", "hash", "rs", "rust", 13, None)
            .unwrap();
        db.save_code_chunks(
            file,
            &[CodeChunk {
                id: 0,
                project_id: project,
                file_id: file,
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
