mod airwindows;
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

/// What the command line asked for.
///
/// Split out from `main` so the flag/path decision is testable without a terminal, and so
/// there is one place that decides whether a run touches the terminal at all.
#[derive(Debug, PartialEq)]
enum Invocation {
    /// Print the version and exit, before anything with a side effect happens.
    Version,
    /// Print usage and exit, likewise.
    Help,
    /// Start the editor. `Some(path)` is queued for opening; a directory instead sets the
    /// starting directory of the Files panel.
    Run(Option<std::path::PathBuf>, Option<std::path::PathBuf>),
}

/// Anything that starts with `-` is a flag, and an unrecognised one is an error rather than a
/// filename: a mistyped `--verison` that fell through to the path branch would open the editor
/// on a file that cannot exist, which looks like the flag was accepted and did nothing.
fn parse_args(arg: Option<String>, is_dir: impl Fn(&Path) -> bool) -> Result<Invocation, String> {
    match arg.as_deref() {
        Some("--version" | "-V") => Ok(Invocation::Version),
        Some("--help" | "-h") => Ok(Invocation::Help),
        Some(flag) if flag.starts_with('-') && flag != "-" => {
            Err(format!("unrecognised option: {flag}"))
        }
        Some(p) if is_dir(Path::new(p)) => Ok(Invocation::Run(None, Some(Path::new(p).to_path_buf()))),
        // A non-existent path is still queued: `load_file` reports why it could not be opened,
        // which is more useful than exiting with nothing on screen.
        Some(p) => Ok(Invocation::Run(Some(Path::new(p).to_path_buf()), containing_directory(p))),
        None => Ok(Invocation::Run(None, None)),
    }
}

const USAGE: &str = "\
tui-wave — a keyboard-driven terminal audio editor

USAGE:
    tui-wave [FILE|DIRECTORY]

ARGS:
    FILE         audio file to open (.wav, .flac, .aif, .aiff)
    DIRECTORY    directory for the Files panel to start in

OPTIONS:
    -h, --help       print this help and exit
    -V, --version    print the version and exit

Press F10 or Alt+<mnemonic> for the menu bar, F1 for in-app help.
See DOCUMENTATION.md for the full guide.";

fn main() -> color_eyre::Result<()> {
    // Flags are resolved *first*, before the panic hook, the stale-temp-dir sweep and the
    // terminal init below — `--version` must be a pure read that a packaging script can call
    // safely. Getting this order wrong is what made release.sh hang: any invocation that
    // reaches `terminal::init` puts the tty in raw mode and enters the event loop, where it
    // waits for a keypress forever, and with output redirected there is nothing on screen to
    // say so.
    let invocation = match parse_args(std::env::args().nth(1), |p| p.is_dir()) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("tui-wave: {message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let (open_path, directory) = match invocation {
        Invocation::Version => {
            println!("tui-wave {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Invocation::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Invocation::Run(open_path, directory) => (open_path, directory),
    };

    color_eyre::install()?;
    ui::terminal::install_panic_hook();

    // Before anything can create one: a CDP job directory tagged with this PID can only be the
    // work of an earlier process that was killed before it could clean up, and leaving it there
    // lets a job land in it and read its leftovers as its own output. See the function's own
    // comment for why this belongs here and not in `CdpRunner::new`.
    cdp::runner::sweep_stale_temp_dirs();
    praat::runner::sweep_stale_temp_dirs();

    // A file argument is *queued*, not decoded here: `App::load_file` is what decides whether a
    // file fits in RAM or has to open streamed and read-only (`model::stream`), and decoding
    // eagerly at startup bypassed that entirely — so the command line, the primary way a large
    // take gets opened, always tried to load it fully. It also means an unreadable path now
    // surfaces as a dialog inside the running app rather than aborting before the terminal is
    // even initialized.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `parse_args` takes its directory test as a closure so these can run without touching
    /// the filesystem.
    fn parse(arg: Option<&str>) -> Result<Invocation, String> {
        parse_args(arg.map(str::to_string), |_| false)
    }

    #[test]
    fn version_and_help_flags_do_not_start_the_editor() {
        for flag in ["--version", "-V"] {
            assert_eq!(parse(Some(flag)), Ok(Invocation::Version), "{flag}");
        }
        for flag in ["--help", "-h"] {
            assert_eq!(parse(Some(flag)), Ok(Invocation::Help), "{flag}");
        }
    }

    /// The regression behind the release.sh hang: every invocation that is not a
    /// terminating flag reaches the event loop, so a flag that is silently treated as a
    /// filename hangs a non-interactive caller instead of failing it.
    #[test]
    fn an_unrecognised_flag_is_an_error_not_a_filename() {
        assert!(parse(Some("--verison")).is_err());
        assert!(parse(Some("-x")).is_err());
    }

    #[test]
    fn a_file_argument_is_queued_with_its_containing_directory() {
        assert_eq!(
            parse(Some("take.wav")),
            Ok(Invocation::Run(Some("take.wav".into()), Some(".".into()))),
        );
        assert_eq!(
            parse(Some("/audio/take.wav")),
            Ok(Invocation::Run(Some("/audio/take.wav".into()), Some("/audio".into()))),
        );
    }

    /// A leading `-` marks a flag, but a bare `-` is a plausible filename and has no flag
    /// meaning here, so it must not be swallowed by the flag branch.
    #[test]
    fn a_bare_dash_is_treated_as_a_path() {
        assert_eq!(parse(Some("-")), Ok(Invocation::Run(Some("-".into()), Some(".".into()))));
    }

    #[test]
    fn a_directory_argument_sets_the_files_panel_and_opens_nothing() {
        assert_eq!(
            parse_args(Some("/audio".to_string()), |_| true),
            Ok(Invocation::Run(None, Some("/audio".into()))),
        );
    }

    #[test]
    fn no_argument_opens_the_placeholder_screen() {
        assert_eq!(parse(None), Ok(Invocation::Run(None, None)));
    }
}
