use crate::model::*;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Repository {
    pub db: Connection,
    pub root: PathBuf,
    pub base: PathBuf,
    pub fail: bool,
    pub delay_ms: u64,
    pub test: bool,
    pub staged: HashMap<String, PathBuf>,
    _lock: fs::File,
}
pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn rows<T: DeserializeOwned>(db: &Connection, table: &str) -> Result<Vec<T>> {
    // Table names are internal constants, never user input.
    let mut s = db
        .prepare(&format!("SELECT data FROM {table} ORDER BY id"))
        .map_err(err)?;
    let raw = s
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(err)?;
    raw.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(err))
        .collect()
}
pub fn put<T: Serialize>(db: &Connection, table: &str, id: &str, item: &T) -> Result<()> {
    db.execute(&format!("INSERT INTO {table}(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data"),params![id,serde_json::to_string(item).map_err(err)?]).map_err(err)?;
    Ok(())
}
pub fn get<T: DeserializeOwned>(db: &Connection, table: &str, id: &str) -> Result<T> {
    let raw: String = db
        .query_row(
            &format!("SELECT data FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .map_err(|_| "Item is missing".to_string())?;
    serde_json::from_str(&raw).map_err(err)
}
pub fn setting(db: &Connection, key: &str) -> Result<Option<String>> {
    db.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
        r.get(0)
    })
    .optional()
    .map_err(err)
}
pub fn set_setting(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value]).map_err(err)?;
    Ok(())
}

