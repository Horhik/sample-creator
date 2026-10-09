//! Exports every flagged region from the library without the UI:
//! `cargo run --example export_all -- DEST_DIR [TEMPLATE]`. Reads the library only.

use std::sync::atomic::AtomicBool;

use sample_creator::export::{self, OutputFormat, Source};
use sample_creator::store::Library;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dest: std::path::PathBuf = args
        .next()
        .expect("usage: export_all DEST [TEMPLATE]")
        .into();
    let template = args
        .next()
        .unwrap_or_else(|| export::DEFAULT_TEMPLATE.into());
    std::fs::create_dir_all(&dest)?;

    let library = Library::open_default()?;
    let regions: Vec<_> = library
        .all_regions()?
        .into_iter()
        .filter(|(_, r)| r.export)
        .map(|(f, r)| {
            let source = Source {
                path: f.path.clone(),
                title: f.title(),
                channels: f.channels,
            };
            (source, r)
        })
        .collect();

    for item in export::plan(&regions, &template, &dest) {
        let files = export::render(
            &item,
            OutputFormat::AsSource,
            &AtomicBool::new(false),
            &mut |_| {},
        )?;
        for f in files {
            println!(
                "{:<30} <- {} [{}..{}]",
                f.file_name().unwrap().to_string_lossy(),
                item.source.title,
                item.region.start,
                item.region.end
            );
        }
    }
    Ok(())
}
