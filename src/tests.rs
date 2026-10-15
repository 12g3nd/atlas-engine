use super::*;
use portability::hash;
use repository::get;
use std::io::{Read, Write};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

fn library() -> (tempfile::TempDir, Repository) {
    let d = tempfile::tempdir().unwrap();
    let r = Repository::open(d.path().to_path_buf(), true).unwrap();
    (d, r)
}
fn legacy_starter(r: &Repository, key: &str) -> Entry {
    let seeds: Value = serde_json::from_str(include_str!("../seed/starter-pages.json")).unwrap();
    let p = seeds["pages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["seed_key"] == key)
        .unwrap();
    let c = r.canonical().unwrap();
    let mut e = Entry::blank();
    e.seed_key = Some(key.into());
    e.title = p["title"].as_str().unwrap().into();
    e.kind = p["kind"].as_str().unwrap_or("").into();
    e.entity_type = p["entity_type"].as_str().unwrap().into();
    e.pinned = p["pin_on_home"].as_bool().unwrap();
    e.placeholder = p["editor_placeholder"]
        .as_str()
        .unwrap_or("Begin wherever you are…")
        .into();
    e.body = if p["initial_editor_block"] == "poetry" {
        json!({"type":"doc","content":[{"type":"poetry"}]})
    } else {
        from_markdown(p["body_markdown"].as_str().unwrap())
    };
    e.area_ids = p["area_seed_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            c.areas
                .iter()
                .find(|a| a.seed_key.as_deref() == k.as_str())
                .unwrap()
                .id
                .clone()
        })
        .collect();
    Repository::write_entry(&r.db, &e).unwrap();
    e
}
#[test]
fn conservative_starter_review_revalidation_and_recoverable_cleanup() {
    let (d, mut r) = library();
    assert!(r.untouched_starters().unwrap().is_empty());
    let pristine = legacy_starter(&r, "first-poem");
    let mut edited = legacy_starter(&r, "first-lesson");
    edited = r.save(edited, 0).unwrap(); // Even an edit reverted to the original is uncertain.
    let attached = legacy_starter(&r, "first-reference");
    r.import_attachment(&attached.id, "fiction.txt", b"fiction")
        .unwrap();
    let linked = legacy_starter(&r, "letter-to-myself");
    r.add_relation(
        linked.id.clone(),
        pristine.id.clone(),
        "about".into(),
        "".into(),
    )
    .unwrap();
    assert!(r.untouched_starters().unwrap().is_empty()); // Incoming connections protect the poem too.
    let clean = legacy_starter(&r, "first-observation");
    assert_eq!(
        r.untouched_starters()
            .unwrap()
            .iter()
            .map(|e| e.id.clone())
            .collect::<Vec<_>>(),
        vec![clean.id.clone()]
    );
    assert!(r
        .trash_starters(&[clean.id.clone(), edited.id.clone()])
        .is_err());
    assert!(r.entry(&clean.id).unwrap().trashed.is_none());
    r.trash_starters(std::slice::from_ref(&clean.id)).unwrap();
    assert!(r.entry(&clean.id).unwrap().trashed.is_some());
    assert_eq!(r.history(&clean.id).unwrap()[0].entry.body, clean.body);
    r.trash(&clean.id, true).unwrap();
    assert_eq!(r.entry(&clean.id).unwrap().body, clean.body);
    assert!(r.untouched_starters().unwrap().is_empty());
    drop(r);
    let r = Repository::open(d.path().into(), true).unwrap();
    assert_eq!(r.entry(&clean.id).unwrap().id, clean.id);
    assert_eq!(r.entry(&edited.id).unwrap().revision, edited.revision);
}
fn prose(s: &str) -> Value {
    json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":s}]}]})
}
fn write(r: &mut Repository, title: &str, body: Value) -> Entry {
    let mut e = r.create(None).unwrap();
    e.title = title.into();
    e.body = body;
    r.save(e, 0).unwrap()
}
fn poem() -> String {
    include_str!("../fixtures/poem-whitespace.txt").to_string()
}
fn mutate_zip(
    input: &std::path::Path,
    out: &std::path::Path,
    mutator: impl FnOnce(&mut std::collections::BTreeMap<String, Vec<u8>>),
) {
    let mut a = ZipArchive::new(fs::File::open(input).unwrap()).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        let mut bytes = vec![];
        f.read_to_end(&mut bytes).unwrap();
        files.insert(f.name().to_string(), bytes);
    }
    mutator(&mut files);
    let mut w = ZipWriter::new(fs::File::create(out).unwrap());
    for (n, b) in files {
        w.start_file(n, SimpleFileOptions::default()).unwrap();
        w.write_all(&b).unwrap();
    }
    w.finish().unwrap();
}
fn refresh_manifest(files: &mut std::collections::BTreeMap<String, Vec<u8>>) {
    let mut m: Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    let mut records = serde_json::Map::new();
    for (n, b) in files.iter().filter(|(n, _)| n.as_str() != "manifest.json") {
        records.insert(n.clone(), json!({"size":b.len(),"sha256":hash(b)}));
    }
    m["files"] = Value::Object(records);
    files.insert("manifest.json".into(), serde_json::to_vec(&m).unwrap());
}

