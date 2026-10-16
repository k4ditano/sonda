//! «Open with…»: what kind of file something is (its MIME type, from the
//! shared MIME database), which programs say they open it, which one the
//! desktop opens it with, and opening it with another one —once, or always.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

fn data_dirs() -> Vec<PathBuf> {
    let home = crate::paths::home();
    let mut v = vec![std::env::var("XDG_DATA_HOME").ok().filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let sys = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    v.extend(sys.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    v.push(home.join(".local/share/flatpak/exports/share"));
    v.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    let mut seen = HashSet::new();
    v.retain(|d| seen.insert(d.clone()));
    v
}

struct MimeDb {
    /// «*.png» → image/png (the heaviest glob wins), by lower-case extension.
    ext: HashMap<String, (u32, String)>,
    /// Other globs (README, *.tar.gz…), heaviest first.
    other: Vec<(u32, String, String)>,
    /// image/svg+xml → application/xml…
    parents: HashMap<String, Vec<String>>,
    aliases: HashMap<String, String>,
}

fn mimedb() -> &'static MimeDb {
    static DB: OnceLock<MimeDb> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = MimeDb { ext: HashMap::new(), other: Vec::new(), parents: HashMap::new(), aliases: HashMap::new() };
        for d in data_dirs().iter().rev() {
            let m = d.join("mime");
            if let Ok(text) = std::fs::read_to_string(m.join("globs2")) {
                for line in text.lines().filter(|l| !l.starts_with('#')) {
                    let mut it = line.splitn(4, ':');
                    let (Some(w), Some(mime), Some(glob)) = (it.next(), it.next(), it.next()) else { continue };
                    let w: u32 = w.parse().unwrap_or(50);
                    match glob.strip_prefix("*.") {
                        Some(e) if !e.contains(['*', '?', '[']) && !e.contains('.') => {
                            let e = e.to_lowercase();
                            if db.ext.get(&e).is_none_or(|(old, _)| w > *old) {
                                db.ext.insert(e, (w, mime.to_owned()));
                            }
                        }
                        _ => db.other.push((w, glob.to_owned(), mime.to_owned())),
                    }
                }
            }
            if let Ok(text) = std::fs::read_to_string(m.join("subclasses")) {
                for line in text.lines() {
                    if let Some((child, parent)) = line.split_once(' ') {
                        db.parents.entry(child.to_owned()).or_default().push(parent.to_owned());
                    }
                }
            }
            if let Ok(text) = std::fs::read_to_string(m.join("aliases")) {
                for line in text.lines() {
                    if let Some((alias, real)) = line.split_once(' ') {
                        db.aliases.insert(alias.to_owned(), real.to_owned());
                    }
                }
            }
        }
        db.other.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.len().cmp(&a.1.len())));
        db
    })
}

/// A shell glob: `*`, `?` and `[…]`, case-blind.
pub fn glob_match(glob: &str, name: &str) -> bool {
    fn go(g: &[char], n: &[char]) -> bool {
        match g.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&g[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&g[1..], &n[1..]),
            Some('[') => {
                let Some(close) = g.iter().position(|c| *c == ']') else { return !n.is_empty() && n[0] == '[' && go(&g[1..], &n[1..]) };
                let set = &g[1..close];
                !n.is_empty() && set.contains(&n[0]) && go(&g[close + 1..], &n[1..])
            }
            Some(c) => !n.is_empty() && *c == n[0] && go(&g[1..], &n[1..]),
        }
    }
    let g: Vec<char> = glob.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    go(&g, &n)
}

/// The MIME type of a file: by its name, and when the name says nothing, by
/// what is in it (`file`).
pub fn mime_of(path: &Path) -> String {
    if path.is_dir() {
        return "inode/directory".into();
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let db = mimedb();
    for (_, glob, mime) in &db.other {
        if glob_match(glob, &name) {
            return mime.clone();
        }
    }
    if let Some(e) = Path::new(&name).extension() {
        if let Some((_, m)) = db.ext.get(&e.to_string_lossy().to_lowercase()) {
            return m.clone();
        }
    }
    Command::new("file")
        .args(["--mime-type", "-b", "-L", "--"])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|m| m.contains('/'))
        .unwrap_or_else(|| "application/octet-stream".into())
}

/// The type and what it descends from: a shell script is text too.
fn lineage(mime: &str) -> Vec<String> {
    let db = mimedb();
    let mut out = vec![db.aliases.get(mime).cloned().unwrap_or_else(|| mime.to_owned())];
    let mut i = 0;
    while i < out.len() && out.len() < 12 {
        if let Some(ps) = db.parents.get(&out[i]) {
            for p in ps {
                if !out.contains(p) {
                    out.push(p.clone());
                }
            }
        }
        i += 1;
    }
    // Everything textual is text/plain underneath.
    if mime.starts_with("text/") && !out.iter().any(|m| m == "text/plain") {
        out.push("text/plain".into());
    }
    out
}

