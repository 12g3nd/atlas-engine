use atlas_engine::{from_markdown, Repository};

fn main() -> Result<(), String> {
    let temporary = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut repository = Repository::open(temporary.path().join("source"), true)?;
    let mut page = repository.create(None)?;
    page.title = "A fictional field note".into();
    page.body = from_markdown("A thought worth keeping.");
    let stale = page.clone();
    let saved = repository.save(page, 0)?;
    assert!(repository.save(stale, 0).is_err());
    let archive = temporary.path().join("snapshot.atlas.zip");
    repository.export_to(&archive)?;
    let staged = repository.stage_archive(&archive)?;
    let token = staged["token"]
        .as_str()
        .ok_or("Missing staged restore token")?;
    repository.confirm_restore(token)?;
    assert_eq!(repository.entry(&saved.id)?, saved);
    println!(
        "Committed revision {}; stale write rejected; backup restored exactly.",
        saved.revision
    );
    Ok(())
}
