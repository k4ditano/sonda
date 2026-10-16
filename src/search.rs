//! Finding: by meaning (EmbeddingGemma's vectors, every one scored against
//! the query) and by words (FTS5), as DigUp does. Meaning is put on one scale
//! per query —each vector's cosine minus its modality's average, in standard
//! deviations of all of them—, and only what stands out counts, which also
//! gives an honest «nothing found». Words rescue what a vector cannot hold:
//! invoice numbers, codes, names.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::embed::DIM;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Modality {
    None,
    Text,
    Image,
    Audio,
}

fn modality(s: &str) -> Modality {
    match s {
        "text" => Modality::Text,
        "image" => Modality::Image,
        "audio" => Modality::Audio,
        _ => Modality::None,
    }
}

/// Every vector, in memory, row by row.
pub struct Vectors {
    data: Vec<f32>,
    segment: Vec<i64>,
    file: Vec<i64>,
    modality: Vec<Modality>,
    code: Vec<bool>,
    gone: HashSet<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub segment: String,
    pub loc: Option<f64>,
    pub loc_end: Option<f64>,
    pub excerpt: Option<String>,
    pub score: f64,
    pub z: Option<f64>,
    pub mtime: f64,
    pub size: i64,
    pub info: serde_json::Value,
    /// Words that matched, for the preview to mark.
    pub terms: Vec<String>,
}

impl Vectors {
    pub fn load(db: &Connection) -> Result<Vectors> {
        let mut v = Vectors { data: Vec::new(), segment: Vec::new(), file: Vec::new(), modality: Vec::new(), code: Vec::new(), gone: HashSet::new() };
        let mut st = db.prepare("SELECT s.id, s.file, s.modality, s.vec, f.code FROM segments s JOIN files f ON f.id = s.file WHERE s.vec IS NOT NULL ORDER BY s.id")?;
        let mut rows = st.query([])?;
        let mut row = vec![0f32; DIM];
        while let Some(r) = rows.next()? {
            let blob: Vec<u8> = r.get(3)?;
            if blob.len() != DIM * 2 {
                continue;
            }
            crate::store::from_blob(&blob, &mut row);
            v.data.extend_from_slice(&row);
            v.segment.push(r.get(0)?);
            v.file.push(r.get(1)?);
            v.modality.push(modality(&r.get::<_, String>(2)?));
            v.code.push(r.get::<_, i64>(4)? != 0);
        }
        Ok(v)
    }

    pub fn len(&self) -> usize {
        self.segment.len()
    }

    pub fn add(&mut self, segment: i64, file: i64, modality_name: &str, code: bool, vec: &[f32]) {
        self.data.extend_from_slice(&vec[..DIM]);
        self.segment.push(segment);
        self.file.push(file);
        self.modality.push(modality(modality_name));
        self.code.push(code);
    }

    /// A file read again or gone: its rows go.
    pub fn forget_file(&mut self, file: i64) {
        if self.file.contains(&file) {
            self.gone.insert(file);
            self.compact();
        }
    }

    fn compact(&mut self) {
        let mut keep = 0;
        for r in 0..self.segment.len() {
            if self.gone.contains(&self.file[r]) {
                continue;
            }
            if keep != r {
                self.data.copy_within(r * DIM..(r + 1) * DIM, keep * DIM);
                self.segment[keep] = self.segment[r];
                self.file[keep] = self.file[r];
                self.modality[keep] = self.modality[r];
                self.code[keep] = self.code[r];
            }
            keep += 1;
        }
        self.data.truncate(keep * DIM);
        self.segment.truncate(keep);
        self.file.truncate(keep);
        self.modality.truncate(keep);
        self.code.truncate(keep);
        self.gone.clear();
    }
}

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "in", "on", "at", "to", "for", "with", "and", "or", "my", "me", "that", "this", "these", "those", "from", "about",
    "where", "what", "which", "who", "when", "is", "was", "are", "were", "be", "it", "its", "some", "any", "find", "show", "file", "files",
    "photo", "photos", "picture", "image", "images", "screenshot", "screenshots", "video", "videos", "pdf", "document", "documents",
    "recording", "de", "la", "el", "en", "y", "que", "los", "las", "un", "una", "unos", "unas", "del", "al", "por", "con", "para", "mi",
    "mis", "su", "sus", "lo", "se", "es", "como", "donde", "cuando", "esta", "este", "eso", "esa", "hay", "foto", "fotos", "imagen",
    "imágenes", "imagenes", "vídeo", "vídeos", "documento", "documentos", "archivo", "archivos", "captura", "capturas", "busca", "buscar",
    "encuentra", "grabación", "audio", "canción",
];

