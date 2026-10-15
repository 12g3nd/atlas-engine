//! Storage operations, history retention and offline queries.
use crate::model::*;
use crate::repository::{err, rows, set_setting, setting, Repository};
use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Preferences {
    pub writing_font: String,
    pub text_size: f64,
    pub page_width: String,
    pub spellcheck_default: bool,
    pub close_to_tray: bool,
    pub launch_at_login: bool,
    pub capture_hotkey: String,
    pub backup_folder: Option<String>,
    pub last_destination: Option<String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            writing_font: "quattro".into(),
            text_size: 17.5,
            page_width: "medium".into(),
            spellcheck_default: true,
            close_to_tray: true,
            // Host preference retained for bundle compatibility.
            launch_at_login: true,
            capture_hotkey: "Ctrl+Alt+Space".into(),
            backup_folder: None,
            last_destination: None,
        }
    }
}
impl Preferences {
    pub fn sanitized(mut self) -> Self {
        if !["quattro", "literata", "mono"].contains(&self.writing_font.as_str()) {
            self.writing_font = "quattro".into();
        }
        if !self.text_size.is_finite() {
            self.text_size = 17.5;
        }
        self.text_size = self.text_size.clamp(15.0, 21.0);
        if !["narrow", "medium", "wide"].contains(&self.page_width.as_str()) {
            self.page_width = "medium".into();
        }
        if self.capture_hotkey.trim().is_empty() || self.capture_hotkey.len() > 60 {
            self.capture_hotkey = "Ctrl+Alt+Space".into();
        }
        if self
            .last_destination
            .as_ref()
            .map(|s| s.len() > 300)
            .unwrap_or(false)
        {
            self.last_destination = None;
        }
        self
    }
}

