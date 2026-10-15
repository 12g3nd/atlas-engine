//! Storage integration tests. Fictional data in isolated temporary directories.
use super::*;
use repository::{get, put, upgrade_canonical};

fn library() -> (tempfile::TempDir, Repository) {
    let d = tempfile::tempdir().unwrap();
    let r = Repository::open(d.path().to_path_buf(), true).unwrap();
    (d, r)
}
fn prose(s: &str) -> Value {
    json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":s}]}]})
}
fn write(r: &mut Repository, title: &str, body: Value, areas: Vec<String>) -> Entry {
    let mut e = r.create(None).unwrap();
    e.title = title.into();
    e.body = body;
    e.area_ids = areas;
    let rev = e.revision;
    r.save(e, rev).unwrap()
}
fn area_by_seed(r: &Repository, key: &str) -> Area {
    r.canonical()
        .unwrap()
        .areas
        .into_iter()
        .find(|a| a.seed_key.as_deref() == Some(key))
        .unwrap()
}

#[test]
fn v31_fresh_library_has_call_numbers_hues_and_patterns() {
    let (_d, r) = library();
    assert_eq!(r.schema_version().unwrap(), 2);
    let expect = [
        ("inner-life", "100", "lilac", "plain"),
        ("academia", "300", "sky", "plain"),
        ("travel", "600", "sky", "dots"),
        ("internet-bible", "750", "lilac", "dots"),
    ];
    for (key, no, hue, pattern) in expect {
        let a = area_by_seed(&r, key);
        assert_eq!(a.call_number.as_deref(), Some(no));
        assert_eq!(a.hue.as_deref(), Some(hue));
        assert_eq!(a.pattern.as_deref(), Some(pattern));
    }
    let numbers: std::collections::HashSet<_> = r
        .canonical()
        .unwrap()
        .areas
        .iter()
        .map(|a| a.call_number.clone().unwrap())
        .collect();
    assert_eq!(numbers.len(), 14);
    // A branch takes the next free number above its parent.
    let making = area_by_seed(&r, "making");
    let mut branch = making.clone();
    branch.id = id();
    branch.seed_key = None;
    branch.parent = Some(making.id.clone());
    branch.call_number = None;
    branch.hue = None;
    branch.pattern = None;
    let mut c = r.canonical().unwrap();
    c.areas.push(branch.clone());
    upgrade_canonical(&mut c);
    let b = c.areas.iter().find(|a| a.id == branch.id).unwrap();
    assert_eq!(b.call_number.as_deref(), Some("401"));
    assert!(b.hue.is_some() && b.pattern.is_some());
}

