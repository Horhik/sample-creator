//! Shares a folder with the phone without the UI, with the same code the
//! desktop would show: `cargo run --example share -- FOLDER [QR.png]`.

use relm4::gtk::prelude::*;
use relm4::gtk::{gdk, glib};
use sample_creator::remote;
use sample_creator::store::Library;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = std::fs::canonicalize(args.next().expect("usage: share FOLDER [QR.png]"))?;
    let library = Library::open_default()?;
    let (secret, token) = remote::share_keys(&library, &folder);
    let share = remote::Share::start(folder.clone(), secret, token)?;
    println!("{}\n{}", folder.display(), share.ticket());

    if let Some(png) = args.next() {
        let code = qrcode::QrCode::with_error_correction_level(
            share.ticket(),
            qrcode::EcLevel::L,
        )?;
        let (n, cell, quiet) = (code.width(), 8, 4);
        let side = (n + 2 * quiet) * cell;
        let colors = code.to_colors();
        let mut pixels = vec![255u8; side * side];
        for (i, pixel) in pixels.iter_mut().enumerate() {
            let (x, y) = ((i % side) / cell, (i / side) / cell);
            if (quiet..quiet + n).contains(&x)
                && (quiet..quiet + n).contains(&y)
                && colors[(y - quiet) * n + x - quiet] == qrcode::Color::Dark
            {
                *pixel = 0;
            }
        }
        gdk::MemoryTexture::new(
            side as i32,
            side as i32,
            gdk::MemoryFormat::G8,
            &glib::Bytes::from_owned(pixels),
            side,
        )
        .save_to_png(&png)?;
        println!("QR: {png}");
    }
    loop {
        std::thread::park();
    }
}