fn day_key(t: &str, offset: &FixedOffset) -> Option<String> {
    DateTime::parse_from_rfc3339(t)
        .ok()
        .map(|d| d.with_timezone(offset).format("%Y-%m-%d").to_string())
}
fn lower(s: &str) -> String {
    s.to_lowercase()
}
/// Case-insensitive whole-phrase search returning a char-boundary-safe byte index.
fn find_phrase(haystack: &str, needle: &str) -> Option<usize> {
    if needle.chars().count() < 3 {
        return None;
    }
    let hl = lower(haystack);
    let nl = lower(needle);
    if hl.len() != haystack.len() || nl.len() != needle.len() {
        // Lower-casing changed byte lengths; fall back to exact-case search.
        return whole_word(haystack, needle, haystack.find(needle)?);
    }
    let mut from = 0;
    while let Some(i) = hl[from..].find(&nl) {
        let at = from + i;
        if whole_word(haystack, needle, at).is_some() {
            return Some(at);
        }
        from = at + nl.len().max(1);
        if from >= hl.len() {
            break;
        }
    }
    None
}
fn whole_word(h: &str, n: &str, at: usize) -> Option<usize> {
    let before = h[..at].chars().last();
    let after = h[at + n.len()..].chars().next();
    let edge = |c: Option<char>| c.map(|c| !c.is_alphanumeric()).unwrap_or(true);
    (edge(before) && edge(after)).then_some(at)
}
fn snippet_around(text: &str, at: usize, len: usize) -> String {
    let start = text[..at]
        .char_indices()
        .rev()
        .nth(70)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end_base = at + len;
    let end = text[end_base..]
        .char_indices()
        .nth(90)
        .map(|(i, _)| end_base + i)
        .unwrap_or(text.len());
    let clean = |s: &str| s.replace(['\n', '\t'], " ");
    format!(
        "{}{}\u{2}{}\u{3}{}{}",
        if start > 0 { "…" } else { "" },
        clean(&text[start..at]),
        clean(&text[at..end_base]),
        clean(&text[end_base..end]),
        if end < text.len() { "…" } else { "" }
    )
}
/// Plain text of a document outside poems and code, block by block.
fn prose_blocks(node: &Value, out: &mut Vec<String>) {
    let kind = node["type"].as_str().unwrap_or("");
    if kind == "poetry" || kind == "codeBlock" {
        return;
    }
    if ["paragraph", "heading", "tableCell", "tableHeader"].contains(&kind) {
        out.push(inline_text(node, None));
        return;
    }
    if let Some(a) = node["content"].as_array() {
        for c in a {
            prose_blocks(c, out);
        }
    }
}
fn inline_text(node: &Value, link: Option<(&str, &str)>) -> String {
    match node["type"].as_str().unwrap_or("") {
        "text" => node["text"].as_str().unwrap_or("").into(),
        "hardBreak" => " ".into(),
        "atlasLink" => {
            let target = node["attrs"]["target"].as_str().unwrap_or("");
            let label = node["attrs"]["label"].as_str().unwrap_or("");
            match link {
                Some((t, title)) if t == target => {
                    format!("\u{2}{}\u{3}", if label.is_empty() { title } else { label })
                }
                _ => label.to_string(),
            }
        }
        _ => node["content"]
            .as_array()
            .map(|a| a.iter().map(|c| inline_text(c, link)).collect())
            .unwrap_or_default(),
    }
}
fn block_with_link(node: &Value, target: &str, title: &str) -> Option<String> {
    let kind = node["type"].as_str().unwrap_or("");
    if ["paragraph", "heading", "tableCell", "tableHeader"].contains(&kind) {
        let s = inline_text(node, Some((target, title)));
        return s.contains('\u{2}').then_some(s);
    }
    node["content"]
        .as_array()?
        .iter()
        .find_map(|c| block_with_link(c, target, title))
}
fn trim_snippet(s: &str) -> String {
    let Some(at) = s.find('\u{2}') else {
        return s.chars().take(160).collect();
    };
    let start = s[..at]
        .char_indices()
        .rev()
        .nth(60)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end = s[at..]
        .char_indices()
        .nth(120)
        .map(|(i, _)| at + i)
        .unwrap_or(s.len());
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        &s[start..end],
        if end < s.len() { "…" } else { "" }
    )
}
fn display_title(e: &Entry) -> String {
    if !e.title.trim().is_empty() {
        return e.title.clone();
    }
    plain(&e.body)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(100).collect())
        .unwrap_or_else(|| "Untitled".into())
}
/// Replace the first whole-phrase occurrence in prose with a stable link.
fn link_first(node: &mut Value, phrase: &str, target: &str) -> bool {
    let kind = node["type"].as_str().unwrap_or("").to_string();
    if kind == "poetry" || kind == "codeBlock" {
        return false;
    }
    let Some(children) = node["content"].as_array_mut() else {
        return false;
    };
    for i in 0..children.len() {
        if children[i]["type"] == "text" {
            let text = children[i]["text"].as_str().unwrap_or("").to_string();
            if let Some(at) = find_phrase(&text, phrase) {
                let marks = children[i].get("marks").cloned();
                let mut parts = vec![];
                let piece = |t: &str| {
                    let mut v = json!({"type":"text","text":t});
                    if let Some(m) = &marks {
                        v["marks"] = m.clone();
                    }
                    v
                };
                if at > 0 {
                    parts.push(piece(&text[..at]));
                }
                parts.push(json!({"type":"atlasLink","attrs":{"target":target,"label":"","autoLabel":true}}));
                let rest = &text[at + phrase.len()..];
                if !rest.is_empty() {
                    parts.push(piece(rest));
                }
                children.splice(i..=i, parts);
                return true;
            }
        } else if link_first(&mut children[i], phrase, target) {
            return true;
        }
    }
    false
}
fn body_from_text(text: &str) -> Value {
    let mut blocks = vec![];
    for chunk in text.replace("\r\n", "\n").split("\n\n") {
        let lines: Vec<&str> = chunk.split('\n').collect();
        let mut content = vec![];
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                content.push(json!({"type":"hardBreak"}));
            }
            if !line.is_empty() {
                content.push(json!({"type":"text","text":line}));
            }
        }
        while content
            .last()
            .map(|c| c["type"] == "hardBreak")
            .unwrap_or(false)
        {
            content.pop();
        }
        if content.is_empty() {
            continue;
        }
        blocks.push(json!({"type":"paragraph","content":content}));
    }
    if blocks.is_empty() {
        blocks.push(json!({"type":"paragraph"}));
    }
    json!({"type":"doc","content":blocks})
}

