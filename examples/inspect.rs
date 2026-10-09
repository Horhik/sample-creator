//! Prints what the app reads from an audio file: `cargo run --example inspect -- FILE`.

use sample_creator::audio::decode;

fn main() -> anyhow::Result<()> {
    let path = std::env::args_os().nth(1).expect("usage: inspect FILE");
    let started = std::time::Instant::now();
    let (info, mut source) = decode::open(path.as_ref())?;

    println!(
        "{} ch, {} Hz, {}",
        info.channels, info.sample_rate, info.encoding
    );
    println!(
        "{} frames = {:.3} s",
        info.total_frames,
        info.duration_secs()
    );
    if let Some(bext) = &info.bext {
        let tr = bext.time_reference as f64 / f64::from(info.sample_rate);
        println!(
            "bext: {:?} {} {}, time reference {:02}:{:02}:{:06.3}",
            bext.originator,
            bext.origination_date,
            bext.origination_time,
            (tr / 3600.0) as u32,
            (tr / 60.0 % 60.0) as u32,
            tr % 60.0
        );
    }
    for cue in &info.cues {
        let secs = cue.frame as f64 / f64::from(info.sample_rate);
        println!(
            "cue {:>2} at {:>10} ({secs:8.3} s) {:?}",
            cue.id, cue.frame, cue.label
        );
    }

    let channels = usize::from(info.channels);
    let mut peak = vec![0f32; channels];
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if source.read(&mut buf, 65536)? == 0 {
            break;
        }
        for frame in buf.chunks_exact(channels) {
            for (p, s) in peak.iter_mut().zip(frame) {
                *p = p.max(s.abs());
            }
        }
    }
    let db: Vec<String> = peak
        .iter()
        .map(|p| format!("{:.1} dBFS", 20.0 * p.log10()))
        .collect();
    println!("peak per channel: {}", db.join(", "));
    println!("read in {:?}", started.elapsed());
    Ok(())
}
