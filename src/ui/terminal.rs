use ratatui::crossterm::event::{
    DisableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Tracks whether we pushed keyboard-enhancement flags, so `restore` (which is also called
/// from the panic hook with no other state) only pops when we actually pushed.
static KBD_ENHANCED: AtomicBool = AtomicBool::new(false);

pub fn init() -> color_eyre::Result<Tui> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    // Enable button-event tracking (mode 1002) for press/release/drag,
    // and SGR encoding (mode 1006) for coordinates beyond 223 columns.
    use std::io::Write;
    write!(io::stdout(), "\x1b[?1002h\x1b[?1006h")?;
    // Progressive keyboard enhancement (the "kitty keyboard protocol"): ask the terminal to
    // report modifier+key combinations unambiguously. Without it, kitty *strips* Ctrl from
    // Ctrl+Shift+arrow (the app only sees Shift+arrow) and turns Alt+arrow into composed text,
    // so the fine-step selection bindings never arrive. Supported by kitty, Ghostty, foot,
    // WezTerm and recent xterm; `supports_keyboard_enhancement` round-trips a query so we only
    // push where it actually works (and stays in plain legacy mode everywhere else).
    if supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KBD_ENHANCED.store(true, Ordering::SeqCst);
    }
    io::stdout().flush()?;
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok(terminal)
}

pub fn restore() -> color_eyre::Result<()> {
    use std::io::Write;
    if KBD_ENHANCED.swap(false, Ordering::SeqCst) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
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
