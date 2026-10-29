//! Portuguese dictionary for a double-clicked word: the user's own glossary first, then the
//! Portuguese Wiktionary (fetched on a background thread, native only), and a link to the same
//! word on Dicio, which has no public API and so only opens in the browser.
//!
//! The glossary is a JSON file next to the app's other per-user data (the portable data folder
//! in portable mode), with export/import for backups. Nothing here may panic: the file, the
//! network answer and the selected text are all untrusted.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Longest word looked up; anything longer is a selection, not a word.
const MAX_WORD_CHARS: usize = 60;

/// Extracts one word from a selection: trims surrounding punctuation and quotes, keeps letters,
/// hyphens and apostrophes. `None` when nothing word-like is left.
pub fn lookup_word(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_matches(|c: char| !c.is_alphabetic());
    let count = trimmed.chars().count();
    if !(2..=MAX_WORD_CHARS).contains(&count) {
        return None;
    }
    let mut chars = trimmed.chars();
    let first_is_letter = chars.next().is_some_and(char::is_alphabetic);
    let rest_ok = chars.all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’'));
    (first_is_letter && rest_ok).then(|| trimmed.to_string())
}

/// Lowercase without accents, so "Usucapião" and "usucapiao" are the same key.
pub fn fold(text: &str) -> String {
    text.trim().chars().flat_map(char::to_lowercase).map(strip_accent).collect()
}

fn strip_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        other => other,
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Dicio addresses use the word in lowercase without accents: `dicio.com.br/usucapiao/`.
pub fn dicio_url(word: &str) -> String {
    format!("https://www.dicio.com.br/{}/", encode(&fold(word)))
}

pub fn wiktionary_url(word: &str) -> String {
    format!("https://pt.wiktionary.org/wiki/{}", encode(&word.to_lowercase()))
}

// ---------------------------------------------------------------------------------------------
// Wiktionary
// ---------------------------------------------------------------------------------------------

/// One part of a definition, such as "Substantivo" with its numbered senses.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub heading: String,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Definition {
    Loading,
    Found { title: String, sections: Vec<Section> },
    NotFound,
    Failed,
}

/// Sections of an entry that are not definitions. Their subsections (the "Brasil" and
/// "Portugal" pronunciations) are skipped with them.
const SKIPPED_SECTIONS: &[&str] = &[
    "etimologia",
    "pronúncia",
    "ver também",
    "no wikcionário",
    "ligações externas",
    "tradução",
    "traduções",
    "anagrama",
    "anagramas",
    "referências",
    "fontes",
    "notas",
    "divisão silábica",
    "formas alternativas",
    "galeria",
    "na wikipédia",
];

/// `== Name ==` → (2, "Name").
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|&c| c == '=').count();
    if level == 0 {
        return None;
    }
    let marks = "=".repeat(level);
    let inner = line.strip_prefix(&marks)?.strip_suffix(&marks)?.trim();
    (!inner.is_empty() && !inner.starts_with('=')).then_some((level, inner))
}

/// The definition sections of the Portuguese entry in a plain-text Wiktionary page.
pub fn parse_portuguese(extract: &str) -> Vec<Section> {
    let mut lines = extract.lines().map(str::trim);
    if !lines.by_ref().any(|line| heading(line).is_some_and(|(level, name)| level == 1 && name == "Português")) {
        return Vec::new();
    }
    let mut sections: Vec<Section> = Vec::new();
    let mut skipped_level = 0;
    for line in lines {
        if let Some((level, name)) = heading(line) {
            // The next language starts with a single "=" heading.
            if level == 1 {
                break;
            }
            if skipped_level != 0 && level > skipped_level {
                continue;
            }
            skipped_level = if SKIPPED_SECTIONS.contains(&name.to_lowercase().as_str()) { level } else { 0 };
            if skipped_level == 0 {
                sections.push(Section { heading: name.to_string(), lines: Vec::new() });
            }
            continue;
        }
        // Blank lines, and expressions listed without a definition ("preclusão temporal:").
        if skipped_level != 0 || line.is_empty() || line.ends_with(':') {
            continue;
        }
        if let Some(section) = sections.last_mut() {
            section.lines.push(line.to_string());
        }
    }
    sections.retain(|section| !section.lines.is_empty());
    sections
}

/// The text offered for a new glossary entry: the first section without its leading
/// syllable/gender line ("pre.clu.são, feminino").
pub fn suggested_definition(sections: &[Section]) -> String {
    let Some(first) = sections.first() else { return String::new() };
    let skip = first
        .lines
        .first()
        .is_some_and(|line| line.split_once(',').is_some_and(|(head, _)| head.contains('.') && head.chars().all(|c| c.is_alphabetic() || c == '.')));
    first.lines.iter().skip(usize::from(skip)).cloned().collect::<Vec<_>>().join("\n")
}

