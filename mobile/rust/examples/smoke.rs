//! Runs the bridge API on a real file without the phone:
//! `cargo run -p sample_creator_bridge --example smoke -- FILE`
use sample_creator_bridge::api::engine as e;

fn main() -> anyhow::Result<()> {
    let file = std::env::args().nth(1).expect("FILE");
    let tmp = std::env::temp_dir().join("sample-creator-smoke");
    let _ = std::fs::remove_dir_all(&tmp);
    e::init(
        tmp.join("data").display().to_string(),
        tmp.join("cache").display().to_string(),
    )?;
    let info = e::open_file(file.clone())?;
    println!(
        "{} · {} Hz · {} ch · {} frames · {:?}",
        info.title, info.sample_rate, info.channels, info.total_frames, info.recorded_at
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let p = e::peaks(0.0, info.total_frames as f64, 100)?;
    println!(
        "peaks done {:.0}%, first {:?}",
        p.done * 100.0,
        &p.data[..4]
    );
    println!("markers from recorder: {}", e::markup()?.markers.len());
    let r = e::add_region(48_000, 96_000, 3)?;
    e::rename(e::ItemKind::Region, r, "test".into())?;
    e::set_region_speed(r, 200, true)?;
    let m = e::markup()?;
    let reg = &m.regions[0];
    println!(
        "region {} {}..{} speed {} keep {}",
        reg.name, reg.start, reg.end, reg.speed, reg.keep_pitch
    );
    e::undo()?;
    e::undo()?;
    e::undo()?;
    println!(
        "after 3 undo: {} regions, can_redo {}",
        e::markup()?.regions.len(),
        e::markup()?.can_redo
    );
    let dir = std::path::Path::new(&file)
        .parent()
        .unwrap()
        .display()
        .to_string();
    let list = e::list_dir(dir)?;
    println!(
        "list: {} entries, first {:?}",
        list.len(),
        list.first().map(|x| &x.name)
    );
    Ok(())
}