#[test]
fn sqlite_save_reopen_stale_and_failure() {
    let (d, mut r) = library();
    let e = write(&mut r, "Fictional", prose("A committed thought."));
    let mut stale = e.clone();
    stale.title = "new revision".into();
    let newer = r.save(stale.clone(), e.revision).unwrap();
    assert!(r.save(stale, e.revision).is_err());
    r.fail = true;
    let mut dirty = newer.clone();
    dirty.body = prose("Never committed");
    assert!(r.save(dirty, newer.revision).is_err());
    assert_eq!(r.entry(&e.id).unwrap(), newer);
    drop(r);
    let r = Repository::open(d.path().into(), true).unwrap();
    assert_eq!(r.entry(&e.id).unwrap(), newer);
    let mode: String =
        r.db.query_row("PRAGMA journal_mode", [], |q| q.get(0))
            .unwrap();
    assert_eq!(mode, "wal");
    let sync: i64 =
        r.db.query_row("PRAGMA synchronous", [], |q| q.get(0))
            .unwrap();
    assert_eq!(sync, 2);
    assert_eq!(
        r.db.query_row("PRAGMA integrity_check", [], |q| q.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
#[test]
fn seeds_edit_once_taxonomy_and_area_cycles() {
    let (d, mut r) = library();
    let c = r.canonical().unwrap();
    assert_eq!(c.areas.len(), 14);
    let mut why = c
        .entries
        .iter()
        .find(|e| e.seed_key.as_deref() == Some("meta-why"))
        .unwrap()
        .clone();
    why.title = "Changed philosophical purpose".into();
    let rev = why.revision;
    r.save(why.clone(), rev).unwrap();
    let blank = c
        .entries
        .iter()
        .find(|e| e.seed_key.as_deref() == Some("meta-own-interpretation"))
        .unwrap();
    assert_eq!(plain(&blank.body).trim(), "");
    assert!(c.entries.iter().all(|e| e
        .seed_key
        .as_deref()
        .map(|s| s.starts_with("meta-"))
        .unwrap_or(true)));
    // A legacy suggestion is retained by its existing initialization ledger.
    let mut starter = Entry::blank();
    starter.seed_key = Some("first-lesson".into());
    starter.title = "A life lesson".into();
    Repository::write_entry(&r.db, &starter).unwrap();
    r.trash(&starter.id, false).unwrap();
    let mut a = c.areas[0].clone();
    a.name = "Renamed area".into();
    a.order = 99;
    a.hidden = true;
    a.parent = Some(c.areas[1].id.clone());
    r.upsert_area(a.clone()).unwrap();
    let mut b = c.areas[1].clone();
    b.parent = Some(a.id.clone());
    assert!(r.upsert_area(b).is_err());
    drop(r);
    let mut r = Repository::open(d.path().into(), true).unwrap();
    r.seed().unwrap();
    assert_eq!(r.entry(&why.id).unwrap().title, why.title);
    assert!(r.entry(&starter.id).unwrap().trashed.is_some());
    assert_eq!(get::<Area>(&r.db, "areas", &a.id).unwrap(), a);
    r.delete_area(&a.id).unwrap();
    assert!(r.entry(&a.overview).is_ok());
}
#[test]
fn poetry_attachment_history_trash_full_wal_roundtrip_json_reconstruction() {
    let (d, mut r) = library();
    let fixture = poem();
    assert!(fixture.ends_with('\n'));
    assert!(fixture.contains('\t'));
    let mut e = write(
        &mut r,
        "Fictional poem",
        json!({"type":"doc","content":[{"type":"poetry","content":[{"type":"text","text":fixture}]}]}),
    );
    let c = r.canonical().unwrap();
    e.area_ids = c.areas.iter().take(3).map(|a| a.id.clone()).collect();
    e.metadata = json!({"source":"fictional","context":"test"});
    e.hidden = true;
    e.rediscovery = true;
    let rev = e.revision;
    e = r.save(e, rev).unwrap();
    let source = d.path().join("fictional-image.png");
    let png = b"\x89PNG\r\n\x1a\nFictional test bytes";
    fs::write(&source, png).unwrap();
    let image = r
        .import_attachment(&e.id, "fictional-image.png", &fs::read(&source).unwrap())
        .unwrap();
    fs::remove_file(&source).unwrap();
    let file = r
        .import_attachment(&e.id, "fictional.txt", b"Fictional retained file")
        .unwrap();
    assert_eq!(r.attachment_bytes(&image.id).unwrap().1, png);
    assert_eq!(
        r.attachment_bytes(&file.id).unwrap().1,
        b"Fictional retained file"
    );
    let version = r.snapshot(&e.id, "First poem", false).unwrap().unwrap();
    let mut next = e.clone();
    next.title = "Renamed poem".into();
    next.body = prose("Later version");
    let rev = next.revision;
    r.save(next, rev).unwrap();
    let restored = r.restore_version(&version.id).unwrap();
    assert_eq!(plain(&restored.body), fixture);
    assert!(r
        .history(&e.id)
        .unwrap()
        .iter()
        .any(|r| r.label == "Before restoring a version"));
    r.trash(&e.id, false).unwrap();
    assert!(r.entry(&e.id).unwrap().trashed.is_some());
    let e = r.trash(&e.id, true).unwrap();
    assert_eq!(plain(&e.body), fixture);
    let other = write(
        &mut r,
        "Fictional poem",
        prose("Duplicate title with unique UUID"),
    );
    r.add_relation(
        e.id.clone(),
        other.id.clone(),
        "challenges".into(),
        "An invented connection".into(),
    )
    .unwrap();
    let mut area = c.areas[0].clone();
    area.name = "Renamed test area".into();
    r.upsert_area(area).unwrap();
    r.upsert_collection(Collection {
        id: id(),
        title: "Fixture collection".into(),
        introduction: other.id.clone(),
        members: vec![e.id.clone(), other.id.clone()],
        seed_key: None,
        pinned: false,
    })
    .unwrap();
    r.trash(&other.id, false).unwrap();
    let canonical = r.canonical().unwrap();
    let backup = d.path().join("roundtrip.atlas.zip");
    r.export_to(&backup).unwrap();
    let mut archive = ZipArchive::new(fs::File::open(&backup).unwrap()).unwrap();
    let mut poetry_text = String::new();
    archive
        .by_name(&format!("poetry/{}-0.txt", e.id))
        .unwrap()
        .read_to_string(&mut poetry_text)
        .unwrap();
    assert_eq!(poetry_text, fixture);
    let mut snapshot = vec![];
    archive
        .by_name("atlas.sqlite")
        .unwrap()
        .read_to_end(&mut snapshot)
        .unwrap();
    let snap_path = d.path().join("snapshot.sqlite");
    fs::write(&snap_path, snapshot).unwrap();
    let snap = rusqlite::Connection::open(snap_path).unwrap();
    assert_eq!(Repository::canonical_from(&snap).unwrap(), canonical);
    assert_eq!(
        snap.query_row("PRAGMA integrity_check", [], |q| q.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(snap);
    let staged = r.stage_archive(&backup).unwrap();
    let old_root = r.root.clone();
    r.confirm_restore(staged["token"].as_str().unwrap())
        .unwrap();
    assert_eq!(r.canonical().unwrap(), canonical);
    assert!(old_root.join("atlas.sqlite").exists());
    assert_eq!(plain(&r.entry(&e.id).unwrap().body), fixture);
    drop(r);
    let mut r = Repository::open(d.path().into(), true).unwrap();
    assert_eq!(plain(&r.entry(&e.id).unwrap().body), fixture);
    let json_only = d.path().join("json-only.atlas.zip");
    mutate_zip(&backup, &json_only, |f| {
        f.insert(
            "atlas.sqlite".into(),
            b"Deliberately unusable SQLite; JSON is authoritative".to_vec(),
        );
        refresh_manifest(f);
    });
    let stage = r.stage_archive(&json_only).unwrap();
    r.confirm_restore(stage["token"].as_str().unwrap()).unwrap();
    assert_eq!(r.canonical().unwrap(), canonical);
    assert_eq!(r.attachment_bytes(&image.id).unwrap().1, png);
}
#[test]
fn restore_validation_rejects_corruption_and_keeps_original() {
    let (d, mut r) = library();
    let e = write(&mut r, "Original test data", prose("Stay intact"));
    let a = r
        .import_attachment(&e.id, "fictional.txt", b"test")
        .unwrap();
    let good = d.path().join("good.atlas.zip");
    r.export_to(&good).unwrap();
    let original = r.root.clone();
    for scenario in 0..7 {
        let bad = d.path().join(format!("bad-{scenario}.zip"));
        mutate_zip(&good, &bad, |f| match scenario {
            0 => {
                f.insert(a.relative_path.clone(), b"bad".to_vec());
            }
            1 => {
                f.remove(&a.relative_path);
                refresh_manifest(f);
            }
            2 => {
                f.insert("../escape.txt".into(), b"bad".to_vec());
                refresh_manifest(f);
            }
            3 => {
                let mut m: Value = serde_json::from_slice(&f["manifest.json"]).unwrap();
                m["format_version"] = json!(999);
                f.insert("manifest.json".into(), serde_json::to_vec(&m).unwrap());
            }
            4 => {
                f.insert("canonical.json".into(), b"{broken".to_vec());
                refresh_manifest(f);
            }
            5 => {
                let mut c: Value = serde_json::from_slice(&f["canonical.json"]).unwrap();
                c["entries"][0]["area_ids"] = json!(["missing-area"]);
                f.insert("canonical.json".into(), serde_json::to_vec(&c).unwrap());
                refresh_manifest(f);
            }
            _ => {
                f.insert("C:/escape.txt".into(), b"bad".to_vec());
                refresh_manifest(f);
            }
        });
        assert!(r.stage_archive(&bad).is_err(), "scenario {scenario}");
        assert_eq!(r.root, original);
        assert_eq!(r.entry(&e.id).unwrap(), e);
    }
    assert!(r.export_to(&good).is_err());
    assert!(Repository::validate_archive(&good).is_ok());
    let stage = r.stage_archive(&good).unwrap();
    assert!(!stage["token"].is_null());
    drop(r);
    let r = Repository::open(d.path().into(), true).unwrap();
    assert_eq!(r.root, original);
    assert_eq!(r.entry(&e.id).unwrap(), e);
}
#[test]
fn unsafe_archive_symlink_and_huge_declared_file() {
    let (d, r) = library();
    let good = d.path().join("good.zip");
    r.export_to(&good).unwrap();
    let bad = d.path().join("symlink.zip");
    let mut w = ZipWriter::new(fs::File::create(&bad).unwrap());
    w.add_symlink("evil", "../../outside", SimpleFileOptions::default())
        .unwrap();
    w.finish().unwrap();
    assert!(Repository::validate_archive(&bad).is_err());
    let mut bytes = fs::read(&good).unwrap();
    let central = bytes.windows(4).position(|s| s == b"PK\x01\x02").unwrap();
    bytes[central + 24..central + 28]
        .copy_from_slice(&(2u32 * 1024 * 1024 * 1024 + 1).to_le_bytes());
    let huge = d.path().join("huge-declared.zip");
    fs::write(&huge, bytes).unwrap();
    assert!(Repository::validate_archive(&huge).is_err());
}
#[test]
fn backup_daily_rotation_and_named_history_retention() {
    // SPEC_V2 §7.10 and Q25: newest bundle per day for 14 days and per ISO
    // week for 8 weeks; automatic history thins by age, named never does.
    let (d, mut r) = library();
    let mut e = write(&mut r, "Rotation", prose("initial"));
    let version = r.snapshot(&e.id, "Keep forever", false).unwrap().unwrap();
    let start = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for day in 0..20 {
        e.title = format!("day {day}");
        let rev = e.revision;
        e = r.save(e, rev).unwrap();
        let time = start + chrono::Duration::days(day);
        assert!(r.automatic_backup_v2(time).unwrap().is_some());
        set_setting(&r.db, "changed", "true").unwrap();
        assert!(r
            .automatic_backup_v2(time + chrono::Duration::hours(1))
            .unwrap()
            .is_none());
    }
    let names: Vec<String> = fs::read_dir(d.path().join("backups"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("automatic-"))
        .collect();
    // Days 6..=19 by day, plus 4 January as the newest of ISO week 2026-W01.
    assert_eq!(names.len(), 15, "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("automatic-20260104")));
    assert!(!names.iter().any(|n| n.starts_with("automatic-20260101")));
    let manual = d.path().join("manual.atlas.zip");
    r.export_to(&manual).unwrap();
    crate::features::retain_backups(
        &d.path().join("backups"),
        start + chrono::Duration::days(400),
    )
    .unwrap();
    assert!(manual.exists());
    for i in 0..105 {
        e.title = format!("version {i}");
        let rev = e.revision;
        e = r.save(e, rev).unwrap();
        r.snapshot(&e.id, "", true).unwrap();
    }
    let h = r.history(&e.id).unwrap();
    assert!(h.iter().filter(|r| r.automatic).count() >= 105);
    let later = chrono::Utc::now() + chrono::Duration::days(40);
    Repository::thin_history(&r.db, &e.id, later).unwrap();
    let h = r.history(&e.id).unwrap();
    assert!(h.iter().filter(|r| r.automatic).count() <= 2);
    assert!(h.iter().any(|r| r.id == version.id));
}
#[test]
fn markdown_plain_whitespace_and_active_html_inert() {
    let source="# Fictional heading\n\nAn **emphasis** and [safe](https://example.invalid).\n\n<script>alert('test')</script>\n";
    let body = from_markdown(source);
    validate_body(&body, 0).unwrap();
    assert!(plain(&body).contains("<script>"));
    assert!(markdown(&body).contains("**emphasis**"));
    assert!(!markdown(&body).contains("<script>"));
    assert!(!safe_url("javascript:alert(1)"));
    assert!(!safe_url("file:///C:/private"));
    assert!(safe_url("https://example.invalid"));
    let p = poem();
    let body =
        json!({"type":"doc","content":[{"type":"poetry","content":[{"type":"text","text":p}]}]});
    assert_eq!(plain(&body), p);
    assert!(markdown(&body).contains("```poem\n"));
}

#[test]
fn selected_page_export_copies_assets_and_preserves_poetry() {
    let (d, mut r) = library();
    let fixture = poem();
    let mut e = write(
        &mut r,
        "Readable",
        json!({"type":"doc","content":[{"type":"poetry","content":[{"type":"text","text":fixture}]}]}),
    );
    let a = r
        .import_attachment(&e.id, "fictional.txt", b"asset bytes")
        .unwrap();
    e.body["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"attachment","attrs":{"attachmentId":a.id,"name":a.name}}));
    let rev = e.revision;
    e = r.save(e, rev).unwrap();
    let md = d.path().join("selected.md");
    r.export_page_to(&e.id, "markdown", &md).unwrap();
    let exported = fs::read_to_string(&md).unwrap();
    assert!(exported.contains(&format!("{}-assets/{}", e.id, a.id)));
    assert_eq!(
        fs::read(d.path().join(format!("{}-assets/{}", e.id, a.id))).unwrap(),
        b"asset bytes"
    );
    assert!(exported.contains(&fixture));
    assert!(r.export_page_to(&e.id, "markdown", &md).is_err());
}
#[test]
fn malformed_documents_and_hashed_restore_marks_refused_before_switch() {
    let (d, mut r) = library();
    let e = write(&mut r, "Intact fictional page", prose("Do not discard"));
    let good = d.path().join("supported.zip");
    r.export_to(&good).unwrap();
    let original = r.root.clone();
    let invalid = vec![
        json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"unsafe mark","marks":[{"type":"unsupported"}]}]}]}),
        json!({"type":"doc","content":[{"type":"text","text":"not a block"}]}),
        json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"bad URL","marks":[{"type":"link","attrs":{"href":"javascript:alert(1)"}}]}]}]}),
        json!({"type":"doc","content":[{"type":"heading","attrs":{"level":"broken"}}]}),
        json!({"type":"doc","content":[{"type":"paragraph","content":"broken"}]}),
        json!({"type":"doc","content":[{"type":"poetry","content":[{"type":"text","text":"line","marks":[{"type":"bold"}]}]}]}),
    ];
    for (i, body) in invalid.into_iter().enumerate() {
        let mut changed = e.clone();
        changed.body = body.clone();
        assert!(r.save(changed, e.revision).is_err());
        assert_eq!(r.entry(&e.id).unwrap(), e);
        let bad = d.path().join(format!("invalid-schema-{i}.zip"));
        mutate_zip(&good, &bad, |f| {
            let mut canonical: Value = serde_json::from_slice(&f["canonical.json"]).unwrap();
            canonical["entries"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|v| v["id"] == e.id)
                .unwrap()["body"] = body;
            f.insert(
                "canonical.json".into(),
                serde_json::to_vec(&canonical).unwrap(),
            );
            refresh_manifest(f);
        });
        assert!(r.stage_archive(&bad).is_err());
        assert_eq!(r.root, original);
        assert_eq!(r.entry(&e.id).unwrap(), e);
    }
}