/// Whether a file passes a picker's filter: its globs or its MIME types
/// (`image/*` too).
pub fn passes(path: &Path, globs: &[String], mimes: &[String]) -> bool {
    if globs.is_empty() && mimes.is_empty() {
        return true;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if globs.iter().any(|g| glob_match(g, &name)) {
        return true;
    }
    if mimes.is_empty() {
        return false;
    }
    let all = lineage(&mime_of(path));
    mimes.iter().any(|want| {
        if let Some(prefix) = want.strip_suffix("/*") {
            all.iter().any(|m| m.split('/').next() == Some(prefix))
        } else {
            want == "application/octet-stream" || all.iter().any(|m| m == want)
        }
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub icon: String,
    #[serde(skip)]
    pub file: PathBuf,
    #[serde(skip)]
    pub mimes: Vec<String>,
    pub recommended: bool,
    pub default: bool,
}

fn localized(map: &HashMap<String, String>, key: &str) -> Option<String> {
    let lang = std::env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()).or_else(|| std::env::var("LANG").ok()).unwrap_or_default();
    let lang = lang.split('.').next().unwrap_or("");
    let short = lang.split('_').next().unwrap_or("");
    [format!("{key}[{lang}]"), format!("{key}[{short}]"), key.to_owned()].iter().find_map(|k| map.get(k).cloned())
}

fn read_desktop(file: &Path, id: &str) -> Option<App> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut map = HashMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if in_entry {
            if let Some((k, v)) = line.split_once('=') {
                map.entry(k.trim().to_owned()).or_insert_with(|| v.trim().to_owned());
            }
        }
    }
    if map.get("Type").map(String::as_str) != Some("Application") || map.get("Hidden").map(String::as_str) == Some("true") || !map.contains_key("Exec") {
        return None;
    }
    // Only for this desktop, or not for it.
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    if let Some(only) = map.get("OnlyShowIn") {
        if !only.to_lowercase().split(';').any(|d| !d.is_empty() && desktop.split(':').any(|x| x == d)) {
            return None;
        }
    }
    if let Some(not) = map.get("NotShowIn") {
        if not.to_lowercase().split(';').any(|d| !d.is_empty() && desktop.split(':').any(|x| x == d)) {
            return None;
        }
    }
    let mimes = map.get("MimeType").map(|m| m.split(';').filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default();
    Some(App {
        id: id.to_owned(),
        name: localized(&map, "Name").unwrap_or_else(|| id.trim_end_matches(".desktop").to_owned()),
        icon: map.get("Icon").cloned().unwrap_or_else(|| "application-x-executable".into()),
        file: file.to_path_buf(),
        mimes,
        recommended: false,
        default: false,
    })
}

/// Every program, by its desktop id (the first one found counts).
pub fn all() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in data_dirs() {
        let root = d.join("applications");
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("desktop") {
                    continue;
                }
                // Subfolders give ids like «kde-org.kde.foo.desktop».
                let id = p.strip_prefix(&root).map(|r| r.to_string_lossy().replace('/', "-")).unwrap_or_default();
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(a) = read_desktop(&p, &id) {
                    out.push(a);
                }
            }
        }
    }
    out
}

pub fn default_for(mime: &str) -> Option<String> {
    let o = Command::new("xdg-mime").args(["query", "default", mime]).stderr(Stdio::null()).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// The programs for a type: the desktop's choice first, then the ones that
/// say they open it (or what it descends from), then the rest, by name.
pub fn for_mime(mime: &str) -> Vec<App> {
    let line = lineage(mime);
    let default = default_for(mime);
    let mut apps: Vec<App> = all()
        .into_iter()
        .filter(|a| a.id != "sonda.desktop" || mime == "inode/directory")
        .map(|mut a| {
            a.recommended = a.mimes.iter().any(|m| line.contains(m) || (m.ends_with("/*") && line.iter().any(|l| l.split('/').next() == m.strip_suffix("/*"))));
            a.default = default.as_deref() == Some(a.id.as_str());
            a
        })
        .collect();
    // What is never shown in menus is only worth it when it opens this.
    apps.retain(|a| a.recommended || !std::fs::read_to_string(&a.file).unwrap_or_default().contains("NoDisplay=true"));
    apps.sort_by(|a, b| b.default.cmp(&a.default).then(b.recommended.cmp(&a.recommended)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    apps
}

/// Opens the files with that program (gio reads its Exec line as the
/// desktop does); `always` makes it the choice for the type.
pub fn open_with(id: &str, paths: &[String], always: bool) -> Result<()> {
    let app = all().into_iter().find(|a| a.id == id).ok_or_else(|| anyhow!("{id} is not installed"))?;
    if always {
        if let Some(first) = paths.first() {
            let mime = mime_of(Path::new(first));
            let ok = Command::new("xdg-mime").args(["default", id, &mime]).status()?.success();
            if !ok {
                bail!("could not make {} the program for {mime}", app.name);
            }
        }
    }
    Command::new("setsid")
        .arg("-f")
        .arg("gio")
        .arg("launch")
        .arg(&app.file)
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