#[test]
fn v01_v1_library_migrates_with_safety_bundle_and_failure_leaves_it_untouched() {
    let d = tempfile::tempdir().unwrap();
    {
        let mut r = Repository::open(d.path().to_path_buf(), true).unwrap();
        let a = area_by_seed(&r, "relationships");
        let mut person = r.create(Some("person".into())).unwrap();
        person.title = "Fictional Admired Person".into();
        person.metadata = json!({"relationship":"follow their work","contexts":["professional"]});
        person.area_ids = vec![a.id.clone()];
        let rev = person.revision;
        let person = r.save(person, rev).unwrap();
        let other = write(&mut r, "Fictional other", prose("x"), vec![a.id.clone()]);
        r.add_relation(
            person.id.clone(),
            other.id.clone(),
            "met through".into(),
            String::new(),
        )
        .unwrap();
        // Turn the stored records back into v1 shapes: no v2 fields, version 1.
        for mut area in rows::<Area>(&r.db, "areas").unwrap() {
            area.call_number = None;
            area.hue = None;
            area.pattern = None;
            let mut v = serde_json::to_value(&area).unwrap();
            for k in ["call_number", "hue", "pattern", "pinned"] {
                v.as_object_mut().unwrap().remove(k);
            }
            r.db.execute(
                "UPDATE areas SET data=?1 WHERE id=?2",
                rusqlite::params![v.to_string(), area.id],
            )
            .unwrap();
        }
        for rel in rows::<Relation>(&r.db, "relations").unwrap() {
            let mut v = serde_json::to_value(&rel).unwrap();
            v.as_object_mut().unwrap().remove("created");
            r.db.execute(
                "UPDATE relations SET data=?1 WHERE id=?2",
                rusqlite::params![v.to_string(), rel.id],
            )
            .unwrap();
        }
        r.db.execute_batch("PRAGMA user_version=1").unwrap();
        // Injected failure: nothing changes, version stays 1.
        r.fail = true;
        let before = r.canonical().unwrap();
        assert!(r.migrate().is_err());
        assert_eq!(r.schema_version().unwrap(), 1);
        assert_eq!(r.canonical().unwrap().areas, before.areas);
    }
    let r = Repository::open(d.path().to_path_buf(), true).unwrap();
    assert_eq!(r.schema_version().unwrap(), 2);
    let safety: Vec<_> = fs::read_dir(d.path().join("backups"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("pre-migration-v2-")
        })
        .collect();
    assert!(!safety.is_empty());
    assert!(Repository::validate_archive(&safety[0].path()).is_ok());
    let c = r.canonical().unwrap();
    assert!(c
        .areas
        .iter()
        .all(|a| a.call_number.is_some() && a.hue.is_some()));
    assert!(c.relations.iter().all(|r| r.created.is_some()));
    let person = c
        .entries
        .iter()
        .find(|e| e.title == "Fictional Admired Person")
        .unwrap();
    assert_eq!(person.metadata["circle"], "admire");
    assert_eq!(person.metadata["relationship"], "follow their work");
}

#[test]
fn v02_v1_bundle_restores_and_v2_round_trips_presentation() {
    let (d, mut r) = library();
    let a = area_by_seed(&r, "making");
    let e = write(
        &mut r,
        "Fictional bundle page",
        prose("kept"),
        vec![a.id.clone()],
    );
    r.set_presentation("shelf", json!([e.id])).unwrap();
    r.set_presentation(
        "layout",
        json!({"tokens":{(e.id.clone()):{"x":10,"y":20,"home":a.id}}}),
    )
    .unwrap();
    let mut c = r.canonical().unwrap();
    assert_eq!(c.format_version, 2);
    let root = d.path().join("rebuilt");
    Repository::rebuild(&root, &c).unwrap();
    let rebuilt =
        Repository::canonical_from(&rusqlite::Connection::open(root.join("atlas.sqlite")).unwrap())
            .unwrap();
    assert_eq!(rebuilt.presentation, c.presentation);
    // The same data presented as a v1 bundle still reconstructs.
    c.format_version = 1;
    c.schema_version = 1;
    c.presentation.clear();
    for a in &mut c.areas {
        a.call_number = None;
        a.hue = None;
        a.pattern = None;
    }
    let root1 = d.path().join("rebuilt-v1");
    Repository::rebuild(&root1, &c).unwrap();
    let v1 = Repository::canonical_from(
        &rusqlite::Connection::open(root1.join("atlas.sqlite")).unwrap(),
    )
    .unwrap();
    assert!(v1.areas.iter().all(|a| a.call_number.is_some()));
    assert!(v1
        .entries
        .iter()
        .any(|x| x.id == e.id && plain(&x.body) == "kept\n"));
}

