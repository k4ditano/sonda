//! What a file holds, as pieces the model can take: a picture, the text of a
//! page (or the page itself, when it is a scan), stretches of a document,
//! half minutes of sound, frames of a video, stretches of code. Long things
//! come out a piece at a time (`sink`), so a film is never all in memory.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow, bail};
use image::RgbImage;

use crate::store::Piece;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Screenshot,
    Image,
    Pdf,
    Doc,
    Audio,
    Video,
    Code,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Screenshot => "screenshot",
            Kind::Image => "image",
            Kind::Pdf => "pdf",
            Kind::Doc => "doc",
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Code => "code",
        }
    }
    pub fn from_name(s: &str) -> Option<Kind> {
        Some(match s {
            "screenshot" => Kind::Screenshot,
            "image" => Kind::Image,
            "pdf" => Kind::Pdf,
            "doc" => Kind::Doc,
            "audio" => Kind::Audio,
            "video" => Kind::Video,
            "code" => Kind::Code,
            _ => return None,
        })
    }
}

pub const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];
pub const DOC_EXT: &[&str] = &["txt", "text", "md", "markdown", "rtf", "docx", "odt", "html", "htm"];
pub const AUDIO_EXT: &[&str] = &["mp3", "wav", "flac", "ogg", "oga", "m4a", "opus", "aac", "wma"];
pub const VIDEO_EXT: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "3gp"];

pub const SOURCE_EXT: &[&str] = &[
    "py", "pyi", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "m", "mm", "swift", "ts", "tsx", "mts", "js", "jsx", "mjs", "cjs", "go", "rs",
    "java", "kt", "kts", "scala", "rb", "php", "cs", "sh", "bash", "zsh", "fish", "lua", "luau", "pl", "r", "jl", "dart", "sql", "vue",
    "svelte", "html", "htm", "css", "scss", "less", "gradle", "cmake", "proto", "graphql", "nix", "zig", "hs", "ex", "exs", "erl", "clj",
    "elm", "ml", "vim", "el", "glsl", "hlsl", "wgsl", "qml", "plm", "gd", "tex", "md", "markdown", "rst", "toml", "yaml", "yml",
];
pub const CODE_NAMES: &[&str] = &["Makefile", "makefile", "Dockerfile", "Justfile", "justfile", "CMakeLists.txt", "PKGBUILD"];

pub fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_secret(name: &str) -> bool {
    let lower = name.to_lowercase();
    let e = lower.rsplit('.').next().unwrap_or("");
    let secret_ext = ["pem", "key", "p12", "pfx", "keystore", "jks", "ppk", "kdbx", "gpg", "asc", "crt", "cer", "der", "p8"];
    if lower.contains('.') && secret_ext.contains(&e) {
        return true;
    }
    ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", ".env", "secret", "credential", ".netrc", ".npmrc", ".pypirc", "htpasswd", ".htpasswd", "token"]
        .iter()
        .any(|p| lower.starts_with(p))
}

pub fn is_screenshot(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    ["screenshot", "screen shot", "captura", "scr-", "cleanshot", "grim", "shot-", "pantallazo"].iter().any(|p| name.starts_with(p))
        || name.contains("captura de pantalla")
}

/// What a file in a folder read by its contents is.
pub fn classify(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy();
    if is_secret(&name) {
        return None;
    }
    let e = ext(path);
    if IMAGE_EXT.contains(&e.as_str()) {
        return Some(if is_screenshot(path) { Kind::Screenshot } else { Kind::Image });
    }
    if e == "pdf" {
        return Some(Kind::Pdf);
    }
    if DOC_EXT.contains(&e.as_str()) {
        return Some(Kind::Doc);
    }
    if AUDIO_EXT.contains(&e.as_str()) {
        return Some(Kind::Audio);
    }
    if VIDEO_EXT.contains(&e.as_str()) {
        return Some(Kind::Video);
    }
    None
}

/// What a file in a folder read as code is.
pub fn classify_code(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy();
    if is_secret(&name) {
        return None;
    }
    let lower = name.to_lowercase();
    if lower.ends_with(".min.js") || lower.ends_with(".min.css") || ["license", "licence", "copying", "notice"].iter().any(|p| lower.starts_with(p)) {
        return None;
    }
    if lower.ends_with(".lock") || lower == "package-lock.json" {
        return None;
    }
    if SOURCE_EXT.contains(&ext(path).as_str()) || CODE_NAMES.contains(&name.as_ref()) {
        return Some(Kind::Code);
    }
    None
}

