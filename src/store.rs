//! The index: one SQLite file. Each file read, and its pieces —a picture, a
//! page, a stretch of text, half a minute of sound, a frame of video, a
//! stretch of code—, each with its vector (768 halves) and, where it has
//! words, a line in the keyword index (FTS5).

use std::path::Path;

use anyhow::Result;
use half::f16;
use rusqlite::{Connection, OptionalExtension, params};

pub struct Store {
    pub db: Connection,
}

/// The pipeline that made the vectors: a change here reads everything again.
pub const VECTOR_VERSION: &str = "embeddinggemma-2 q8 llama.cpp-b11461 v1";

#[derive(Clone, Debug)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub mtime: f64,
    pub size: i64,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
pub struct Piece {
    /// name, image, page, page_image, chunk, audio, frame, lines
    pub kind: String,
    /// A page, a second, a line.
    pub loc: Option<f64>,
    pub loc_end: Option<f64>,
    /// text, image, audio; none for words only.
    pub modality: String,
    pub vec: Option<Vec<f32>>,
    /// What the keyword index holds of it.
    pub title: String,
    pub body: String,
    /// What a result shows of it.
    pub excerpt: Option<String>,
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for x in v {
        out.extend_from_slice(&f16::from_f32(*x).to_le_bytes());
    }
    out
}