impl Repository {
    pub fn open(base: PathBuf, test: bool) -> Result<Self> {
        fs::create_dir_all(&base).map_err(err)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(base.join("library.lock"))
            .map_err(err)?;
        lock.try_lock_exclusive().map_err(|_| {
            "This Atlas is already open. Return to its existing window.".to_string()
        })?;
        let root = if base.join("active-library.txt").exists() {
            let name = fs::read_to_string(base.join("active-library.txt")).map_err(err)?;
            if !name.starts_with("restore-") || name.contains(['/', '\\']) {
                return Err("Invalid active library pointer".into());
            }
            base.join(name)
        } else {
            base.join("library")
        };
        let db = Self::connect(&root)?;
        let mut r = Self {
            db,
            root,
            base,
            fail: false,
            delay_ms: 0,
            test,
            staged: HashMap::new(),
            _lock: lock,
        };
        r.migrate()?;
        r.seed()?;
        r.sweep_ghosts()?;
        Ok(r)
    }
    pub fn schema_version(&self) -> Result<i64> {
        self.db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)
    }
    /// v1 -> v2. A restorable safety bundle is written first; the upgrade runs in
    /// one transaction so a failure leaves the v1 library untouched.
    pub fn migrate(&mut self) -> Result<()> {
        let version = self.schema_version()?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        let folder = self.base.join("backups");
        fs::create_dir_all(&folder).map_err(err)?;
        let safety = folder.join(format!(
            "pre-migration-v2-{}-{}.atlas.zip",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            id()
        ));
        self.export_to(&safety).map_err(|e| {
            format!("Could not write the safety backup before upgrading; nothing changed. {e}")
        })?;
        if self.fail {
            return Err("Test migration failure: the library was not changed.".into());
        }
        let mut c = self.canonical()?;
        upgrade_canonical(&mut c);
        let tx = self.db.transaction().map_err(err)?;
        for a in &c.areas {
            put(&tx, "areas", &a.id, a)?;
        }
        for co in &c.collections {
            put(&tx, "collections", &co.id, co)?;
        }
        for r in &c.relations {
            put(&tx, "relations", &r.id, r)?;
        }
        for e in &c.entries {
            put(&tx, "entries", &e.id, e)?;
        }
        tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))
            .map_err(err)?;
        tx.commit().map_err(err)
    }
    /// Ghost pages hold no writing: only a title typed into a link. Unreferenced,
    /// unwritten ghosts are removed at startup, never during an editing session,
    /// so undo can restore a link without breaking it.
    pub fn sweep_ghosts(&mut self) -> Result<usize> {
        let c = self.canonical()?;
        let mut removed = 0;
        let tx = self.db.transaction().map_err(err)?;
        for e in &c.entries {
            if !is_ghost(e)
                || !plain(&e.body).trim().is_empty()
                || e.pinned
                || c.relations
                    .iter()
                    .any(|r| r.target == e.id || r.source == e.id)
                || c.revisions.iter().any(|r| r.entry_id == e.id)
                || c.attachments.iter().any(|a| a.entry_id == e.id)
                || c.collections.iter().any(|col| col.members.contains(&e.id))
            {
                continue;
            }
            tx.execute("DELETE FROM entry_areas WHERE entry_id=?1", [&e.id])
                .map_err(err)?;
            tx.execute("DELETE FROM entry_fts WHERE id=?1", [&e.id])
                .map_err(err)?;
            tx.execute("DELETE FROM entries WHERE id=?1", [&e.id])
                .map_err(err)?;
            removed += 1;
        }
        tx.commit().map_err(err)?;
        Ok(removed)
    }
    pub fn create_ghost(&mut self, title: String, area_ids: Vec<String>) -> Result<Entry> {
        let title = title.trim().to_string();
        if title.is_empty() || title.len() > 300 {
            return Err("Give the new page a short title".into());
        }
        let mut e = Entry::blank();
        e.title = title;
        e.area_ids = area_ids;
        e.metadata = json!({"ghost": true});
        let tx = self.db.transaction().map_err(err)?;
        Self::write_entry(&tx, &e)?;
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)?;
        Ok(e)
    }
    pub fn connect(root: &Path) -> Result<Connection> {
        fs::create_dir_all(root.join("attachments")).map_err(err)?;
        let db = Connection::open(root.join("atlas.sqlite")).map_err(err)?;
        db.busy_timeout(Duration::from_secs(5)).map_err(err)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )
        .map_err(err)?;
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > SCHEMA_VERSION {
            return Err("This library needs a newer version of Personal Atlas".into());
        }
        let tx = db.unchecked_transaction().map_err(err)?;
        tx.execute_batch(include_str!("schema.sql")).map_err(err)?;
        if setting(&tx, "library_id")?.is_none() {
            // A fresh library starts at the current schema; existing ones keep
            // their recorded version until `migrate` upgrades them.
            tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))
                .map_err(err)?;
            set_setting(&tx, "library_id", &id())?;
            set_setting(
                &tx,
                "kinds",
                &serde_json::to_string(&[
                    "note",
                    "poem",
                    "reflection",
                    "observation",
                    "lesson",
                    "letter",
                    "thought",
                    "feeling",
                    "story",
                    "idea",
                    "question",
                    "plan",
                    "goal-context",
                    "reference",
                ])
                .map_err(err)?,
            )?;
        }
        tx.commit().map_err(err)?;
        let integrity: String = db
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(err)?;
        if integrity != "ok" {
            return Err(
                "Library integrity check failed; preserve files and restore a backup".into(),
            );
        }
        Ok(db)
    }
    pub fn seed(&mut self) -> Result<()> {
        let initialized: bool = self
            .db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM seed_ledger WHERE key='v1')",
                [],
                |r| r.get(0),
            )
            .map_err(err)?;
        if initialized {
            return Ok(());
        }
        let areas: Value =
            serde_json::from_str(include_str!("../seed/life-areas.json")).map_err(err)?;
        let pages: Value =
            serde_json::from_str(include_str!("../seed/starter-pages.json")).map_err(err)?;
        let tx = self.db.transaction().map_err(err)?;
        let mut area_keys = HashMap::new();
        let mut page_keys = HashMap::new();
        for a in areas["areas"].as_array().unwrap() {
            let mut overview = Entry::blank();
            overview.title = a["name"].as_str().unwrap().into();
            overview.body = from_markdown(a["description"].as_str().unwrap());
            overview.metadata = json!({"overview":true});
            Self::write_entry(&tx, &overview)?;
            let area = Area {
                id: id(),
                name: overview.title.clone(),
                description: a["description"].as_str().unwrap().into(),
                parent: None,
                overview: overview.id.clone(),
                order: a["display_order"].as_i64().unwrap(),
                hidden: false,
                seed_key: a["seed_key"].as_str().map(str::to_string),
                call_number: None,
                hue: None,
                pattern: None,
                pinned: false,
            };
            let mut area = area;
            area_defaults(&mut area, &[]);
            area_keys.insert(area.seed_key.clone().unwrap(), area.id.clone());
            put(&tx, "areas", &area.id, &area)?;
        }
        for p in pages["pages"].as_array().unwrap() {
            // Updated visual brief: fresh libraries contain structural entrances
            // and the approved editable philosophy only. The v1 ledger protects
            // every existing library from reseeding or cleanup on upgrade.
            if !p["seed_key"].as_str().unwrap_or("").starts_with("meta-") {
                continue;
            }
            let mut e = Entry::blank();
            e.title = p["title"].as_str().unwrap().into();
            e.kind = p["kind"].as_str().unwrap_or("").into();
            e.entity_type = p["entity_type"].as_str().unwrap().into();
            e.body = from_markdown(p["body_markdown"].as_str().unwrap());
            e.seed_key = p["seed_key"].as_str().map(str::to_string);
            e.pinned = p["pin_on_home"].as_bool().unwrap();
            e.placeholder = p["editor_placeholder"]
                .as_str()
                .unwrap_or("Begin wherever you are…")
                .into();
            if p["initial_editor_block"] == "poetry" {
                e.body = json!({"type":"doc","content":[{"type":"poetry"}]});
            }
            e.area_ids = p["area_seed_keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| area_keys[s.as_str().unwrap()].clone())
                .collect();
            page_keys.insert(e.seed_key.clone().unwrap(), e.id.clone());
            Self::write_entry(&tx, &e)?;
        }
        for c in pages["collections"].as_array().unwrap() {
            let mut intro = Entry::blank();
            intro.title = c["title"].as_str().unwrap().into();
            intro.body = from_markdown(c["description"].as_str().unwrap());
            intro.metadata = json!({"collection_introduction":true});
            Self::write_entry(&tx, &intro)?;
            let col = Collection {
                id: id(),
                title: intro.title.clone(),
                introduction: intro.id,
                members: c["member_seed_keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| page_keys[v.as_str().unwrap()].clone())
                    .collect(),
                seed_key: c["seed_key"].as_str().map(str::to_string),
                pinned: false,
            };
            put(&tx, "collections", &col.id, &col)?;
        }
        for r in pages["relations"].as_array().unwrap() {
            if !page_keys.contains_key(r["source_seed_key"].as_str().unwrap())
                || !page_keys.contains_key(r["target_seed_key"].as_str().unwrap())
            {
                continue;
            }
            let rel = Relation {
                id: id(),
                source: page_keys[r["source_seed_key"].as_str().unwrap()].clone(),
                target: page_keys[r["target_seed_key"].as_str().unwrap()].clone(),
                label: r["label"].as_str().unwrap().into(),
                reason: String::new(),
                origin: "manual".into(),
                created: Some(now()),
            };
            put(&tx, "relations", &rel.id, &rel)?;
        }
        tx.execute("INSERT INTO seed_ledger(key) VALUES('v1')", [])
            .map_err(err)?;
        tx.commit().map_err(err)
    }
    pub fn untouched_starters(&self) -> Result<Vec<Entry>> {
        let c = self.canonical()?;
        let seeds: Value =
            serde_json::from_str(include_str!("../seed/starter-pages.json")).map_err(err)?;
        let mut candidates = Vec::new();
        for e in &c.entries {
            let Some(key) = &e.seed_key else {
                continue;
            };
            if key.starts_with("meta-")
                || e.hidden
                || e.trashed.is_some()
                || e.revision != 0
                || e.created != e.updated
                || e.metadata != json!({})
                || e.rediscovery
                || c.attachments.iter().any(|a| a.entry_id == e.id)
                || c.revisions.iter().any(|r| r.entry_id == e.id)
                || c.relations
                    .iter()
                    .any(|r| r.source == e.id || r.target == e.id)
                || c.collections
                    .iter()
                    .any(|col| col.members.contains(&e.id) || col.introduction == e.id)
                || c.areas.iter().any(|a| a.overview == e.id)
            {
                continue;
            }
            let Some(p) = seeds["pages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["seed_key"] == *key)
            else {
                continue;
            };
            let expected_body = if p["initial_editor_block"] == "poetry" {
                json!({"type":"doc","content":[{"type":"poetry"}]})
            } else {
                from_markdown(p["body_markdown"].as_str().unwrap())
            };
            let expected_areas: Option<Vec<String>> = p["area_seed_keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| {
                    c.areas
                        .iter()
                        .find(|a| a.seed_key.as_deref() == k.as_str())
                        .map(|a| a.id.clone())
                })
                .collect();
            if e.title == p["title"].as_str().unwrap()
                && e.kind == p["kind"].as_str().unwrap_or("")
                && e.entity_type == p["entity_type"].as_str().unwrap()
                && e.body == expected_body
                && e.pinned == p["pin_on_home"].as_bool().unwrap()
                && e.placeholder
                    == p["editor_placeholder"]
                        .as_str()
                        .unwrap_or("Begin wherever you are…")
                && expected_areas.as_ref() == Some(&e.area_ids)
            {
                candidates.push(e.clone());
            }
        }
        Ok(candidates)
    }
    pub fn trash_starters(&mut self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let eligible = self.untouched_starters()?;
        let mut entries = Vec::new();
        for id in ids {
            entries.push(eligible.iter().find(|e| &e.id == id).ok_or("A selected page is no longer demonstrably untouched. Review again; no pages moved.")?.clone());
        }
        let tx = self.db.transaction().map_err(err)?;
        for mut e in entries {
            let r = Revision {
                id: id(),
                entry_id: e.id.clone(),
                time: now(),
                label: "Before moving to Trash".into(),
                automatic: false,
                entry: e.clone(),
            };
            put(&tx, "revisions", &r.id, &r)?;
            e.trashed = Some(now());
            e.updated = now();
            e.revision += 1;
            Self::write_entry(&tx, &e)?;
        }
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)
    }
    pub fn write_entry(db: &Connection, e: &Entry) -> Result<()> {
        validate_body(&e.body, 0)?;
        put(db, "entries", &e.id, e)?;
        db.execute("DELETE FROM entry_areas WHERE entry_id=?1", [&e.id])
            .map_err(err)?;
        for a in &e.area_ids {
            db.execute("INSERT INTO entry_areas VALUES(?1,?2)", params![e.id, a])
                .map_err(err)?;
        }
        db.execute("DELETE FROM entry_fts WHERE id=?1", [&e.id])
            .map_err(err)?;
        db.execute(
            "INSERT INTO entry_fts(id,title,body) VALUES(?1,?2,?3)",
            params![e.id, e.title, plain(&e.body)],
        )
        .map_err(err)?;
        let mut previous: HashMap<String, String> = HashMap::new();
        {
            let mut s = db.prepare("SELECT json_extract(data,'$.target'), json_extract(data,'$.created') FROM relations WHERE json_extract(data,'$.source')=?1 AND json_extract(data,'$.origin')='inline'").map_err(err)?;
            let found = s
                .query_map([&e.id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })
                .map_err(err)?;
            for row in found {
                let (target, created) = row.map_err(err)?;
                if let Some(c) = created {
                    previous.insert(target, c);
                }
            }
        }
        db.execute("DELETE FROM relations WHERE json_extract(data,'$.source')=?1 AND json_extract(data,'$.origin')='inline'",[&e.id]).map_err(err)?;
        let mut refs = vec![];
        references(&e.body, &mut refs);
        refs.sort();
        refs.dedup();
        for target in refs {
            let rel = Relation {
                id: format!("inline-{}-{}", e.id, target),
                source: e.id.clone(),
                created: Some(previous.get(&target).cloned().unwrap_or_else(now)),
                target,
                label: "links to".into(),
                reason: String::new(),
                origin: "inline".into(),
            };
            put(db, "relations", &rel.id, &rel)?;
        }
        Ok(())
    }
    pub fn entry(&self, id: &str) -> Result<Entry> {
        get(&self.db, "entries", id)
    }
    pub fn save(&mut self, mut e: Entry, expected: u64) -> Result<Entry> {
        if self.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.delay_ms));
        }
        if self.fail {
            return Err(
                "Test write failure: no transaction committed. Your changes remain unsaved.".into(),
            );
        }
        if serde_json::to_vec(&e.body).map_err(err)?.len() > 10 * 1024 * 1024 {
            return Err("Page exceeds the 10 MB document limit".into());
        }
        if e.title.len() > 1000
            || e.kind.len() > 100
            || e.area_ids.len() > 500
            || !e.metadata.is_object()
        {
            return Err("Invalid page metadata".into());
        }
        let old = self.entry(&e.id)?;
        if old.revision != expected {
            return Err("Revision conflict: this page changed. Keep your writing and reopen before retrying.".into());
        }
        e.created = old.created;
        e.seed_key = old.seed_key;
        if is_ghost(&e) && !plain(&e.body).trim().is_empty() {
            if let Some(m) = e.metadata.as_object_mut() {
                m.remove("ghost");
            }
        }
        e.revision = expected + 1;
        e.updated = now();
        let last:Option<String>=self.db.query_row("SELECT json_extract(data,'$.time') FROM revisions WHERE json_extract(data,'$.entry_id')=?1 ORDER BY json_extract(data,'$.time') DESC LIMIT 1",[&e.id],|r|r.get(0)).optional().map_err(err)?;
        let checkpoint = last
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| chrono::Utc::now().signed_duration_since(d).num_seconds() >= 300)
            .unwrap_or(true);
        let tx = self.db.transaction().map_err(err)?;
        Self::write_entry(&tx, &e)?;
        set_setting(&tx, "changed", "true")?;
        if checkpoint {
            let previous:Option<String>=tx.query_row("SELECT data FROM revisions WHERE json_extract(data,'$.entry_id')=?1 ORDER BY json_extract(data,'$.time') DESC LIMIT 1",[&e.id],|r|r.get(0)).optional().map_err(err)?;
            let changed = previous
                .map(|p| {
                    serde_json::from_str::<Revision>(&p)
                        .map(|r| {
                            r.entry.body != e.body
                                || r.entry.title != e.title
                                || r.entry.metadata != e.metadata
                        })
                        .map_err(err)
                })
                .transpose()?
                .unwrap_or(true);
            if changed {
                let r = Revision {
                    id: id(),
                    entry_id: e.id.clone(),
                    time: now(),
                    label: String::new(),
                    automatic: true,
                    entry: e.clone(),
                };
                put(&tx, "revisions", &r.id, &r)?;
                Self::thin_history(&tx, &e.id, chrono::Utc::now())?;
            }
        }
        tx.commit().map_err(err)?;
        Ok(e)
    }
    pub fn create(&mut self, entity_type: Option<String>) -> Result<Entry> {
        let mut e = Entry::blank();
        if let Some(t) = entity_type {
            if !["note", "person", "place", "project", "experience", "theme"].contains(&t.as_str())
            {
                return Err("Unknown page type".into());
            }
            e.entity_type = t;
        }
        let tx = self.db.transaction().map_err(err)?;
        Self::write_entry(&tx, &e)?;
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)?;
        Ok(e)
    }
    pub fn canonical(&self) -> Result<Canonical> {
        Self::canonical_from(&self.db)
    }
    pub fn canonical_from(db: &Connection) -> Result<Canonical> {
        let mut ledger = db
            .prepare("SELECT key FROM seed_ledger ORDER BY key")
            .map_err(err)?;
        let seed_ledger = ledger
            .query_map([], |r| r.get(0))
            .map_err(err)?
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(err)?;
        Ok(Canonical {
            format_version: SCHEMA_VERSION as u32,
            schema_version: SCHEMA_VERSION as u32,
            library_id: setting(db, "library_id")?.ok_or("Missing library identity")?,
            entries: rows(db, "entries")?,
            areas: rows(db, "areas")?,
            collections: rows(db, "collections")?,
            relations: rows(db, "relations")?,
            attachments: rows(db, "attachments")?,
            revisions: rows(db, "revisions")?,
            kinds: serde_json::from_str(&setting(db, "kinds")?.unwrap_or("[]".into()))
                .map_err(err)?,
            seed_ledger,
            presentation: PRESENTATION_KEYS
                .iter()
                .filter_map(|k| setting(db, k).ok().flatten().map(|v| (k.to_string(), v)))
                .collect(),
        })
    }
    pub fn snapshot(
        &mut self,
        entry_id: &str,
        label: &str,
        automatic: bool,
    ) -> Result<Option<Revision>> {
        let e = self.entry(entry_id)?;
        let previous = self.history(entry_id)?;
        if automatic
            && previous
                .first()
                .map(|r| {
                    r.entry.body == e.body
                        && r.entry.title == e.title
                        && r.entry.metadata == e.metadata
                })
                .unwrap_or(false)
        {
            return Ok(None);
        }
        let r = Revision {
            id: id(),
            entry_id: entry_id.into(),
            time: now(),
            label: label.chars().take(200).collect(),
            automatic,
            entry: e,
        };
        let tx = self.db.transaction().map_err(err)?;
        put(&tx, "revisions", &r.id, &r)?;
        Self::thin_history(&tx, entry_id, chrono::Utc::now())?;
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)?;
        Ok(Some(r))
    }
    pub fn history(&self, entry_id: &str) -> Result<Vec<Revision>> {
        let mut r: Vec<Revision> = rows(&self.db, "revisions")?;
        r.retain(|r| r.entry_id == entry_id);
        r.sort_by(|a, b| b.time.cmp(&a.time));
        Ok(r)
    }
    pub fn restore_version(&mut self, revision_id: &str) -> Result<Entry> {
        let r: Revision = get(&self.db, "revisions", revision_id)?;
        let mut e = self.entry(&r.entry_id)?;
        self.snapshot(&e.id, "Before restoring a version", false)?;
        e.body = r.entry.body;
        e.title = r.entry.title;
        e.metadata = r.entry.metadata;
        e.kind = r.entry.kind;
        let rev = e.revision;
        self.save(e, rev)
    }
    pub fn upsert_area(&mut self, a: Area) -> Result<()> {
        if a.name.trim().is_empty() || a.name.len() > 200 {
            return Err("Give this area a name under 200 characters".into());
        }
        self.entry(&a.overview)?;
        let areas: Vec<Area> = rows(&self.db, "areas")?;
        let mut next = a.parent.clone();
        let mut seen = HashSet::new();
        seen.insert(a.id.clone());
        while let Some(n) = next {
            if !seen.insert(n.clone()) {
                return Err("An area cannot contain itself or one of its ancestors".into());
            }
            next = areas
                .iter()
                .find(|x| x.id == n)
                .ok_or("Parent area is missing")?
                .parent
                .clone();
        }
        put(&self.db, "areas", &a.id, &a)?;
        set_setting(&self.db, "changed", "true")
    }
    pub fn delete_area(&mut self, id: &str) -> Result<()> {
        let tx = self.db.transaction().map_err(err)?;
        for mut e in rows::<Entry>(&tx, "entries")? {
            if e.area_ids.contains(&id.to_string()) {
                e.area_ids.retain(|a| a != id);
                e.revision += 1;
                e.updated = now();
                Self::write_entry(&tx, &e)?;
            }
        }
        for mut a in rows::<Area>(&tx, "areas")? {
            if a.parent.as_deref() == Some(id) {
                a.parent = None;
                put(&tx, "areas", &a.id, &a)?;
            }
        }
        tx.execute("DELETE FROM entry_areas WHERE area_id=?1", [id])
            .map_err(err)?;
        tx.execute("DELETE FROM areas WHERE id=?1", [id])
            .map_err(err)?;
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)
    }
    pub fn upsert_collection(&mut self, c: Collection) -> Result<()> {
        self.entry(&c.introduction)?;
        if c.title.trim().is_empty() {
            return Err("Give this collection a title".into());
        }
        let mut seen = HashSet::new();
        for member in &c.members {
            self.entry(member)?;
            if !seen.insert(member) {
                return Err("Collection members must be unique".into());
            }
        }
        put(&self.db, "collections", &c.id, &c)?;
        set_setting(&self.db, "changed", "true")
    }
    pub fn add_relation(
        &mut self,
        source: String,
        target: String,
        label: String,
        reason: String,
    ) -> Result<Relation> {
        self.entry(&source)?;
        self.entry(&target)?;
        if label.trim().is_empty() || label.len() > 100 || reason.len() > 2000 {
            return Err("Use a short label and reason".into());
        }
        let r = Relation {
            id: id(),
            source,
            target,
            label,
            reason,
            origin: "manual".into(),
            created: Some(now()),
        };
        put(&self.db, "relations", &r.id, &r)?;
        set_setting(&self.db, "changed", "true")?;
        Ok(r)
    }
    pub fn trash(&mut self, id: &str, restore: bool) -> Result<Entry> {
        let mut e = self.entry(id)?;
        if !restore {
            self.snapshot(id, "Before moving to Trash", false)?;
        }
        e.trashed = if restore { None } else { Some(now()) };
        let revision = e.revision;
        self.save(e, revision)
    }
    pub fn rebuild(root: &Path, c: &Canonical) -> Result<Connection> {
        Self::validate(c)?;
        let mut c = c.clone();
        upgrade_canonical(&mut c);
        let c = &c;
        let mut db = Self::connect(root)?;
        let tx = db.transaction().map_err(err)?;
        for t in [
            "entry_areas",
            "entries",
            "areas",
            "collections",
            "relations",
            "attachments",
            "revisions",
            "entry_fts",
            "seed_ledger",
        ] {
            tx.execute(&format!("DELETE FROM {t}"), []).map_err(err)?;
        }
        for a in &c.areas {
            put(&tx, "areas", &a.id, a)?;
        }
        for e in &c.entries {
            Self::write_entry(&tx, e)?;
        }
        for co in &c.collections {
            put(&tx, "collections", &co.id, co)?;
        }
        for r in &c.relations {
            put(&tx, "relations", &r.id, r)?;
        }
        for a in &c.attachments {
            put(&tx, "attachments", &a.id, a)?;
        }
        for r in &c.revisions {
            put(&tx, "revisions", &r.id, r)?;
        }
        for s in &c.seed_ledger {
            tx.execute("INSERT INTO seed_ledger VALUES(?1)", [s])
                .map_err(err)?;
        }
        set_setting(&tx, "library_id", &c.library_id)?;
        set_setting(&tx, "kinds", &serde_json::to_string(&c.kinds).map_err(err)?)?;
        for (k, v) in &c.presentation {
            if PRESENTATION_KEYS.contains(&k.as_str()) {
                set_setting(&tx, k, v)?;
            }
        }
        set_setting(&tx, "changed", "false")?;
        tx.commit().map_err(err)?;
        Ok(db)
    }
    pub fn validate(c: &Canonical) -> Result<()> {
        if !(1..=SCHEMA_VERSION as u32).contains(&c.format_version)
            || !(1..=SCHEMA_VERSION as u32).contains(&c.schema_version)
        {
            return Err("Unsupported Atlas format or schema version".into());
        }
        let mut all = HashSet::new();
        for e in &c.entries {
            if uuid::Uuid::parse_str(&e.id).is_err() || !all.insert(e.id.clone()) {
                return Err("Duplicate or invalid page ID".into());
            }
            validate_body(&e.body, 0)?;
        }
        let mut area_ids = HashSet::new();
        for a in &c.areas {
            if !area_ids.insert(a.id.clone()) || !all.contains(&a.overview) {
                return Err("Invalid area or overview reference".into());
            }
        }
        for e in &c.entries {
            for a in &e.area_ids {
                if !area_ids.contains(a) {
                    return Err("Broken area membership".into());
                }
            }
        }
        for a in &c.areas {
            let mut p = a.parent.clone();
            let mut seen = HashSet::new();
            seen.insert(a.id.clone());
            while let Some(n) = p {
                if !seen.insert(n.clone()) {
                    return Err("Area cycle in backup".into());
                }
                p = c
                    .areas
                    .iter()
                    .find(|a| a.id == n)
                    .ok_or("Missing parent area")?
                    .parent
                    .clone();
            }
        }
        for co in &c.collections {
            if !all.contains(&co.introduction) || co.members.iter().any(|m| !all.contains(m)) {
                return Err("Broken collection reference".into());
            }
        }
        for r in &c.relations {
            if !all.contains(&r.source) || !all.contains(&r.target) {
                return Err("Broken connection reference".into());
            }
        }
        let mut attachment_ids = HashSet::new();
        for a in &c.attachments {
            if !all.contains(&a.entry_id)
                || !attachment_ids.insert(a.id.clone())
                || a.relative_path != format!("attachments/{}", a.id)
                || uuid::Uuid::parse_str(&a.id).is_err()
            {
                return Err("Invalid attachment record".into());
            }
        }
        fn check_media(v: &Value, ids: &HashSet<String>) -> Result<()> {
            if ["atlasImage", "attachment"].contains(&v["type"].as_str().unwrap_or(""))
                && !ids.contains(v["attrs"]["attachmentId"].as_str().unwrap_or(""))
            {
                return Err("Broken document attachment reference".into());
            }
            if let Some(a) = v["content"].as_array() {
                for n in a {
                    check_media(n, ids)?;
                }
            }
            Ok(())
        }
        for e in &c.entries {
            check_media(&e.body, &attachment_ids)?;
            let mut r = vec![];
            references(&e.body, &mut r);
            if r.iter().any(|id| !all.contains(id)) {
                return Err("Broken inline link reference".into());
            }
        }
        for r in &c.revisions {
            if !all.contains(&r.entry_id) || r.entry.id != r.entry_id {
                return Err("Broken history reference".into());
            }
            validate_body(&r.entry.body, 0)?;
            check_media(&r.entry.body, &attachment_ids)?;
        }
        Ok(())
    }
}