/// What the model is given for a piece, before it has a vector.
pub enum Media {
    /// Words only (the keyword index): a file's name and folders.
    None,
    /// Words, already with their prompt.
    Text(String),
    /// A picture and its token budget.
    Picture(RgbImage, usize),
    /// Sound, mono at 16 kHz.
    Sound(Vec<f32>),
}

pub struct Draft {
    pub piece: Piece,
    pub media: Media,
}

pub type Sink<'a> = dyn FnMut(Draft) -> Result<()> + 'a;

pub struct Limits {
    pub max_pages: usize,
    pub max_seconds: f64,
}

/// A file's name and folders, for the keyword index: "Factura luz marzo.pdf" is
/// found by "factura" whatever it holds.
pub fn name_piece(path: &Path, root: &Path) -> Draft {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let folders = path.parent().and_then(|p| p.strip_prefix(root.parent().unwrap_or(root)).ok()).map(|p| p.to_string_lossy().replace('/', " ")).unwrap_or_default();
    let words = stem.replace(['_', '-', '.'], " ");
    Draft { piece: Piece { kind: "name".into(), modality: "none".into(), title: words, body: folders, ..Default::default() }, media: Media::None }
}

pub fn read(kind: Kind, path: &Path, limits: &Limits, sink: &mut Sink) -> Result<serde_json::Value> {
    match kind {
        Kind::Screenshot | Kind::Image => picture(path, if kind == Kind::Screenshot { 280 } else { 140 }, sink),
        Kind::Pdf => pdf(path, limits, sink),
        Kind::Doc => doc(path, sink),
        Kind::Audio => {
            let duration = probe_duration(path).unwrap_or(0.0);
            let n = sound(path, 0.0, limits.max_seconds, sink)?;
            if n == 0 {
                bail!("no sound in it");
            }
            Ok(serde_json::json!({ "duration": duration }))
        }
        Kind::Video => video(path, limits, sink),
        Kind::Code => Ok(serde_json::json!({})),
    }
}

// ── pictures ──

pub fn load_rgb(path: &Path) -> Result<RgbImage> {
    let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?;
    Ok(img.to_rgb8())
}

fn picture(path: &Path, budget: usize, sink: &mut Sink) -> Result<serde_json::Value> {
    let img = load_rgb(path)?;
    let (w, h) = img.dimensions();
    if w < 48 || h < 48 {
        bail!("a picture too small ({w}×{h})");
    }
    // A huge one is made smaller first: the encoder never sees more than ~1000 px.
    let img = if w.max(h) > 2048 { image::DynamicImage::ImageRgb8(img).thumbnail(1536, 1536).to_rgb8() } else { img };
    sink(Draft { piece: Piece { kind: "image".into(), modality: "image".into(), ..Default::default() }, media: Media::Picture(img, budget) })?;
    Ok(serde_json::json!({ "width": w, "height": h }))
}

// ── PDFs ──

