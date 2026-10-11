use std::path::PathBuf;

use relm4::RelmApp;
use sample_creator::APP_ID;

fn main() {
    // GTK would try to treat the argument as a file to open; we handle it ourselves.
    // A folder becomes the tree root; a file also opens that file.
    let arg = std::env::args_os().nth(1).map(PathBuf::from);
    let (root, file) = match arg {
        Some(path) if path.is_file() => {
            let dir = path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(default_root);
            (dir, Some(path))
        }
        Some(dir) => (dir, None),
        None => (default_root(), None),
    };

    RelmApp::new(APP_ID)
        .with_args(Vec::new())
        .run::<sample_creator::ui::app::App>((root, file));
}

fn default_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into());
    let music = home.join("Music");
    if music.is_dir() { music } else { home }
}