fn kind_words(word: &str) -> Option<&'static str> {
    Some(match word {
        "screenshot" | "screenshots" | "captura" | "capturas" | "pantallazo" => "screenshot",
        "photo" | "photos" | "picture" | "pictures" | "image" | "images" | "foto" | "fotos" | "imagen" | "imágenes" | "imagenes" => "image",
        "pdf" | "pdfs" | "paper" | "papers" => "pdf",
        "document" | "documents" | "note" | "notes" | "documento" | "documentos" | "nota" | "notas" => "doc",
        "video" | "videos" | "vídeo" | "vídeos" | "clip" | "movie" | "película" | "pelicula" => "video",
        "audio" | "song" | "podcast" | "voice" | "sound" | "canción" | "cancion" | "grabación" | "grabacion" | "voz" | "sonido" | "música" | "musica" => "audio",
        _ => return None,
    })
}

pub fn terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2 && !STOPWORDS.contains(w))
        .filter(|w| seen.insert(w.to_string()))
        .map(|w| w.to_owned())
        .collect()
}

fn is_lookup(terms: &[String]) -> bool {
    !terms.is_empty() && terms.iter().all(|t| t.chars().any(|c| c.is_ascii_digit()) && (t.chars().any(|c| c.is_alphabetic()) || t.len() >= 4))
}

pub struct Query<'a> {
    pub text: &'a str,
    pub vector: Option<&'a [f32]>,
    pub code: bool,
    pub kinds: Option<HashSet<String>>,
    pub limit: usize,
    /// Only files under this folder.
    pub under: Option<String>,
}