impl Repository {
    pub fn prefs_path(&self) -> PathBuf {
        self.base.join("preferences.json")
    }
    pub fn preferences(&self) -> Preferences {
        fs::read_to_string(self.prefs_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Preferences>(&s).ok())
            .unwrap_or_default()
            .sanitized()
    }
    pub fn save_preferences(&self, p: &Preferences) -> Result<()> {
        if let Some(folder) = &p.backup_folder {
            check_writable(Path::new(folder))?;
        }
        let tmp = self.base.join("preferences.json.part");
        fs::write(&tmp, serde_json::to_vec_pretty(p).map_err(err)?).map_err(err)?;
        fs::rename(&tmp, self.prefs_path()).map_err(err)
    }
    pub fn backup_folder(&self) -> PathBuf {
        self.preferences()
            .backup_folder
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| self.base.join("backups"))
    }
    pub fn view_v2(&self) -> Result<Value> {
        let c = self.canonical()?;
        let trash_count = c.entries.iter().filter(|e| e.trashed.is_some()).count();
        let entries: Vec<_> = c
            .entries
            .into_iter()
            .filter(|e| e.trashed.is_none())
            .collect();
        let ids: HashSet<_> = entries.iter().map(|e| e.id.as_str()).collect();
        let relations: Vec<_> = c
            .relations
            .into_iter()
            .filter(|r| ids.contains(r.source.as_str()) && ids.contains(r.target.as_str()))
            .collect();
        let attachments: Vec<_> = c
            .attachments
            .into_iter()
            .filter(|a| ids.contains(a.entry_id.as_str()))
            .collect();
        Ok(json!({
            "entries": entries,
            "areas": c.areas,
            "collections": c.collections,
            "relations": relations,
            "attachments": attachments,
            "kinds": c.kinds,
            "data_path": self.root,
            "backup_path": self.backup_folder(),
            "library_id": c.library_id,
            "test_mode": self.test,
            "trash_count": trash_count,
        }))
    }
    pub fn create_in(
        &mut self,
        entity_type: Option<String>,
        area_ids: Vec<String>,
    ) -> Result<Entry> {
        let areas: HashSet<String> = rows::<Area>(&self.db, "areas")?
            .into_iter()
            .map(|a| a.id)
            .collect();
        if area_ids.iter().any(|a| !areas.contains(a)) || area_ids.len() > 50 {
            return Err("Unknown area".into());
        }
        let mut e = self.create(entity_type)?;
        if !area_ids.is_empty() {
            e.area_ids = area_ids;
            let rev = e.revision;
            e = self.save(e, rev)?;
        }
        Ok(e)
    }
    pub fn add_area(&mut self, id: &str, area_id: &str) -> Result<Entry> {
        let mut e = self.entry(id)?;
        rows::<Area>(&self.db, "areas")?
            .iter()
            .find(|a| a.id == area_id)
            .ok_or("Unknown area")?;
        if !e.area_ids.iter().any(|a| a == area_id) {
            e.area_ids.push(area_id.into());
            let rev = e.revision;
            e = self.save(e, rev)?;
        }
        Ok(e)
    }
    pub fn backlinks(&self, id: &str) -> Result<Value> {
        let target = self.entry(id)?;
        let title = display_title(&target);
        let entries: HashMap<String, Entry> = rows::<Entry>(&self.db, "entries")?
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        let mut out = vec![];
        for r in rows::<Relation>(&self.db, "relations")? {
            let other = if r.target == id {
                &r.source
            } else if r.source == id && r.origin == "manual" {
                &r.target
            } else {
                continue;
            };
            let Some(src) = entries.get(other) else {
                continue;
            };
            if src.trashed.is_some() {
                continue;
            }
            let snippet = if r.origin == "inline" && !src.hidden {
                block_with_link(&src.body, id, &title)
                    .map(|s| trim_snippet(&s))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            out.push(json!({"source": other, "snippet": snippet, "label": r.label, "origin": r.origin, "reason": r.reason}));
        }
        Ok(json!(out))
    }
    pub fn unlinked_mentions(&self, id: &str) -> Result<Value> {
        let target = self.entry(id)?;
        let title = target.title.trim().to_string();
        if title.chars().count() < 3 {
            return Ok(json!([]));
        }
        let linked: HashSet<String> = rows::<Relation>(&self.db, "relations")?
            .into_iter()
            .filter(|r| r.target == id && r.origin == "inline")
            .map(|r| r.source)
            .collect();
        let phrase = format!("\"{}\"", title.replace('"', "\"\""));
        let mut s = self
            .db
            .prepare("SELECT e.data FROM entry_fts f JOIN entries e ON e.id=f.id WHERE entry_fts MATCH ?1 LIMIT 200")
            .map_err(err)?;
        let candidates = s
            .query_map([format!("body : {phrase}")], |r| r.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut out = vec![];
        for raw in candidates {
            let Ok(e) = serde_json::from_str::<Entry>(&raw) else {
                continue;
            };
            if e.id == id || e.trashed.is_some() || e.hidden || linked.contains(&e.id) {
                continue;
            }
            let mut blocks = vec![];
            prose_blocks(&e.body, &mut blocks);
            if let Some((text, at)) = blocks
                .iter()
                .find_map(|b| find_phrase(b, &title).map(|at| (b, at)))
            {
                out.push(json!({"source": e.id, "snippet": snippet_around(text, at, title.len())}));
            }
            if out.len() >= 30 {
                break;
            }
        }
        Ok(json!(out))
    }
    pub fn link_mention(&mut self, source: &str, target: &str) -> Result<Entry> {
        let t = self.entry(target)?;
        let mut e = self.entry(source)?;
        if !link_first(&mut e.body, t.title.trim(), target) {
            return Err("That mention has changed; nothing was linked.".into());
        }
        let rev = e.revision;
        self.save(e, rev)
    }
    pub fn calendar_month(&self, year: i32, month: u32, offset_minutes: i32) -> Result<Value> {
        let offset = FixedOffset::east_opt(offset_minutes.clamp(-14 * 60, 14 * 60) * 60)
            .ok_or("Invalid time zone offset")?;
        let prefix = format!("{year:04}-{month:02}-");
        // Per day: pages created, pages written to, pages about that day.
        type Day = (Vec<String>, Vec<String>, Vec<String>);
        let mut days: BTreeMap<String, Day> = BTreeMap::new();
        let entries = rows::<Entry>(&self.db, "entries")?;
        let live: HashSet<String> = entries
            .iter()
            .filter(|e| e.trashed.is_none())
            .map(|e| e.id.clone())
            .collect();
        let mut push = |key: Option<String>, slot: usize, id: &str| {
            if let Some(k) = key.filter(|k| k.starts_with(&prefix)) {
                let d = days.entry(k).or_default();
                let list = match slot {
                    0 => &mut d.0,
                    1 => &mut d.1,
                    _ => &mut d.2,
                };
                if !list.iter().any(|x| x == id) && list.len() < 50 {
                    list.push(id.to_string());
                }
            }
        };
        for e in entries.iter().filter(|e| e.trashed.is_none()) {
            push(day_key(&e.created, &offset), 0, &e.id);
            push(day_key(&e.updated, &offset), 1, &e.id);
            if let Some(d) = e.metadata.get("about_date").and_then(Value::as_str) {
                push(Some(d.chars().take(10).collect()), 2, &e.id);
            }
        }
        let mut s = self
            .db
            .prepare("SELECT json_extract(data,'$.entry_id'), json_extract(data,'$.time') FROM revisions")
            .map_err(err)?;
        let revs = s
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(err)?;
        for r in revs {
            let (id, time) = r.map_err(err)?;
            if live.contains(&id) {
                push(day_key(&time, &offset), 1, &id);
            }
        }
        Ok(json!(days
            .into_iter()
            .map(|(k, (made, touched, about))| (
                k,
                json!({"made": made, "touched": touched, "about": about})
            ))
            .collect::<serde_json::Map<_, _>>()))
    }
    pub fn search_v2(
        &self,
        query: &str,
        include_hidden: bool,
        area: Option<String>,
        kind: Option<String>,
        entity: Option<String>,
        after: Option<String>,
    ) -> Result<Value> {
        let terms: Vec<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .take(32)
            .map(|s| format!("\"{}\"*", s.replace('"', "\"\"")))
            .collect();
        let found: Vec<(Entry, String)> = if terms.is_empty() {
            let mut all = rows::<Entry>(&self.db, "entries")?;
            all.sort_by(|a, b| b.updated.cmp(&a.updated));
            all.into_iter().map(|e| (e, String::new())).collect()
        } else {
            let mut s = self.db.prepare("SELECT e.data, snippet(entry_fts, 2, char(2), char(3), '…', 16) FROM entry_fts f JOIN entries e ON e.id=f.id WHERE entry_fts MATCH ?1 ORDER BY bm25(entry_fts) LIMIT 300").map_err(err)?;
            let raw = s
                .query_map([terms.join(" AND ")], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(err)?;
            raw.into_iter()
                .map(|(d, snip)| {
                    serde_json::from_str::<Entry>(&d)
                        .map(|e| (e, snip))
                        .map_err(err)
                })
                .collect::<Result<Vec<_>>>()?
        };
        let out: Vec<Value> = found
            .into_iter()
            .filter(|(e, _)| {
                e.trashed.is_none()
                    && (include_hidden || !e.hidden)
                    && area
                        .as_ref()
                        .map(|a| e.area_ids.contains(a))
                        .unwrap_or(true)
                    && kind.as_ref().map(|k| &e.kind == k).unwrap_or(true)
                    && entity.as_ref().map(|t| &e.entity_type == t).unwrap_or(true)
                    && after.as_ref().map(|a| &e.updated >= a).unwrap_or(true)
            })
            .take(200)
            .map(|(e, snippet)| json!({"entry": e, "snippet": snippet.replace('\n', " ")}))
            .collect();
        Ok(json!(out))
    }
    pub fn kinds_usage(&self) -> Result<Value> {
        let mut m: BTreeMap<String, u64> = BTreeMap::new();
        for e in rows::<Entry>(&self.db, "entries")? {
            if e.trashed.is_none() && !e.kind.is_empty() {
                *m.entry(e.kind).or_default() += 1;
            }
        }
        Ok(json!(m))
    }
    fn kinds(&self) -> Result<Vec<String>> {
        serde_json::from_str(&setting(&self.db, "kinds")?.unwrap_or("[]".into())).map_err(err)
    }
    pub fn rename_kind(&mut self, from: &str, to: &str) -> Result<()> {
        let to = to.trim();
        if to.is_empty() || to.len() > 100 {
            return Err("Use a kind name under 100 characters".into());
        }
        let mut kinds = self.kinds()?;
        if !kinds.iter().any(|k| k == from) {
            return Err("That kind no longer exists".into());
        }
        let tx = self.db.transaction().map_err(err)?;
        if kinds.iter().any(|k| k == to) {
            kinds.retain(|k| k != from);
        } else {
            for k in kinds.iter_mut() {
                if k == from {
                    *k = to.into();
                }
            }
        }
        set_setting(&tx, "kinds", &serde_json::to_string(&kinds).map_err(err)?)?;
        for mut e in rows::<Entry>(&tx, "entries")? {
            if e.kind == from {
                e.kind = to.into();
                e.revision += 1;
                e.updated = now();
                Self::write_entry(&tx, &e)?;
            }
        }
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)
    }
    pub fn delete_kind(&mut self, name: &str) -> Result<()> {
        let mut kinds = self.kinds()?;
        kinds.retain(|k| k != name);
        let tx = self.db.transaction().map_err(err)?;
        set_setting(&tx, "kinds", &serde_json::to_string(&kinds).map_err(err)?)?;
        for mut e in rows::<Entry>(&tx, "entries")? {
            if e.kind == name {
                e.kind = String::new();
                e.revision += 1;
                e.updated = now();
                Self::write_entry(&tx, &e)?;
            }
        }
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)
    }
    pub fn capture(&mut self, text: &str, kind: &str, dest: &str) -> Result<Entry> {
        if text.trim().is_empty() {
            return Err("Write something to keep".into());
        }
        if text.len() > 1_000_000 {
            return Err("That capture is too long; paste it into a page instead".into());
        }
        let (area_ids, about) = match kind {
            "area" => {
                rows::<Area>(&self.db, "areas")?
                    .iter()
                    .find(|a| a.id == dest)
                    .ok_or("That area no longer exists")?;
                (vec![dest.to_string()], None)
            }
            "page" => {
                let t = self.entry(dest)?;
                if t.trashed.is_some() {
                    return Err("That page is in Trash".into());
                }
                (t.area_ids.iter().take(1).cloned().collect(), Some(t.id))
            }
            _ => return Err("Choose where this goes".into()),
        };
        let mut e = self.create(None)?;
        e.body = body_from_text(text);
        e.area_ids = area_ids;
        let rev = e.revision;
        let e = self.save(e, rev)?;
        if let Some(target) = about {
            self.add_relation(e.id.clone(), target, "about".into(), String::new())?;
        }
        Ok(e)
    }
    pub fn backup_status(&self) -> Result<Value> {
        // A chosen folder that has gone (an unplugged drive) is reported, not
        // silently swapped for the local one.
        let chosen = self.preferences().backup_folder;
        let missing = chosen.as_ref().is_some_and(|f| !PathBuf::from(f).is_dir());
        Ok(json!({
            "last": setting(&self.db, "last_auto_backup")?,
            "folder": self.backup_folder(),
            "chosen": chosen,
            "missing": missing,
        }))
    }
    /// A backup cut off by quitting leaves its scratch copy beside the library
    /// and a `.part` bundle in the backups folder. Only those are removed.
    pub fn sweep_interrupted_backups(&self) {
        let ours = |name: &str, prefix: &str, suffix: &str| {
            name.strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(suffix))
                .is_some_and(|rest| uuid::Uuid::parse_str(rest).is_ok())
        };
        if let Ok(list) = fs::read_dir(&self.base) {
            for item in list.flatten() {
                let name = item.file_name().to_string_lossy().to_string();
                if item.path().is_dir() && ours(&name, "export-", "") {
                    let _ = fs::remove_dir_all(item.path());
                }
            }
        }
        for folder in [self.backup_folder(), self.base.join("backups")] {
            if let Ok(list) = fs::read_dir(&folder) {
                for item in list.flatten() {
                    let name = item.file_name().to_string_lossy().to_string();
                    if item.path().is_file() && ours(&name, ".atlas-export-", ".part") {
                        let _ = fs::remove_file(item.path());
                    }
                }
            }
        }
    }
    /// Automatic bundle into the chosen folder, then retention: the newest
    /// bundle of each of the last 14 days and of each of the last 8 ISO weeks.
    pub fn backup_into_folder(&self, time: DateTime<Utc>) -> Result<PathBuf> {
        if self.fail {
            return Err("test backup failure, no bundle written".into());
        }
        let folder = self.backup_folder();
        fs::create_dir_all(&folder).map_err(err)?;
        let destination = folder.join(format!(
            "automatic-{}-{}.atlas.zip",
            time.format("%Y%m%d-%H%M%S"),
            id()
        ));
        self.export_to(&destination)?;
        set_setting(&self.db, "last_auto_backup", &time.to_rfc3339())?;
        set_setting(&self.db, "changed", "false")?;
        retain_backups(&folder, time)?;
        Ok(destination)
    }
    pub fn automatic_backup_v2(&self, time: DateTime<Utc>) -> Result<Option<PathBuf>> {
        if setting(&self.db, "changed")?.as_deref() != Some("true") {
            return Ok(None);
        }
        if let Some(last) = setting(&self.db, "last_auto_backup")? {
            let d = DateTime::parse_from_rfc3339(&last).map_err(err)?;
            if time.signed_duration_since(d).num_seconds() < 86400 {
                return Ok(None);
            }
        }
        self.backup_into_folder(time).map(Some)
    }
    pub fn backup_on_quit(&self) -> Result<Option<PathBuf>> {
        if setting(&self.db, "changed")?.as_deref() != Some("true") {
            return Ok(None);
        }
        self.backup_into_folder(Utc::now()).map(Some)
    }
    /// Revision thinning for one entry (SPEC_V2 Q25). Named and safety
    /// snapshots (automatic = false) are never pruned.
    pub fn thin_history(
        db: &rusqlite::Connection,
        entry_id: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let mut s = db
            .prepare("SELECT id, json_extract(data,'$.time') FROM revisions WHERE json_extract(data,'$.entry_id')=?1 AND json_extract(data,'$.automatic')=1 ORDER BY json_extract(data,'$.time') DESC")
            .map_err(err)?;
        let list = s
            .query_map([entry_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(err)?;
        let mut kept: HashSet<String> = HashSet::new();
        let mut remove = vec![];
        for (rid, time) in list {
            let Ok(t) = DateTime::parse_from_rfc3339(&time) else {
                continue;
            };
            let age = now.signed_duration_since(t.with_timezone(&Utc));
            let bucket = if age < Duration::hours(48) {
                None
            } else if age < Duration::days(30) {
                Some(t.format("h%Y%m%d%H").to_string())
            } else {
                Some(t.format("d%Y%m%d").to_string())
            };
            if let Some(b) = bucket {
                if !kept.insert(b) {
                    remove.push(rid);
                }
            }
        }
        for rid in &remove {
            db.execute("DELETE FROM revisions WHERE id=?1", [rid])
                .map_err(err)?;
        }
        Ok(remove.len())
    }
}

pub fn check_writable(folder: &Path) -> Result<()> {
    if !folder.is_dir() {
        return Err("That backup folder does not exist".into());
    }
    let probe = folder.join(format!(".atlas-write-test-{}", id()));
    fs::write(&probe, b"ok")
        .map_err(|_| "Personal Atlas cannot write to that folder".to_string())?;
    let _ = fs::remove_file(probe);
    Ok(())
}
pub fn retain_backups(folder: &Path, now: DateTime<Utc>) -> Result<()> {
    let mut bundles: Vec<(String, PathBuf, DateTime<Utc>)> = fs::read_dir(folder)
        .map_err(err)?
        .filter_map(|r| r.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !(name.starts_with("automatic-") && name.ends_with(".atlas.zip")) {
                return None;
            }
            let stamp = name.get(10..25)?;
            let t = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()?;
            Some((name, e.path(), t.and_utc()))
        })
        .collect();
    bundles.sort_by_key(|b| std::cmp::Reverse(b.2));
    let mut days = HashSet::new();
    let mut weeks = HashSet::new();
    for (_, path, t) in bundles {
        let age = now.signed_duration_since(t);
        let day = t.format("%Y%m%d").to_string();
        let week = t.format("%G%V").to_string();
        let keep_day = age < Duration::days(14) && days.insert(day);
        let keep_week = age < Duration::weeks(8) && weeks.insert(week);
        if !keep_day && !keep_week {
            fs::remove_file(path).map_err(err)?;
        }
    }
    Ok(())
}