pub const SCHEMA_VERSION: i64 = 2;
pub fn is_ghost(e: &Entry) -> bool {
    e.metadata.get("ghost").and_then(Value::as_bool) == Some(true)
}
const SEED_LOOK: [(&str, &str, &str, &str); 14] = [
    ("inner-life", "100", "lilac", "plain"),
    ("health", "150", "sage", "plain"),
    ("relationships", "200", "rose", "plain"),
    ("roots", "250", "peach", "dots"),
    ("academia", "300", "sky", "plain"),
    ("career", "350", "butter", "dots"),
    ("making", "400", "peach", "plain"),
    ("arts", "450", "rose", "dots"),
    ("money", "500", "mint", "plain"),
    ("everyday", "550", "butter", "plain"),
    ("travel", "600", "sky", "dots"),
    ("play", "650", "mint", "dots"),
    ("community", "700", "sage", "dots"),
    ("internet-bible", "750", "lilac", "dots"),
];
/// Fill presentation defaults (call number, hue, pattern) for one area, given
/// the areas already settled. Never overwrites a value the owner chose.
pub fn area_defaults(a: &mut Area, others: &[Area]) {
    let seeded = a
        .seed_key
        .as_deref()
        .and_then(|k| SEED_LOOK.iter().find(|s| s.0 == k));
    if a.call_number.is_none() {
        let taken: HashSet<String> = others
            .iter()
            .filter_map(|o| o.call_number.clone())
            .collect();
        a.call_number = match seeded {
            Some(s) if !taken.contains(s.1) => Some(s.1.into()),
            _ => {
                let parent = a
                    .parent
                    .as_ref()
                    .and_then(|p| others.iter().find(|o| &o.id == p))
                    .and_then(|o| o.call_number.as_ref())
                    .and_then(|n| n.split('.').next().and_then(|n| n.parse::<u32>().ok()));
                match parent {
                    Some(base) => (base + 1..base + 50)
                        .map(|n| n.to_string())
                        .find(|n| !taken.contains(n)),
                    None => (1..200)
                        .map(|n| (n * 50).to_string())
                        .find(|n| !taken.contains(n)),
                }
            }
        };
    }
    if a.hue.is_none() || a.pattern.is_none() {
        let (hue, pattern) = match seeded {
            Some(s) => (s.2.to_string(), s.3.to_string()),
            None => {
                let used: HashSet<(String, String)> = others
                    .iter()
                    .filter_map(|o| Some((o.hue.clone()?, o.pattern.clone()?)))
                    .collect();
                let mut options = vec![];
                for p in ["plain", "dots"] {
                    for h in TONES {
                        options.push((h.to_string(), p.to_string()));
                    }
                }
                let fallback =
                    options[a.id.bytes().map(usize::from).sum::<usize>() % options.len()].clone();
                options
                    .into_iter()
                    .find(|o| !used.contains(o))
                    .unwrap_or(fallback)
            }
        };
        a.hue.get_or_insert(hue);
        a.pattern.get_or_insert(pattern);
    }
}
/// Bring a canonical v1 snapshot to v2 shape. Idempotent.
pub fn upgrade_canonical(c: &mut Canonical) {
    let mut settled: Vec<Area> = vec![];
    let mut order: Vec<usize> = (0..c.areas.len()).collect();
    // Seeded and top-level areas first so branches number from their parent.
    order.sort_by_key(|&i| {
        (
            c.areas[i].parent.is_some(),
            c.areas[i].seed_key.is_none(),
            c.areas[i].order,
        )
    });
    for i in order {
        let mut a = c.areas[i].clone();
        area_defaults(&mut a, &settled);
        c.areas[i] = a.clone();
        settled.push(a);
    }
    let created: HashMap<String, String> = c
        .entries
        .iter()
        .map(|e| (e.id.clone(), e.created.clone()))
        .collect();
    for r in &mut c.relations {
        if r.created.is_none() {
            let a = created.get(&r.source).cloned().unwrap_or_default();
            let b = created.get(&r.target).cloned().unwrap_or_default();
            r.created = Some(if a > b { a } else { b }).filter(|s| !s.is_empty());
        }
    }
    for e in &mut c.entries {
        if e.metadata.get("circle").is_some() {
            continue;
        }
        let circle = match e.metadata.get("relationship").and_then(Value::as_str) {
            Some("know personally") | Some("have contacted") => Some("know"),
            Some("would like to contact") => Some("want_to_meet"),
            Some("follow their work") => Some("admire"),
            _ => None,
        };
        if let (Some(circle), Some(m)) = (circle, e.metadata.as_object_mut()) {
            m.insert("circle".into(), json!(circle));
        }
    }
    c.format_version = SCHEMA_VERSION as u32;
    c.schema_version = SCHEMA_VERSION as u32;
}

