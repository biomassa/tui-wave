mod audio;
mod cdp;
mod commands;
mod config;
mod model;
mod ui;

use std::path::Path;

/// The directory the Files panel should start in, given the file path the app was launched
/// with.
///
/// `Path::parent` returns `Some("")` — not `Some(".")` — for a bare filename with no directory
/// component, so `tui-wave take.wav` used to start the Files panel on an empty path and show
/// nothing at all, while `tui-wave ./take.wav` and `tui-wave /abs/take.wav` both worked.
fn containing_directory(path: &str) -> Option<std::path::PathBuf> {
    let parent = Path::new(path).parent()?;
    Some(if parent.as_os_str().is_empty() { Path::new(".").to_path_buf() } else { parent.to_path_buf() })
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    ui::terminal::install_panic_hook();

    let arg = std::env::args().nth(1);
    let (document, directory) = match arg {
        Some(ref p) if Path::new(p).is_dir() => {
            (None, Some(Path::new(p).to_path_buf()))
        }
        Some(ref p) if Path::new(p).is_file() => {
            let doc = Some(model::io::load_audio(p)?);
            (doc, containing_directory(p))
        }
        Some(p) => {
            // Try as file anyway; load_audio will report the error
            let doc = Some(model::io::load_audio(&p)?);
            (doc, containing_directory(&p))
        }
        None => (None, None),
    };

    let (mut terminal, picker) = ui::terminal::init()?;
    let mut app = ui::app::App::new(document, directory);
    app.set_picker(picker);
    let result = app.run(&mut terminal);

    ui::terminal::restore()?;
    result
}