#[test]
fn v03_callout_and_highlight_validate_and_export() {
    let (_d, mut r) = library();
    let body = json!({"type":"doc","content":[{"type":"callout","attrs":{"tone":"sage"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Note","marks":[{"type":"highlight"}]}]}]}]});
    let e = write(&mut r, "Callout", body, vec![]);
    let md = markdown(&e.body);
    assert!(md.contains("> [!sage]"));
    assert!(md.contains("==Note=="));
    let bad = json!({"type":"doc","content":[{"type":"callout","attrs":{"tone":"neon"},"content":[{"type":"paragraph"}]}]});
    assert!(validate_body(&bad, 0).is_err());
}

#[test]
fn v04_ghosts_link_by_uuid_clear_when_written_and_sweep_only_when_orphaned() {
    let d = tempfile::tempdir().unwrap();
    let (kept, orphan, written);
    {
        let mut r = Repository::open(d.path().to_path_buf(), true).unwrap();
        let a = area_by_seed(&r, "travel");
        let g1 = r
            .create_ghost("Lighthouses".into(), vec![a.id.clone()])
            .unwrap();
        let g2 = r.create_ghost("Never linked".into(), vec![]).unwrap();
        let mut g3 = r.create_ghost("Becomes real".into(), vec![]).unwrap();
        let source = write(
            &mut r,
            "Source",
            json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"atlasLink","attrs":{"target":g1.id,"label":"","autoLabel":true}}]}]}),
            vec![],
        );
        assert!(r
            .canonical()
            .unwrap()
            .relations
            .iter()
            .any(|x| x.source == source.id && x.target == g1.id));
        g3.body = prose("now it has writing");
        let rev = g3.revision;
        let g3 = r.save(g3, rev).unwrap();
        assert!(g3.metadata.get("ghost").is_none());
        let mut renamed = r.entry(&g1.id).unwrap();
        renamed.title = "Lighthouses of the north".into();
        let rev = renamed.revision;
        r.save(renamed, rev).unwrap();
        kept = g1.id;
        orphan = g2.id;
        written = g3.id;
        // A backup with ghosts present validates.
        let path = d.path().join("ghosts.atlas.zip");
        r.export_to(&path).unwrap();
        assert!(Repository::validate_archive(&path).is_ok());
    }
    let r = Repository::open(d.path().to_path_buf(), true).unwrap();
    assert!(r.entry(&kept).is_ok());
    assert!(r.entry(&written).is_ok());
    assert!(r.entry(&orphan).is_err());
}

#[test]
fn v19_calendar_buckets_by_local_day() {
    let (_d, mut r) = library();
    let mut e = write(&mut r, "Dated", prose("x"), vec![]);
    e.created = "2026-03-01T23:30:00+00:00".into();
    e.metadata = json!({"about_date":"2026-03-14"});
    put(&r.db, "entries", &e.id, &e).unwrap();
    // UTC+2: created 23:30Z is 2 March locally.
    let m = r.calendar_month(2026, 3, 120).unwrap();
    assert!(m["2026-03-02"]["made"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!(e.id)));
    assert!(m
        .get("2026-03-01")
        .map(|d| !d["made"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == &json!(e.id)))
        .unwrap_or(true));
    assert!(m["2026-03-14"]["about"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!(e.id)));
    let utc = r.calendar_month(2026, 3, 0).unwrap();
    assert!(utc["2026-03-01"]["made"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!(e.id)));
}

#[test]
fn v20_unlinked_mentions_skip_poems_and_link_mention_inserts_one_link() {
    let (_d, mut r) = library();
    let target = write(&mut r, "Harbor window", prose("target"), vec![]);
    let prose_page = write(
        &mut r,
        "Mentions",
        prose("I keep thinking about the harbor window at night."),
        vec![],
    );
    let poem_only = write(
        &mut r,
        "Poem only",
        json!({"type":"doc","content":[{"type":"poetry","content":[{"type":"text","text":"harbor window\n  only in a poem\n"}]}]}),
        vec![],
    );
    let found = r.unlinked_mentions(&target.id).unwrap();
    let ids: Vec<_> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["source"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&prose_page.id));
    assert!(!ids.contains(&poem_only.id));
    assert!(found[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("\u{2}harbor window\u{3}"));
    let linked = r.link_mention(&prose_page.id, &target.id).unwrap();
    let mut refs = vec![];
    references(&linked.body, &mut refs);
    assert_eq!(refs, vec![target.id.clone()]);
    assert!(plain(&linked.body).contains("I keep thinking about the "));
    assert!(r
        .unlinked_mentions(&target.id)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let back = r.backlinks(&target.id).unwrap();
    assert!(back
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["source"] == json!(prose_page.id)
            && b["snippet"].as_str().unwrap().contains('\u{2}')));
}