#[test]
fn markdown_round_trips_the_editors_own_blocks() {
    let text = |t: &str| json!({"type":"text","text":t});
    let marked = |t: &str, m: &str| json!({"type":"text","text":t,"marks":[{"type":m}]});
    let para = |c: Value| json!({"type":"paragraph","content":c});
    let item = |kind: &str, c: Value| json!({"type":kind,"content":[para(c)]});
    let doc = json!({"type":"doc","content":[
        para(json!([text("Plain, "), marked("lit", "highlight"), text(" and "), marked("gone", "strike"), text(".")])),
        para(json!([text("# not a heading, a == b, and 1. not a list")])),
        {"type":"orderedList","attrs":{"start":3},"content":[
            item("listItem", json!([text("third")])),
            {"type":"listItem","content":[para(json!([text("fourth")])),
                {"type":"bulletList","content":[item("listItem", json!([text("nested")]))]}]}
        ]},
        {"type":"taskList","content":[
            {"type":"taskItem","attrs":{"checked":true},"content":[para(json!([text("done")]))]},
            {"type":"taskItem","attrs":{"checked":false},"content":[para(json!([text("open")]))]}
        ]},
        {"type":"callout","attrs":{"tone":"sage"},"content":[para(json!([text("Calm "), marked("note", "bold")]))]},
        {"type":"table","content":[
            {"type":"tableRow","content":[
                {"type":"tableHeader","content":[para(json!([text("Name")]))]},
                {"type":"tableHeader","content":[para(json!([text("Pipe")]))]}]},
            {"type":"tableRow","content":[
                {"type":"tableCell","content":[para(json!([text("Ada")]))]},
                {"type":"tableCell","content":[para(json!([text("a|b")]))]}]}
        ]}
    ]});
    validate_body(&doc, 0).unwrap();
    let md = markdown(&doc);
    assert!(md.contains("| --- | --- |"), "{md}");
    assert!(md.contains("3. third\n4. fourth\n   - nested\n"), "{md}");
    let back = from_markdown(&md);
    validate_body(&back, 0).unwrap();
    assert_eq!(plain(&back).trim(), plain(&doc).trim());
    let kinds = |v: &Value| {
        v["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&back), kinds(&doc));
    assert_eq!(back["content"][0]["content"][1], marked("lit", "highlight"));
    assert_eq!(back["content"][0]["content"][3], marked("gone", "strike"));
    assert_eq!(back["content"][1], doc["content"][1]);
    assert_eq!(back["content"][2]["attrs"]["start"], 3);
    assert_eq!(
        back["content"][2]["content"][1]["content"][1]["type"],
        "bulletList"
    );
    assert_eq!(back["content"][3], doc["content"][3]);
    assert_eq!(back["content"][4], doc["content"][4]);
    assert_eq!(
        back["content"][5]["content"][0]["content"][0]["type"],
        "tableHeader"
    );
    assert_eq!(
        back["content"][5]["content"][1]["content"][1],
        doc["content"][5]["content"][1]["content"][1]
    );
}

#[test]
fn obsidian_markdown_imports_as_editor_blocks() {
    let source = "---\ntags: [reading]\n---\n\n> [!tip] Keep\n> this ==bright *idea*== close\n\n- [ ] one\n- two\n\n*a [x](file:///C:/private) b*\n\n> <div>inert</div>\n";
    let body = from_markdown(source);
    validate_body(&body, 0).unwrap();
    let c = body["content"].as_array().unwrap();
    assert_eq!(c[0]["type"], "codeBlock");
    assert_eq!(plain(&c[0]).trim(), "tags: [reading]");
    assert_eq!(c[1]["type"], "callout");
    assert_eq!(c[1]["attrs"]["tone"], "sage");
    let line = c[1]["content"][0]["content"].as_array().unwrap();
    assert_eq!(line[0]["text"], "Keep\nthis ");
    assert_eq!(
        line[1],
        json!({"type":"text","text":"bright ","marks":[{"type":"highlight"}]})
    );
    assert_eq!(
        line[2],
        json!({"type":"text","text":"idea","marks":[{"type":"italic"},{"type":"highlight"}]})
    );
    assert_eq!(c[2]["type"], "taskList");
    assert_eq!(c[2]["content"][1]["attrs"]["checked"], false);
    assert!(plain(&c[4]).contains("<div>inert</div>"));
}

#[test]
fn interrupted_backup_leftovers_swept_and_missing_folder_reported() {
    let (d, r) = library();
    let scratch = r.base.join(format!("export-{}", id()));
    let unrelated = r.base.join("export-notes");
    fs::create_dir_all(&scratch).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    let folder = r.backup_folder();
    fs::create_dir_all(&folder).unwrap();
    let part = folder.join(format!(".atlas-export-{}.part", id()));
    let other = folder.join("letters.part");
    fs::write(&part, b"partial").unwrap();
    fs::write(&other, b"kept").unwrap();
    r.sweep_interrupted_backups();
    assert!(!scratch.exists() && !part.exists());
    assert!(unrelated.exists() && other.exists());
    assert_eq!(r.backup_status().unwrap()["missing"], false);
    let mut p = r.preferences();
    let drive = d.path().join("unplugged");
    fs::create_dir_all(&drive).unwrap();
    p.backup_folder = Some(drive.to_string_lossy().into());
    r.save_preferences(&p).unwrap();
    fs::remove_dir_all(&drive).unwrap();
    let status = r.backup_status().unwrap();
    assert_eq!(status["missing"], true);
    assert_eq!(
        PathBuf::from(status["folder"].as_str().unwrap()),
        r.base.join("backups")
    );
}
