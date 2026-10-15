use crate::{
    model::*,
    repository::{err, get, put, set_setting, Repository},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_FILE: u64 = 100 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 2 * 1024 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct FileRecord {
    size: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    schema_version: u32,
    app_version: String,
    library_id: String,
    created: String,
    files: BTreeMap<String, FileRecord>,
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn safe_name(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\'])
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

impl Repository {
    pub fn export_page_to(&self, entry_id: &str, format: &str, path: &Path) -> Result<()> {
        let entry = self.entry(entry_id)?;
        if path.exists() {
            return Err("Choose a new filename to preserve the existing file".into());
        }
        let mut body = match format {
            "markdown" | "md" => markdown(&entry.body),
            "text" | "txt" => plain(&entry.body),
            _ => return Err("Choose Markdown or plain text export".into()),
        };
        let originals = crate::repository::rows::<Attachment>(&self.db, "attachments")?
            .into_iter()
            .filter(|a| a.entry_id == entry_id)
            .collect::<Vec<_>>();
        let assets_name = format!("{}-assets", entry.id);
        let folder = path.parent().ok_or("No export folder")?.join(&assets_name);
        if !originals.is_empty() {
            fs::create_dir_all(&folder).map_err(err)?;
            for a in originals {
                let (_, bytes) = self.attachment_bytes(&a.id)?;
                let destination = folder.join(&a.id);
                if destination.exists() {
                    if hash(&fs::read(&destination).map_err(err)?) != a.sha256 {
                        return Err("An export asset with different contents exists".into());
                    }
                } else {
                    fs::write(&destination, bytes).map_err(err)?;
                }
                body = body.replace(
                    &format!("../attachments/{}", a.id),
                    &format!("{assets_name}/{}", a.id),
                );
            }
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(err)?;
        file.write_all(body.as_bytes()).map_err(err)?;
        file.sync_all().map_err(err)
    }
    pub fn import_attachment(
        &mut self,
        entry_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> Result<Attachment> {
        self.entry(entry_id)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_FILE {
            return Err("Choose a non-empty attachment up to 100 MB".into());
        }
        let name = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Invalid file name")?
            .chars()
            .filter(|c| !c.is_control())
            .take(255)
            .collect::<String>();
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF8") {
            "image/gif"
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            "image/webp"
        } else {
            "application/octet-stream"
        };
        let a = Attachment {
            id: id(),
            entry_id: entry_id.into(),
            name,
            relative_path: String::new(),
            mime: mime.into(),
            size: bytes.len() as u64,
            sha256: hash(bytes),
        };
        let a = Attachment {
            relative_path: format!("attachments/{}", a.id),
            ..a
        };
        let path = self.root.join(&a.relative_path);
        let temp = path.with_extension("part");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(err)?;
        file.write_all(bytes).map_err(err)?;
        file.sync_all().map_err(err)?;
        drop(file);
        fs::rename(&temp, &path).map_err(err)?;
        let tx = self.db.transaction().map_err(err)?;
        put(&tx, "attachments", &a.id, &a)?;
        set_setting(&tx, "changed", "true")?;
        tx.commit().map_err(err)?;
        Ok(a)
    }
    pub fn attachment_bytes(&self, attachment_id: &str) -> Result<(Attachment, Vec<u8>)> {
        let a: Attachment = get(&self.db, "attachments", attachment_id)?;
        if a.relative_path != format!("attachments/{}", a.id)
            || uuid::Uuid::parse_str(&a.id).is_err()
        {
            return Err("Invalid managed attachment path".into());
        }
        let bytes = fs::read(self.root.join(&a.relative_path)).map_err(|_| {
            "Local attachment is unavailable. Restore it from a complete backup.".to_string()
        })?;
        if bytes.len() as u64 != a.size || hash(&bytes) != a.sha256 {
            return Err("Local attachment is corrupt; restore a backup".into());
        }
        Ok((a, bytes))
    }
    pub fn attachment_data(&self, id: &str) -> Result<String> {
        let (a, bytes) = self.attachment_bytes(id)?;
        if !a.mime.starts_with("image/") {
            return Err("This file has no supported image preview".into());
        }
        Ok(format!("data:{};base64,{}", a.mime, STANDARD.encode(bytes)))
    }
    pub fn export_to(&self, destination: &Path) -> Result<Value> {
        if destination.exists() {
            return Err(
                "A backup already exists at this path. Choose a new name to preserve it.".into(),
            );
        }
        let parent = destination.parent().ok_or("No export folder")?;
        fs::create_dir_all(parent).map_err(err)?;
        let scratch = self.base.join(format!("export-{}", id()));
        fs::create_dir_all(&scratch).map_err(err)?;
        let snapshot_path = scratch.join("atlas.sqlite");
        let operation = (|| {
            // SQLite online backup includes committed WAL pages. All commands share one repository lock.
            self.db.backup("main", &snapshot_path, None).map_err(err)?;
            let snap = rusqlite::Connection::open(&snapshot_path).map_err(err)?;
            let c = Self::canonical_from(&snap)?;
            Self::validate(&c)?;
            let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            files.insert(
                "atlas.sqlite".into(),
                fs::read(&snapshot_path).map_err(err)?,
            );
            files.insert(
                "canonical.json".into(),
                serde_json::to_vec_pretty(&c).map_err(err)?,
            );
            files.insert("README.txt".into(),b"Personal Atlas portable bundle, format 2 (format 1 bundles remain restorable). canonical.json plus attachments is the reconstruction source; atlas.sqlite is a consistent online-backup snapshot. Restore validates hashes and rebuilds a separate staged library from JSON, without executing supplied SQL. Entries keep UUID identity; pages/UUID.md links resolve to the corresponding exported pages. Poetry text files preserve exact whitespace, including trailing newlines. Markdown is readable interchange; formatting and custom nodes may differ in other tools. Image/file links point to ../attachments/UUID (original immutable bytes; original names and MIME types in canonical.json). ALL entries, hidden pages, Trash, retained history and historical originals are included. This bundle and library are NOT application-encrypted. Restoring selects a backed-up state and does not merge timelines. No attachment garbage collection or automatic Trash purge in v1.\n".to_vec());
            for e in &c.entries {
                let md = format!(
                    "<!-- atlas-id: {} ; kind: {} ; entity: {} -->\n# {}\n\n{}",
                    e.id,
                    e.kind,
                    e.entity_type,
                    e.title.replace('\n', " "),
                    markdown(&e.body)
                );
                files.insert(format!("pages/{}.md", e.id), md.into_bytes());
                files.insert(format!("pages/{}.txt", e.id), plain(&e.body).into_bytes());
                let mut poems = vec![];
                poetry(&e.body, &mut poems);
                for (i, p) in poems.into_iter().enumerate() {
                    files.insert(format!("poetry/{}-{i}.txt", e.id), p.into_bytes());
                }
            }
            for a in &c.attachments {
                let (_, bytes) = self.attachment_bytes(&a.id)?;
                files.insert(a.relative_path.clone(), bytes);
            }
            let manifest = Manifest {
                format_version: 1,
                schema_version: 1,
                app_version: env!("CARGO_PKG_VERSION").into(),
                library_id: c.library_id.clone(),
                created: now(),
                files: files
                    .iter()
                    .map(|(p, b)| {
                        (
                            p.clone(),
                            FileRecord {
                                size: b.len() as u64,
                                sha256: hash(b),
                            },
                        )
                    })
                    .collect(),
            };
            let temp = parent.join(format!(".atlas-export-{}.part", id()));
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(err)?;
            let mut writer = ZipWriter::new(file);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o600);
            for (name, bytes) in &files {
                writer.start_file(name, options).map_err(err)?;
                writer.write_all(bytes).map_err(err)?;
            }
            writer.start_file("manifest.json", options).map_err(err)?;
            writer
                .write_all(&serde_json::to_vec_pretty(&manifest).map_err(err)?)
                .map_err(err)?;
            let file = writer.finish().map_err(err)?;
            file.sync_all().map_err(err)?;
            drop(file);
            let validated = Self::validate_archive(&temp)?;
            if validated.0 != c {
                return Err("Export validation changed canonical data".into());
            }
            fs::rename(&temp, destination).map_err(err)?;
            Ok(
                json!({"path":destination,"entries":c.entries.len(),"sha256":hash(&fs::read(destination).map_err(err)?)}),
            )
        })();
        // Only remove our unique scratch folder, never the destination or library.
        let _ = fs::remove_dir_all(&scratch);
        operation
    }
    pub fn validate_archive(path: &Path) -> Result<(Canonical, BTreeMap<String, Vec<u8>>)> {
        if fs::metadata(path).map_err(err)?.len() > MAX_ARCHIVE {
            return Err("Backup exceeds the 2 GB v1 limit".into());
        }
        let file = fs::File::open(path).map_err(err)?;
        let mut archive = ZipArchive::new(file).map_err(err)?;
        if archive.len() > 50000 {
            return Err("Too many files in backup".into());
        }
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        for i in 0..archive.len() {
            let mut f = archive.by_index(i).map_err(err)?;
            let name = f.name().to_string();
            if !safe_name(&name)
                || f.is_dir()
                || f.unix_mode()
                    .map(|m| m & 0o170000 == 0o120000)
                    .unwrap_or(false)
                || f.size() > MAX_ARCHIVE
            {
                return Err("Unsafe archive path, entry type or size".into());
            }
            total = total.checked_add(f.size()).ok_or("Archive size overflow")?;
            if total > MAX_ARCHIVE {
                return Err("Expanded backup exceeds the 2 GB v1 limit".into());
            }
            if files.contains_key(&name) {
                return Err("Duplicate archive file".into());
            }
            let size = f.size();
            let mut bytes = vec![];
            f.by_ref()
                .take(size + 1)
                .read_to_end(&mut bytes)
                .map_err(err)?;
            if bytes.len() as u64 != size {
                return Err("Archive size mismatch".into());
            }
            files.insert(name, bytes);
        }
        let manifest: Manifest =
            serde_json::from_slice(files.get("manifest.json").ok_or("Missing manifest")?)
                .map_err(err)?;
        if manifest.format_version != 1 || manifest.schema_version != 1 {
            return Err("Unsupported newer backup format".into());
        }
        if files.len() != manifest.files.len() + 1 {
            return Err("Unexpected files in backup".into());
        }
        for (name, record) in &manifest.files {
            if !safe_name(name) {
                return Err("Unsafe manifest path".into());
            }
            let bytes = files.get(name).ok_or("A backup file is missing")?;
            if record.size != bytes.len() as u64 || record.sha256 != hash(bytes) {
                return Err("Backup hash or size mismatch".into());
            }
        }
        let c: Canonical = serde_json::from_slice(
            files
                .get("canonical.json")
                .ok_or("Missing canonical JSON")?,
        )
        .map_err(err)?;
        Self::validate(&c)?;
        if c.library_id != manifest.library_id {
            return Err("Backup library identity mismatch".into());
        }
        if !files.contains_key("atlas.sqlite") || !files.contains_key("README.txt") {
            return Err("Backup lacks required files".into());
        }
        // The SQLite file is never opened during restore. Reconstruction uses only typed JSON and attachment bytes.
        for a in &c.attachments {
            let bytes = files.get(&a.relative_path).ok_or("Missing attachment")?;
            if a.size != bytes.len() as u64 || a.sha256 != hash(bytes) {
                return Err("Attachment hash mismatch".into());
            }
        }
        Ok((c, files))
    }
    pub fn stage_archive(&mut self, path: &Path) -> Result<Value> {
        let (canonical, files) = Self::validate_archive(path)?;
        let token = id();
        let root = self.base.join(format!("restore-{token}"));
        let operation = (|| {
            let db = Self::rebuild(&root, &canonical)?;
            for a in &canonical.attachments {
                let bytes = &files[&a.relative_path];
                fs::write(root.join(&a.relative_path), bytes).map_err(err)?;
            }
            let integrity: String = db
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(err)?;
            if integrity != "ok" {
                return Err("Restored database failed integrity check".into());
            }
            let actual = Self::canonical_from(&db)?;
            if actual != canonical {
                return Err("Reconstructed library differs from backup".into());
            }
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .map_err(err)?;
            drop(db);
            Ok(())
        })();
        if let Err(e) = operation {
            let _ = fs::remove_dir_all(&root);
            return Err(e);
        }
        self.staged.insert(token.clone(), root.clone());
        Ok(json!({"token":token,"path":root,"entries":canonical.entries.len()}))
    }
    pub fn confirm_restore(&mut self, token: &str) -> Result<Value> {
        let root = self
            .staged
            .remove(token)
            .ok_or("This staged restore is unavailable; choose the backup again")?;
        let replacement = Self::connect(&root)?;
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Invalid staged folder")?;
        let pointer_temp = self.base.join("active-library.part");
        let mut file = fs::File::create(&pointer_temp).map_err(err)?;
        file.write_all(name.as_bytes()).map_err(err)?;
        file.sync_all().map_err(err)?;
        drop(file);
        // Windows rename replaces an existing file atomically through std::fs::rename.
        fs::rename(&pointer_temp, self.base.join("active-library.txt")).map_err(err)?;
        self.db = replacement;
        self.root = root;
        self.view_v2()
    }
}