/// Answers already fetched this run, so looking a word up twice doesn't ask the network again.
type Cache = Mutex<HashMap<String, Arc<Mutex<Definition>>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The definition of `word`, fetched in the background the first time it is asked for; the UI
/// reads it each frame and is repainted when it arrives.
pub fn definition(ctx: &egui::Context, word: &str) -> Arc<Mutex<Definition>> {
    let key = word.to_lowercase();
    let Ok(mut map) = cache().lock() else { return Arc::new(Mutex::new(Definition::Failed)) };
    if let Some(existing) = map.get(&key) {
        let retry = existing.lock().map(|d| matches!(*d, Definition::Failed)).unwrap_or(true);
        if !retry {
            return existing.clone();
        }
    }
    let slot = Arc::new(Mutex::new(Definition::Loading));
    map.insert(key, slot.clone());
    drop(map);
    spawn_fetch(ctx.clone(), word.to_string(), slot.clone());
    slot
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_fetch(ctx: egui::Context, word: String, slot: Arc<Mutex<Definition>>) {
    let spawned = std::thread::Builder::new().name("dictionary".into()).spawn(move || {
        let result = fetch(&word);
        if let Ok(mut d) = slot.lock() {
            *d = result;
        }
        ctx.request_repaint();
    });
    if spawned.is_err() {
        log::warn!("dictionary: couldn't start the lookup thread");
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_fetch(_ctx: egui::Context, _word: String, slot: Arc<Mutex<Definition>>) {
    if let Ok(mut d) = slot.lock() {
        *d = Definition::Failed;
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        // The OS verifies certificates, so antivirus HTTPS scanning (Kaspersky) is trusted as it
        // is by craft-update.
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false)
            .user_agent(concat!("PdfCraft-dic/", env!("CARGO_PKG_VERSION"), " (https://github.com/BrunoBM88/PdfCraft-dic)"))
            .build()
            .new_agent()
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch(word: &str) -> Definition {
    let lower = word.to_lowercase();
    let mut candidates = vec![lower.clone()];
    if word != lower {
        candidates.push(word.to_string());
    }
    let mut failed = false;
    for title in candidates {
        match fetch_extract(&title) {
            Ok(Some(extract)) => {
                let sections = parse_portuguese(&extract);
                if !sections.is_empty() {
                    return Definition::Found { title, sections };
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("dictionary: {e}");
                failed = true;
            }
        }
    }
    if failed { Definition::Failed } else { Definition::NotFound }
}

/// The plain-text page for `title`, `None` when Wiktionary has no such page.
#[cfg(not(target_arch = "wasm32"))]
fn fetch_extract(title: &str) -> Result<Option<String>, String> {
    let url =
        format!("https://pt.wiktionary.org/w/api.php?action=query&prop=extracts&explaintext=1&redirects=1&format=json&titles={}", encode(title));
    let mut resp = agent().get(&url).call().map_err(|e| format!("couldn't reach Wiktionary ({e})"))?;
    if !resp.status().is_success() {
        return Err(format!("Wiktionary answered {}", resp.status()));
    }
    let body =
        resp.body_mut().with_config().limit(2 * 1024 * 1024).read_to_string().map_err(|e| format!("couldn't read Wiktionary's answer ({e})"))?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("unexpected Wiktionary answer ({e})"))?;
    let page = json.get("query").and_then(|q| q.get("pages")).and_then(|p| p.as_object()).and_then(|p| p.values().next());
    let Some(page) = page else { return Ok(None) };
    if page.get("missing").is_some() {
        return Ok(None);
    }
    Ok(page.get("extract").and_then(|e| e.as_str()).filter(|e| !e.is_empty()).map(str::to_string))
}

// ---------------------------------------------------------------------------------------------
// Glossary
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub term: String,
    pub definition: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub updated: String,
}

#[derive(Serialize, Deserialize)]
struct GlossaryFile {
    app: String,
    version: u32,
    entries: Vec<Entry>,
}

const FILE_APP: &str = "pdfcraft-glossary";

#[derive(Default)]
pub struct Glossary {
    entries: Vec<Entry>,
    /// Where it is saved; `None` keeps it in memory only (web, tests).
    path: Option<PathBuf>,
    /// Bumped on every change, so views can tell their copy is stale.
    pub revision: u64,
}

/// Plural endings (on folded keys) mapped back to the singular, longest first.
const PLURAL_RULES: &[(&str, &str)] = &[
    ("oes", "ao"),
    ("aes", "ao"),
    ("aos", "ao"),
    ("ais", "al"),
    ("eis", "el"),
    ("ois", "ol"),
    ("res", "r"),
    ("zes", "z"),
    ("ses", "s"),
    ("ns", "m"),
    ("s", ""),
];

fn singular_candidates(key: &str) -> Vec<String> {
    let mut out = vec![key.to_string()];
    for (suffix, replacement) in PLURAL_RULES {
        if key.chars().count() > suffix.len() + 2
            && let Some(stem) = key.strip_suffix(suffix)
        {
            out.push(format!("{stem}{replacement}"));
        }
    }
    out
}

/// The current UTC time as RFC 3339, the order imports compare `updated` by.
fn now() -> String {
    let secs = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs = i64::try_from(secs).unwrap_or(0);
    chrono::DateTime::from_timestamp(secs, 0).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default()
}

impl Glossary {
    /// The glossary file: `Glossario.json` in the portable data folder, else
    /// `%LOCALAPPDATA%\PdfCraft\Glossario.json` (Windows) or the platform's equivalent.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn default_path() -> Option<PathBuf> {
        if let Some(dir) = crate::portable::data_dir() {
            return Some(dir.join("Glossario.json"));
        }
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if cfg!(target_os = "macos") {
            env("HOME").map(|h| h.join("Library/Application Support/PdfCraft/Glossario.json"))
        } else if cfg!(windows) {
            env("LOCALAPPDATA").map(|d| d.join("PdfCraft").join("Glossario.json"))
        } else {
            env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("pdfcraft/Glossario.json"))
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn default_path() -> Option<PathBuf> {
        None
    }

    /// Reads the glossary at `path`; a missing or unreadable file gives an empty glossary that
    /// still saves there (an unreadable one is first kept aside as `.bak`, so nothing is lost).
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut glossary = Self { path, ..Default::default() };
        let Some(path) = glossary.path.clone() else { return glossary };
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_file(&text) {
                Ok(entries) => glossary.entries = entries,
                Err(e) => {
                    log::warn!("glossary: {} is unreadable ({e}); keeping it as .bak", path.display());
                    let _ = std::fs::rename(&path, path.with_extension("json.bak"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("glossary: couldn't read {} ({e})", path.display()),
        }
        glossary.sort();
        glossary
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn sort(&mut self) {
        self.entries.sort_by_key(|entry| fold(&entry.term));
    }

    /// The entry for `word`, also trying its singular ("preclusões" → "preclusão").
    pub fn find(&self, word: &str) -> Option<&Entry> {
        let candidates = singular_candidates(&fold(word));
        candidates.iter().find_map(|key| self.entries.iter().find(|entry| fold(&entry.term) == *key))
    }

    /// The entry whose term is exactly `term` (ignoring case and accents).
    pub fn get(&self, term: &str) -> Option<&Entry> {
        let key = fold(term);
        self.entries.iter().find(|entry| fold(&entry.term) == key)
    }

    /// Adds or updates an entry; `previous` is the term being renamed, if any.
    pub fn upsert(&mut self, term: &str, definition: &str, previous: Option<&str>) -> Result<(), String> {
        let term = term.trim();
        let definition = definition.trim();
        if term.is_empty() || definition.is_empty() {
            return Err("empty".into());
        }
        let keys = [Some(fold(term)), previous.map(fold)];
        let created = self.entries.iter().find(|e| keys.contains(&Some(fold(&e.term)))).map(|e| e.created.clone()).filter(|c| !c.is_empty());
        self.entries.retain(|e| !keys.contains(&Some(fold(&e.term))));
        let stamp = now();
        self.entries.push(Entry {
            term: term.to_string(),
            definition: definition.to_string(),
            created: created.unwrap_or_else(|| stamp.clone()),
            updated: stamp,
        });
        self.changed()
    }

    pub fn remove(&mut self, term: &str) -> Result<(), String> {
        let key = fold(term);
        self.entries.retain(|e| fold(&e.term) != key);
        self.changed()
    }

    fn changed(&mut self) -> Result<(), String> {
        self.sort();
        self.revision = self.revision.wrapping_add(1);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // Written beside and renamed over, so a crash mid-write never leaves half a file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.export()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// The whole glossary as the JSON file format (the same as an export).
    pub fn export(&self) -> String {
        let file = GlossaryFile { app: FILE_APP.into(), version: 1, entries: self.entries.clone() };
        serde_json::to_string_pretty(&file).unwrap_or_default()
    }

    /// Merges an exported file; for a term in both, the most recently updated definition wins.
    /// Returns how many entries were added or changed.
    pub fn import(&mut self, text: &str) -> Result<usize, String> {
        let incoming = parse_file(text)?;
        let mut changed = 0;
        for entry in incoming {
            let term = entry.term.trim();
            if term.is_empty() || entry.definition.trim().is_empty() {
                continue;
            }
            let key = fold(term);
            match self.entries.iter_mut().find(|e| fold(&e.term) == key) {
                Some(current) if current.updated >= entry.updated || current.definition == entry.definition => {}
                Some(current) => {
                    *current = entry;
                    changed += 1;
                }
                None => {
                    self.entries.push(entry);
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            self.changed()?;
        }
        Ok(changed)
    }
}

/// Entries from a glossary file, or from a bare list of entries.
fn parse_file(text: &str) -> Result<Vec<Entry>, String> {
    if let Ok(file) = serde_json::from_str::<GlossaryFile>(text) {
        return Ok(file.entries);
    }
    serde_json::from_str::<Vec<Entry>>(text).map_err(|e| e.to_string())
}

/// The app-wide glossary, loaded from disk the first time it is used.
pub fn glossary() -> &'static Mutex<Glossary> {
    static GLOSSARY: OnceLock<Mutex<Glossary>> = OnceLock::new();
    GLOSSARY.get_or_init(|| Mutex::new(Glossary::load(Glossary::default_path())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_trimmed_and_checked() {
        assert_eq!(lookup_word(" «preclusão», ").as_deref(), Some("preclusão"));
        assert_eq!(lookup_word("d'água").as_deref(), Some("d'água"));
        assert_eq!(lookup_word("a"), None);
        assert_eq!(lookup_word("art. 5º"), None);
        assert_eq!(lookup_word("123"), None);
        assert_eq!(lookup_word(""), None);
    }

    #[test]
    fn dicio_urls_drop_accents() {
        assert_eq!(dicio_url("Usucapião"), "https://www.dicio.com.br/usucapiao/");
        assert_eq!(dicio_url("litisconsórcio"), "https://www.dicio.com.br/litisconsorcio/");
    }

    #[test]
    fn portuguese_section_is_parsed() {
        let extract = "= Português =\n\n== Substantivo ==\npre.clu.são, feminino\n(Direito) perda do direito\n\n=== Expressões ===\npreclusão temporal:\n\n== Pronúncia ==\n=== Brasil ===\nAFI: /x/\n\n== Etimologia ==\nDo latim\n\n= Inglês =\n== Noun ==\nother";
        let sections = parse_portuguese(extract);
        assert_eq!(
            sections,
            vec![Section { heading: "Substantivo".into(), lines: vec!["pre.clu.são, feminino".into(), "(Direito) perda do direito".into()] }]
        );
        assert_eq!(suggested_definition(&sections), "(Direito) perda do direito");
        assert!(parse_portuguese("= Inglês =\n== Noun ==\nx").is_empty());
        assert!(parse_portuguese("").is_empty());
    }

    #[test]
    fn glossary_finds_plurals_and_merges_imports() {
        let mut g = Glossary::default();
        assert!(g.upsert("preclusão", "perda do direito", None).is_ok());
        assert_eq!(g.find("Preclusões").map(|e| e.term.as_str()), Some("preclusão"));
        assert_eq!(g.find("PRECLUSAO").map(|e| e.term.as_str()), Some("preclusão"));
        assert!(g.find("ação").is_none());
        assert!(g.upsert("", "x", None).is_err());

        let older = r#"{"app":"pdfcraft-glossary","version":1,"entries":[
            {"term":"preclusão","definition":"antiga","created":"2000-01-01T00:00:00Z","updated":"2000-01-01T00:00:00Z"},
            {"term":"litisconsórcio","definition":"várias partes","created":"2000-01-01T00:00:00Z","updated":"2000-01-01T00:00:00Z"}]}"#;
        assert_eq!(g.import(older), Ok(1));
        assert_eq!(g.get("preclusao").map(|e| e.definition.as_str()), Some("perda do direito"));
        assert!(g.import("not json").is_err());

        assert!(g.upsert("litisconsorte", "parte", Some("litisconsórcio")).is_ok());
        assert!(g.get("litisconsórcio").is_none());
        assert!(g.remove("litisconsorte").is_ok());
        assert_eq!(g.entries().len(), 1);
    }
}