#[test]
fn v21_search_returns_highlighted_snippets_and_hides_hidden() {
    let (_d, mut r) = library();
    let mut e = write(
        &mut r,
        "Snippet page",
        prose("The tide table is a calendar for water."),
        vec![],
    );
    let hits = r
        .search_v2("calendar", false, None, None, None, None)
        .unwrap();
    assert!(hits[0]["snippet"]
        .as_str()
        .unwrap()
        .contains("\u{2}calendar\u{3}"));
    e.hidden = true;
    let rev = e.revision;
    r.save(e, rev).unwrap();
    assert!(r
        .search_v2("calendar", false, None, None, None, None)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        r.search_v2("calendar", true, None, None, None, None)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for q in ["\"", "' OR 1=1 --", "NEAR(", "日本語", ""] {
        assert!(r.search_v2(q, false, None, None, None, None).is_ok());
    }
}

#[test]
fn v22_capture_places_every_page_and_links_page_destinations() {
    let (_d, mut r) = library();
    let a = area_by_seed(&r, "inner-life");
    let e = r
        .capture("first line\nsecond line\n\nnew paragraph", "area", &a.id)
        .unwrap();
    assert_eq!(e.area_ids, vec![a.id.clone()]);
    assert_eq!(e.body["content"].as_array().unwrap().len(), 2);
    assert_eq!(e.body["content"][0]["content"][1]["type"], "hardBreak");
    let travel = area_by_seed(&r, "travel").id;
    let t = write(&mut r, "Destination", prose("x"), vec![travel]);
    let c = r.capture("about the destination", "page", &t.id).unwrap();
    assert_eq!(c.area_ids, t.area_ids);
    assert!(r
        .canonical()
        .unwrap()
        .relations
        .iter()
        .any(|x| x.source == c.id && x.target == t.id && x.label == "about"));
    assert!(r.capture("   ", "area", &a.id).is_err());
    assert!(r.capture("x", "elsewhere", &a.id).is_err());
}

#[test]
fn v29_kinds_rename_and_delete_rewrite_pages_atomically() {
    let (_d, mut r) = library();
    let mut e = write(&mut r, "Kinded", prose("x"), vec![]);
    e.kind = "poem".into();
    let rev = e.revision;
    let e = r.save(e, rev).unwrap();
    assert_eq!(r.kinds_usage().unwrap()["poem"], 1);
    r.rename_kind("poem", "verse").unwrap();
    assert_eq!(r.entry(&e.id).unwrap().kind, "verse");
    assert!(r.canonical().unwrap().kinds.contains(&"verse".to_string()));
    r.delete_kind("verse").unwrap();
    assert_eq!(r.entry(&e.id).unwrap().kind, "");
    assert!(!r.canonical().unwrap().kinds.contains(&"verse".to_string()));
}

#[test]
fn v25_preferences_round_trip_and_reject_unwritable_folders() {
    let (d, r) = library();
    let mut p = r.preferences();
    assert_eq!(p, features::Preferences::default());
    p.text_size = 99.0;
    p.writing_font = "comic".into();
    p.backup_folder = Some(d.path().to_string_lossy().to_string());
    r.save_preferences(&p.clone().sanitized()).unwrap();
    let back = r.preferences();
    assert_eq!(back.text_size, 21.0);
    assert_eq!(back.writing_font, "quattro");
    assert_eq!(r.backup_folder(), d.path());
    let mut bad = back.clone();
    bad.backup_folder = Some(d.path().join("missing").to_string_lossy().to_string());
    assert!(r.save_preferences(&bad).is_err());
    fs::write(r.prefs_path(), b"{ not json").unwrap();
    assert_eq!(r.preferences(), features::Preferences::default());
    let _ = get::<Entry>;
}
