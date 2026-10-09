//! Times the peak summary of a file: `cargo run --release --example peaks_bench -- FILE`.

use std::time::{Duration, Instant};

use sample_creator::audio::{decode, peaks};

fn main() -> anyhow::Result<()> {
    let path = std::path::PathBuf::from(std::env::args_os().nth(1).expect("usage: peaks_bench FILE"));
    let info = decode::probe(&path)?;
    let t = Instant::now();
    let job = peaks::start(&path, usize::from(info.channels), info.total_frames);
    while !job.peaks.is_complete() {
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("{:.0} s of audio summarised in {:?}", info.duration_secs(), t.elapsed());
    Ok(())
}