pub const PRESENTATION_KEYS: [&str; 3] = ["layout", "shelf", "show_call_numbers"];
impl Repository {
    pub fn presentation(&self) -> Result<Value> {
        let read = |k: &str, d: Value| -> Result<Value> {
            Ok(setting(&self.db, k)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(d))
        };
        Ok(json!({
            "layout": read("layout", json!({}))?,
            "shelf": read("shelf", json!([]))?,
            "show_call_numbers": read("show_call_numbers", json!(true))?,
        }))
    }
    pub fn set_presentation(&mut self, key: &str, value: Value) -> Result<()> {
        let ok = match key {
            "layout" => value.is_object(),
            "shelf" => value
                .as_array()
                .map(|a| {
                    a.len() <= 200
                        && a.iter().all(|v| {
                            v.as_str()
                                .map(|s| uuid::Uuid::parse_str(s).is_ok())
                                .unwrap_or(false)
                        })
                })
                .unwrap_or(false),
            "show_call_numbers" => value.is_boolean(),
            _ => false,
        };
        let text = serde_json::to_string(&value).map_err(err)?;
        if !ok || text.len() > 4 * 1024 * 1024 {
            return Err("Unsupported presentation setting".into());
        }
        set_setting(&self.db, key, &text)?;
        set_setting(&self.db, "changed", "true")
    }
}
