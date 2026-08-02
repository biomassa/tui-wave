mod audio;
mod cdp;
mod commands;
mod config;
mod model;
mod praat;
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

    // Before anything can create one: a CDP job directory tagged with this PID can only be the
    // work of an earlier process that was killed before it could clean up, and leaving it there
    // lets a job land in it and read its leftovers as its own output. See the function's own
    // comment for why this belongs here and not in `CdpRunner::new`.
    cdp::runner::sweep_stale_temp_dirs();

    // A file argument is *queued*, not decoded here: `App::load_file` is what decides whether a
    // file fits in RAM or has to open streamed and read-only (`model::stream`), and decoding
    // eagerly at startup bypassed that entirely — so the command line, the primary way a large
    // take gets opened, always tried to load it fully. It also means an unreadable path now
    // surfaces as a dialog inside the running app rather than aborting before the terminal is
    // even initialized.
    let arg = std::env::args().nth(1);
    let (open_path, directory) = match arg {
        Some(ref p) if Path::new(p).is_dir() => (None, Some(Path::new(p).to_path_buf())),
        // A non-existent path is still queued: `load_file` reports why it could not be opened,
        // which is more useful than exiting with nothing on screen.
        Some(p) => {
            let dir = containing_directory(&p);
            (Some(Path::new(&p).to_path_buf()), dir)
        }
        None => (None, None),
    };

    let (mut terminal, picker) = ui::terminal::init()?;
    let mut app = ui::app::App::new(None, directory);
    app.set_picker(picker);
    if let Some(path) = open_path {
        app.queue_open(path);
    }
    let result = app.run(&mut terminal);

    ui::terminal::restore()?;
    result
}