fn run_text(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().map_err(|e| anyhow!("{cmd}: {e}"))?;
    if !out.status.success() {
        bail!("{cmd} failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn pdf_pages(path: &Path) -> Result<usize> {
    let info = run_text("pdfinfo", &[&path.to_string_lossy()])?;
    for line in info.lines() {
        if let Some(n) = line.strip_prefix("Pages:") {
            return Ok(n.trim().parse().unwrap_or(0));
        }
    }
    Ok(0)
}

/// One page of a PDF as a picture, at most `side` pixels on its longer side.
pub fn render_page(path: &Path, page: usize, side: u32, out_prefix: &Path) -> Result<std::path::PathBuf> {
    let p = page.to_string();
    let s = side.to_string();
    Command::new("pdftoppm")
        .args(["-f", &p, "-l", &p, "-scale-to", &s, "-png", "-singlefile"])
        .arg(path)
        .arg(out_prefix)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    let file = out_prefix.with_extension("png");
    if !file.exists() {
        bail!("pdftoppm made nothing for page {page}");
    }
    Ok(file)
}

fn pdf(path: &Path, limits: &Limits, sink: &mut Sink) -> Result<serde_json::Value> {
    let pages = pdf_pages(path)?;
    if pages == 0 {
        bail!("no pages (locked, or not a PDF)");
    }
    let last = pages.min(limits.max_pages);
    let text = run_text("pdftotext", &["-enc", "UTF-8", "-l", &last.to_string(), &path.to_string_lossy(), "-"]).unwrap_or_default();
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = crate::paths::cache_sub("tmp").join(format!("page-{}", std::process::id()));
    let mut rendered = 0;
    for (i, page_text) in text.split('\x0c').take(last).enumerate() {
        let n = i + 1;
        let clean = squash(page_text);
        if clean.chars().count() >= 200 {
            let embed: String = clean.chars().take(6000).collect();
            sink(Draft {
                piece: Piece { kind: "page".into(), loc: Some(n as f64), modality: "text".into(), title: title.clone(), body: clean.clone(), excerpt: Some(clean.chars().take(400).collect()), ..Default::default() },
                media: Media::Text(crate::embed::document(&title, &embed)),
            })?;
        } else if rendered < 80 {
            // Little text: a scan, a slide, a drawing. The page itself, seen.
            rendered += 1;
            if let Ok(file) = render_page(path, n, 1100, &tmp) {
                if let Ok(img) = load_rgb(&file) {
                    sink(Draft { piece: Piece { kind: "page_image".into(), loc: Some(n as f64), modality: "image".into(), title: title.clone(), body: clean.clone(), ..Default::default() }, media: Media::Picture(img, 280) })?;
                }
                let _ = std::fs::remove_file(&file);
            }
        }
    }
    Ok(serde_json::json!({ "pages": pages }))
}

// ── documents ──

pub fn squash(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            blank += 1;
            if blank == 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank = 0;
        out.push_str(&l);
        out.push('\n');
    }
    out.trim().to_owned()
}

fn decode_entities(s: &str) -> String {
    let mut t = s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'");
    let re = regex::Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").unwrap();
    t = re
        .replace_all(&t, |c: &regex::Captures| {
            let v = &c[1];
            let n = if let Some(h) = v.strip_prefix('x') { u32::from_str_radix(h, 16).ok() } else { v.parse().ok() };
            n.and_then(char::from_u32).map(String::from).unwrap_or_default()
        })
        .into_owned();
    t.replace("&amp;", "&")
}

pub fn html_text(html: &str) -> String {
    let mut t = html.to_owned();
    for p in [r"(?is)<script\b.*?</script\s*>", r"(?is)<style\b.*?</style\s*>", r"(?s)<!--.*?-->", r"(?is)<head\b.*?</head\s*>"] {
        t = regex::Regex::new(p).unwrap().replace_all(&t, " ").into_owned();
    }
    t = regex::Regex::new(r"(?i)<(br|/p|/div|/li|/h[1-6]|/tr|/section|/article)\b[^>]*>").unwrap().replace_all(&t, "\n").into_owned();
    t = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&t, " ").into_owned();
    squash(&decode_entities(&t))
}

fn xml_text(xml: &str, paragraph: &str) -> String {
    let t = xml.replace(paragraph, "\n").replace("<w:tab/>", "\t").replace("<text:tab/>", "\t").replace("<w:br/>", "\n").replace("<text:line-break/>", "\n");
    let t = regex::Regex::new(r"<[^>]+>").unwrap().replace_all(&t, "");
    squash(&decode_entities(&t))
}

fn zipped(path: &Path, inner: &str) -> Result<String> {
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f)?;
    let mut e = z.by_name(inner)?;
    let mut s = String::new();
    e.read_to_string(&mut s)?;
    Ok(s)
}

fn rtf_text(rtf: &str) -> String {
    let t = regex::Regex::new(r"\\par[d]?\b").unwrap().replace_all(rtf, "\n");
    let t = regex::Regex::new(r"\\'([0-9a-fA-F]{2})").unwrap().replace_all(&t, |c: &regex::Captures| {
        let b = u8::from_str_radix(&c[1], 16).unwrap_or(b' ');
        // Windows-1252 for the common Spanish letters; anything else as Latin-1.
        char::from(b).to_string()
    });
    let t = regex::Regex::new(r"\{\\\*[^{}]*\}").unwrap().replace_all(&t, "");
    let t = regex::Regex::new(r"\\[a-zA-Z]+-?\d* ?").unwrap().replace_all(&t, "");
    squash(&t.replace(['{', '}'], ""))
}

