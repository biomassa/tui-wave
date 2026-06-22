use ratatui::crossterm::event::DisableMouseCapture;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> color_eyre::Result<Tui> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    // Enable button-event tracking (mode 1002) for press/release/drag,
    // and SGR encoding (mode 1006) for coordinates beyond 223 columns.
    use std::io::Write;
    write!(io::stdout(), "\x1b[?1002h\x1b[?1006h")?;
    io::stdout().flush()?;
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(terminal)
}

pub fn restore() -> color_eyre::Result<()> {
    use std::io::Write;
    write!(io::stdout(), "\x1b[?1002l\x1b[?1006l")?;
    io::stdout().flush()?;
    execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

/// Ensures the terminal is restored to a usable state even if the app panics,
/// since raw mode + alt-screen left enabled would otherwise lock up the shell.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore();
        original_hook(panic_info);
    }));
}