pub fn from_blob(b: &[u8], out: &mut [f32]) {
    for (i, c) in b.chunks_exact(2).enumerate().take(out.len()) {
        out[i] = f16::from_le_bytes([c[0], c[1]]).to_f32();
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS files (
                 id INTEGER PRIMARY KEY,
                 path TEXT UNIQUE NOT NULL,
                 kind TEXT NOT NULL,
                 code INTEGER NOT NULL DEFAULT 0,
                 mtime REAL NOT NULL,
                 size INTEGER NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 error TEXT,
                 info TEXT,
                 indexed REAL
             );
             CREATE INDEX IF NOT EXISTS files_status ON files(status);
             CREATE TABLE IF NOT EXISTS segments (
                 id INTEGER PRIMARY KEY,
                 file INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                 kind TEXT NOT NULL,
                 loc REAL,
                 loc_end REAL,
                 modality TEXT NOT NULL DEFAULT 'none',
                 vec BLOB,
                 excerpt TEXT
             );
             CREATE INDEX IF NOT EXISTS segments_file ON segments(file);
             CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(title, body, tokenize = 'unicode61 remove_diacritics 2');",
        )?;
        let s = Store { db };
        match s.meta("vectors")? {
            Some(v) if v != VECTOR_VERSION => {
                eprintln!("sonda · the vectors were made by «{v}»: everything is read again");
                s.db.execute_batch("DELETE FROM fts; DELETE FROM segments; UPDATE files SET status = 'pending';")?;
                s.set_meta("vectors", VECTOR_VERSION)?;
            }
            None => s.set_meta("vectors", VECTOR_VERSION)?,
            _ => {}
        }
        Ok(s)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [key, value])?;
        Ok(())
    }

    pub fn file(&self, path: &str) -> Result<Option<FileRow>> {
        Ok(self
            .db
            .query_row("SELECT id, path, kind, mtime, size, status FROM files WHERE path = ?", [path], |r| {
                Ok(FileRow { id: r.get(0)?, path: r.get(1)?, kind: r.get(2)?, mtime: r.get(3)?, size: r.get(4)?, status: r.get(5)? })
            })
            .optional()?)
    }

    /// Known as it is now, or (again) waiting to be read.
    pub fn upsert(&self, path: &str, kind: &str, code: bool, mtime: f64, size: i64) -> Result<bool> {
        if let Some(f) = self.file(path)? {
            if (f.mtime - mtime).abs() < 0.001 && f.size == size && f.kind == kind {
                return Ok(false);
            }
            self.clear_pieces(f.id)?;
            self.db.execute("UPDATE files SET kind = ?, code = ?, mtime = ?, size = ?, status = 'pending', error = NULL WHERE id = ?", params![kind, code as i64, mtime, size, f.id])?;
            return Ok(true);
        }
        self.db.execute("INSERT INTO files(path, kind, code, mtime, size) VALUES(?, ?, ?, ?, ?)", params![path, kind, code as i64, mtime, size])?;
        Ok(true)
    }

    pub fn clear_pieces(&self, file: i64) -> Result<()> {
        self.db.execute("DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ?)", [file])?;
        self.db.execute("DELETE FROM segments WHERE file = ?", [file])?;
        Ok(())
    }

    pub fn forget(&self, path: &str) -> Result<Option<i64>> {
        let Some(f) = self.file(path)? else { return Ok(None) };
        self.clear_pieces(f.id)?;
        self.db.execute("DELETE FROM files WHERE id = ?", [f.id])?;
        Ok(Some(f.id))
    }

    /// The next to read: what people look for most first, and code last.
    pub fn next_pending(&self, limit: usize) -> Result<Vec<FileRow>> {
        let mut st = self.db.prepare(
            "SELECT id, path, kind, mtime, size, status FROM files WHERE status = 'pending'
             ORDER BY code, CASE kind WHEN 'screenshot' THEN 0 WHEN 'image' THEN 1 WHEN 'pdf' THEN 2 WHEN 'doc' THEN 3
             WHEN 'audio' THEN 4 WHEN 'video' THEN 5 ELSE 6 END, mtime DESC LIMIT ?",
        )?;
        let rows = st.query_map([limit as i64], |r| Ok(FileRow { id: r.get(0)?, path: r.get(1)?, kind: r.get(2)?, mtime: r.get(3)?, size: r.get(4)?, status: r.get(5)? }))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// A file read: its pieces in, all at once. Returns the new pieces' ids.
    pub fn finish(&mut self, file: i64, pieces: &[Piece], info: &str) -> Result<Vec<i64>> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ?)", [file])?;
        tx.execute("DELETE FROM segments WHERE file = ?", [file])?;
        let mut ids = Vec::new();
        {
            let mut seg = tx.prepare("INSERT INTO segments(file, kind, loc, loc_end, modality, vec, excerpt) VALUES(?, ?, ?, ?, ?, ?, ?)")?;
            let mut fts = tx.prepare("INSERT INTO fts(rowid, title, body) VALUES(?, ?, ?)")?;
            for p in pieces {
                let blob = p.vec.as_ref().map(|v| to_blob(v));
                seg.execute(params![file, p.kind, p.loc, p.loc_end, p.modality, blob, p.excerpt])?;
                let id = tx.last_insert_rowid();
                if !p.title.is_empty() || !p.body.is_empty() {
                    fts.execute(params![id, p.title, p.body])?;
                }
                ids.push(id);
            }
        }
        tx.execute("UPDATE files SET status = 'done', error = NULL, info = ?, indexed = strftime('%s','now') WHERE id = ?", params![info, file])?;
        tx.commit()?;
        Ok(ids)
    }

    pub fn failed(&self, file: i64, why: &str) -> Result<()> {
        self.db.execute("UPDATE files SET status = 'error', error = ? WHERE id = ?", params![why, file])?;
        Ok(())
    }

    pub fn skipped(&self, file: i64, why: &str) -> Result<()> {
        self.db.execute("UPDATE files SET status = 'skipped', error = ? WHERE id = ?", params![why, file])?;
        Ok(())
    }

    pub fn counts(&self) -> Result<(i64, i64, i64, i64)> {
        let q = |s: &str| -> Result<i64> { Ok(self.db.query_row(&format!("SELECT COUNT(*) FROM files WHERE status = '{s}'"), [], |r| r.get(0))?) };
        Ok((q("done")?, q("pending")?, q("error")?, q("skipped")?))
    }

    pub fn all_paths(&self) -> Result<Vec<(String, bool)>> {
        let mut st = self.db.prepare("SELECT path, code FROM files")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}