pub fn read_text_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    })
}

pub fn doc_text(path: &Path) -> Result<String> {
    Ok(match ext(path).as_str() {
        "docx" => xml_text(&zipped(path, "word/document.xml")?, "</w:p>"),
        "odt" => xml_text(&zipped(path, "content.xml")?, "</text:p>"),
        "rtf" => rtf_text(&read_text_file(path)?),
        "html" | "htm" => html_text(&read_text_file(path)?),
        _ => read_text_file(path)?,
    })
}

/// ~`size`-character pieces that overlap a little, cut at whitespace.
pub fn chunks(text: &str, size: usize, overlap: usize, limit: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() && out.len() < limit {
        let mut end = (start + size).min(chars.len());
        if end < chars.len() {
            if let Some(sp) = (start + size / 2..end).rev().find(|&i| chars[i].is_whitespace()) {
                end = sp;
            }
        }
        let piece: String = chars[start..end].iter().collect::<String>().trim().to_owned();
        if !piece.is_empty() {
            out.push(piece);
        }
        if end >= chars.len() {
            break;
        }
        let mut next = end.saturating_sub(overlap).max(start + 1);
        while next < end && !chars[next].is_whitespace() {
            next += 1;
        }
        start = next;
    }
    out
}

fn has_words(text: &str) -> bool {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    let digits = text.chars().filter(|c| c.is_ascii_digit()).count();
    letters > 20 && letters > digits
}

fn doc(path: &Path, sink: &mut Sink) -> Result<serde_json::Value> {
    let text = doc_text(path)?;
    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let pieces = chunks(&text, 1800, 200, 3000);
    if pieces.is_empty() {
        bail!("no text");
    }
    let n = pieces.len();
    for (i, c) in pieces.into_iter().enumerate() {
        let words = has_words(&c);
        sink(Draft {
            piece: Piece {
                kind: "chunk".into(),
                loc: Some(i as f64),
                modality: if words { "text" } else { "none" }.into(),
                title: title.clone(),
                excerpt: Some(c.chars().take(400).collect()),
                body: c.clone(),
                ..Default::default()
            },
            media: if words { Media::Text(crate::embed::document(&title, &c)) } else { Media::None },
        })?;
    }
    Ok(serde_json::json!({ "chunks": n, "chars": text.chars().count() }))
}

// ── sound ──

pub fn probe_duration(path: &Path) -> Option<f64> {
    let out = run_text("ffprobe", &["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", &path.to_string_lossy()]).ok()?;
    out.trim().parse().ok()
}

fn probe_video(path: &Path) -> Option<(u32, u32, bool)> {
    let out = run_text("ffprobe", &["-v", "error", "-show_entries", "stream=codec_type,width,height", "-of", "csv=p=0", &path.to_string_lossy()]).ok()?;
    let mut size = None;
    let mut audio = false;
    for line in out.lines() {
        let f: Vec<&str> = line.split(',').collect();
        match f.first().copied() {
            Some("video") if size.is_none() && f.len() >= 3 => {
                let (w, h) = (f[1].parse().ok()?, f[2].parse().ok()?);
                size = Some((w, h));
            }
            Some("audio") => audio = true,
            _ => {}
        }
    }
    size.map(|(w, h)| (w, h, audio))
}