pub fn search(db: &Connection, vectors: &Vectors, q: &Query) -> Result<Vec<Hit>> {
    let z_gate = 2.0;
    let word_weight = if q.code { 1.0 } else { 2.0 };
    let mut file_kind: HashMap<i64, (String, String, bool)> = HashMap::new();
    {
        let mut st = db.prepare("SELECT id, kind, path, code FROM files WHERE status = 'done'")?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            file_kind.insert(r.get(0)?, (r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0));
        }
    }
    let allowed = |file: i64| -> bool {
        let Some((kind, path, code)) = file_kind.get(&file) else { return false };
        if *code != q.code {
            return false;
        }
        if let Some(k) = &q.kinds {
            let k2 = if kind == "screenshot" && k.contains("image") { "image" } else { kind.as_str() };
            if !k.contains(k2) && !k.contains(kind.as_str()) {
                return false;
            }
        }
        if let Some(u) = &q.under {
            if !path.starts_with(&format!("{u}/")) {
                return false;
            }
        }
        true
    };

    // Meaning.
    let mut best: HashMap<i64, (f64, f64, usize)> = HashMap::new();
    if let Some(query) = q.vector {
        let rows: Vec<usize> = (0..vectors.len()).filter(|&r| vectors.code[r] == q.code && !vectors.gone.contains(&vectors.file[r])).collect();
        if !rows.is_empty() {
            let scores: Vec<f32> = rows.iter().map(|&r| crate::embed::cosine(&vectors.data[r * DIM..(r + 1) * DIM], query)).collect();
            // Each modality's average (a small one takes everyone's).
            let mut sum: HashMap<Modality, (f64, usize)> = HashMap::new();
            for (i, &r) in rows.iter().enumerate() {
                let e = sum.entry(vectors.modality[r]).or_insert((0.0, 0));
                e.0 += scores[i] as f64;
                e.1 += 1;
            }
            let all = scores.iter().map(|&s| s as f64).sum::<f64>() / scores.len() as f64;
            let mean = |m: Modality| -> f64 {
                match sum.get(&m) {
                    Some((s, n)) if *n >= 20 => s / *n as f64,
                    _ => all,
                }
            };
            let centered: Vec<f64> = rows.iter().enumerate().map(|(i, &r)| scores[i] as f64 - mean(vectors.modality[r])).collect();
            let m = centered.iter().sum::<f64>() / centered.len() as f64;
            let dev = (centered.iter().map(|c| (c - m) * (c - m)).sum::<f64>() / centered.len() as f64).sqrt().max(0.02);
            for (i, &r) in rows.iter().enumerate() {
                let f = vectors.file[r];
                if !allowed(f) {
                    continue;
                }
                let z = (centered[i] - m) / dev;
                if best.get(&f).is_none_or(|b| z > b.0) {
                    best.insert(f, (z, scores[i] as f64, r));
                }
            }
        }
    }

    // Words.
    let ts = terms(q.text);
    let mut words: Vec<(i64, i64, Option<String>)> = Vec::new();
    let mut coverage: HashMap<i64, f64> = HashMap::new();
    if !ts.is_empty() {
        let mut patterns: Vec<String> = Vec::new();
        for (i, t) in ts.iter().enumerate() {
            let quoted = format!("\"{}\"", t.replace('"', ""));
            let last = i == ts.len() - 1;
            // A word in no file as typed matches as the start of longer ones, as
            // the last word does while it is being typed.
            let found: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM fts WHERE fts MATCH ?)", [&quoted], |r| r.get(0)).unwrap_or(false);
            patterns.push(if (last || !found) && t.chars().count() >= 3 { format!("{quoted}*") } else { quoted });
        }
        let joined = patterns.join(" OR ");
        let mut seen = HashSet::new();
        let mut st = db.prepare("SELECT s.file, s.id, snippet(fts, 1, '«', '»', '…', 14) FROM fts JOIN segments s ON s.id = fts.rowid WHERE fts MATCH ? ORDER BY bm25(fts, 3.0, 1.0) LIMIT 1500")?;
        let mut rows = st.query([&joined])?;
        while let Some(r) = rows.next()? {
            let f: i64 = r.get(0)?;
            if words.len() >= 150 || !allowed(f) || !seen.insert(f) {
                continue;
            }
            words.push((f, r.get(1)?, r.get(2)?));
        }
        if !words.is_empty() {
            let ids = words.iter().map(|w| w.0.to_string()).collect::<Vec<_>>().join(",");
            for p in &patterns {
                let mut st = db.prepare(&format!("SELECT DISTINCT s.file FROM fts JOIN segments s ON s.id = fts.rowid WHERE fts MATCH ? AND s.file IN ({ids})"))?;
                let mut rows = st.query([p])?;
                while let Some(r) = rows.next()? {
                    *coverage.entry(r.get(0)?).or_insert(0.0) += 1.0 / patterns.len() as f64;
                }
            }
        }
    }

    let lookup = is_lookup(&ts);
    let (mw, ww) = if lookup { (0.5, word_weight * 2.0) } else { (1.0, word_weight) };
    let hinted: HashSet<&str> = q.text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter_map(kind_words).collect();
    let mut candidates: HashSet<i64> = best.iter().filter(|(_, b)| b.0 >= z_gate).map(|(f, _)| *f).collect();
    for (f, share) in &coverage {
        if *share >= 0.5 {
            candidates.insert(*f);
        }
    }
    // Few that stand out: the nearest ones by meaning, and any with one of the
    // words, come too (lower), so a small index or a vague question still
    // shows something worth a look.
    if candidates.len() < 6 {
        let mut near: Vec<(i64, f64)> = best.iter().filter(|(_, b)| b.0 >= 1.0).map(|(f, b)| (*f, b.0)).collect();
        near.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (f, _) in near.into_iter().take(10) {
            candidates.insert(f);
        }
        for (f, share) in &coverage {
            if *share > 0.0 {
                candidates.insert(*f);
            }
        }
    }
    let mut scored: Vec<(i64, f64)> = candidates
        .into_iter()
        .map(|f| {
            let mut s = mw * best.get(&f).map(|b| b.0.max(0.0)).unwrap_or(0.0) + ww * coverage.get(&f).copied().unwrap_or(0.0).powf(1.5);
            if let Some((k, _, _)) = file_kind.get(&f) {
                let k = if k == "screenshot" && hinted.contains("image") { "image" } else { k.as_str() };
                if hinted.contains(k) {
                    s += 1.0;
                }
            }
            (f, s)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));

    let word_of: HashMap<i64, (i64, Option<String>)> = words.iter().map(|w| (w.0, (w.1, w.2.clone()))).collect();
    let mut hits = Vec::new();
    for (f, score) in scored.into_iter().take(q.limit) {
        let sem = best.get(&f);
        let kw = word_of.get(&f);
        let use_meaning = sem.is_some_and(|s| s.0 >= z_gate || kw.is_none());
        let seg = if use_meaning { sem.map(|s| vectors.segment[s.2]) } else { kw.map(|k| k.0) };
        let Some(seg) = seg else { continue };
        let (path, kind, mtime, size, info): (String, String, f64, i64, Option<String>) =
            db.query_row("SELECT path, kind, mtime, size, info FROM files WHERE id = ?", [f], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        let (skind, loc, loc_end, stored): (String, Option<f64>, Option<f64>, Option<String>) =
            db.query_row("SELECT kind, loc, loc_end, excerpt FROM segments WHERE id = ?", [seg], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        // The matched words, only when they come from the piece pointed at.
        let snippet = kw.and_then(|k| if k.0 == seg { k.1.clone() } else { None }).filter(|s| s.contains('«'));
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        hits.push(Hit {
            name,
            path,
            kind,
            segment: skind.clone(),
            loc,
            loc_end,
            excerpt: snippet.or(if skind == "name" { None } else { stored }),
            score,
            z: sem.map(|s| s.0),
            mtime,
            size,
            info: info.and_then(|i| serde_json::from_str(&i).ok()).unwrap_or(serde_json::Value::Null),
            terms: ts.clone(),
        });
    }
    Ok(hits)
}
