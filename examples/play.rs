//! Plays part of a file through the app's player: `cargo run --example play -- FILE [START_SEC] [SECS]`.

use std::time::{Duration, Instant};

use sample_creator::audio::{decode, player::Player};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path: std::path::PathBuf = args.next().expect("usage: play FILE [START] [SECS]").into();
    let start: f64 = args.next().map_or(Ok(0.0), |s| s.parse())?;
    let secs: f64 = args.next().map_or(Ok(2.0), |s| s.parse())?;

    let info = decode::probe(&path)?;
    let rate = f64::from(info.sample_rate);
    let player = Player::new()?;
    player.load(path);
    let from = (start * rate) as u64;
    player.set_range(from, Some(from + (secs * rate) as u64), false);
    player.seek(from);
    player.play();

    let t = Instant::now();
    while t.elapsed() < Duration::from_secs_f64(secs + 1.0) {
        std::thread::sleep(Duration::from_millis(250));
        println!(
            "{:>5.2}s wall  playhead {:>8.3}s  playing={} finished={}",
            t.elapsed().as_secs_f64(),
            player.position() as f64 / rate,
            player.is_playing(),
            player.is_finished()
        );
    }
    Ok(())
}