/// Overlapping half minutes of sound (30 s windows, a new one every 25 s),
/// read as ffmpeg decodes it. Returns how many.
pub fn sound(path: &Path, from: f64, max_seconds: f64, sink: &mut Sink) -> Result<usize> {
    const RATE: usize = 16000;
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-ss", &format!("{from}"), "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-t", &format!("{max_seconds}"), "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let window = 30 * RATE;
    let hop = 25 * RATE;
    let mut buf: Vec<f32> = Vec::with_capacity(window);
    let mut start = 0usize; // in samples
    let mut count = 0;
    let mut bytes = vec![0u8; 4 * RATE];
    let mut pending: Vec<u8> = Vec::new();
    let mut ended = false;
    loop {
        while buf.len() < window && !ended {
            let n = out.read(&mut bytes)?;
            if n == 0 {
                ended = true;
                break;
            }
            pending.extend_from_slice(&bytes[..n]);
            let whole = pending.len() / 4 * 4;
            for c in pending[..whole].chunks_exact(4) {
                buf.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
            pending.drain(..whole);
        }
        if buf.len() < RATE * 2 {
            break; // less than two seconds: nothing worth hearing
        }
        let loud = buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32;
        let t = from + start as f64 / RATE as f64;
        if loud > 1e-6 {
            let take = buf.len().min(window);
            sink(Draft {
                piece: Piece { kind: "audio".into(), loc: Some(t), loc_end: Some(t + take as f64 / RATE as f64), modality: "audio".into(), ..Default::default() },
                media: Media::Sound(buf[..take].to_vec()),
            })?;
            count += 1;
        }
        if ended && buf.len() <= window {
            break;
        }
        let drop = hop.min(buf.len());
        buf.drain(..drop);
        start += drop;
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(count)
}

// ── video ──

fn tiny_gray(img: &RgbImage) -> Vec<u8> {
    let small = image::imageops::resize(img, 16, 16, image::imageops::FilterType::Triangle);
    small.pixels().map(|p| ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8).collect()
}

fn video(path: &Path, limits: &Limits, sink: &mut Sink) -> Result<serde_json::Value> {
    let duration = probe_duration(path).unwrap_or(0.0);
    let (w, h, has_audio) = probe_video(path).ok_or_else(|| anyhow!("no picture in it"))?;
    // Frames at most 448 px on their longer side (the encoder takes ~400 at 70 tokens).
    let scale = 448.0 / w.max(h) as f64;
    let fw = ((w as f64 * scale.min(1.0)) as u32 / 2 * 2).max(2);
    let fh = ((h as f64 * scale.min(1.0)) as u32 / 2 * 2).max(2);
    // One frame every three seconds, or fewer in a long one (at most ~1500).
    let step = (duration.min(limits.max_seconds) / 1500.0).max(3.0);
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-an", "-t", &format!("{}", limits.max_seconds), "-vf", &format!("fps=1/{step},scale={fw}:{fh}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut out = child.stdout.take().unwrap();
    let frame_bytes = (fw * fh * 3) as usize;
    let mut frame = vec![0u8; frame_bytes];
    let mut last: Option<Vec<u8>> = None;
    let mut index = 0usize;
    let mut kept = 0;
    loop {
        if out.read_exact(&mut frame).is_err() {
            break;
        }
        let t = index as f64 * step;
        index += 1;
        let img = RgbImage::from_raw(fw, fh, frame.clone()).unwrap();
        let g = tiny_gray(&img);
        // A frame much like the last one kept says nothing new.
        if let Some(l) = &last {
            let diff: u32 = l.iter().zip(&g).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs()).sum();
            if diff / 256 < 6 {
                continue;
            }
        }
        last = Some(g);
        sink(Draft { piece: Piece { kind: "frame".into(), loc: Some(t), modality: "image".into(), ..Default::default() }, media: Media::Picture(img, 70) })?;
        kept += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    if has_audio {
        let _ = sound(path, 0.0, limits.max_seconds, sink);
    }
    if kept == 0 && !has_audio {
        bail!("nothing could be read from it");
    }
    Ok(serde_json::json!({ "duration": duration, "width": w, "height": h }))
}

// ── code ──

/// A source file as stretches of whole lines (~1800 characters), each with
/// its path in its project as its title.
pub fn code(path: &Path, title: &str, sink: &mut Sink) -> Result<serde_json::Value> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > 1 << 20 {
        bail!("too big to be written by hand");
    }
    let text = read_text_file(path)?;
    if text.contains("PRIVATE KEY-----") {
        bail!("holds a private key");
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut stretches = 0;
    while i < lines.len() {
        let first = i;
        let mut size = 0;
        while i < lines.len() && (size == 0 || size + lines[i].len() < 1800) {
            size += lines[i].len() + 1;
            i += 1;
        }
        let body = lines[first..i].join("\n");
        if body.trim().is_empty() {
            continue;
        }
        sink(Draft {
            piece: Piece {
                kind: "lines".into(),
                loc: Some(first as f64 + 1.0),
                loc_end: Some(i as f64),
                modality: "text".into(),
                title: title.to_owned(),
                excerpt: Some(lines[first..i.min(first + 12)].join("\n")),
                body: body.clone(),
                vec: None,
            },
            media: Media::Text(crate::embed::document(title, &body)),
        })?;
        stretches += 1;
    }
    Ok(serde_json::json!({ "lines": lines.len(), "stretches": stretches }))
}
