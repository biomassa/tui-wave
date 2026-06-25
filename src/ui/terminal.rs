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
use ratatui_image::picker::{Picker, ProtocolType};
use std::env::VarError;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Tracks whether we pushed keyboard-enhancement flags, so `restore` (which is also called
/// from the panic hook with no other state) only pops when we actually pushed.
static KBD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// True when a terminal multiplexer is detected via the environment — `TMUX`, GNU screen's
/// `STY`, or a `TERM` starting with `screen` (the multiplexer's own convention when no outer
/// `TERM` override is set). Pulled out as a pure function (rather than inlined into
/// `detect_graphics_picker`) specifically so it's unit-testable without needing a real
/// terminal or mocking `std::env`.
fn detect_multiplexer(get_var: impl Fn(&str) -> Result<String, VarError>) -> bool {
    get_var("TMUX").is_ok() || get_var("STY").is_ok() || get_var("TERM").is_ok_and(|t| t.starts_with("screen"))
}

/// Detects whether the terminal supports a real image-graphics protocol (kitty, Sixel, or
/// iTerm2's) usable for the graphics-mode waveform renderer — `None` if not, including
/// inside a detected multiplexer or when the query round-trip itself fails or times out.
///
/// `ratatui_image`'s own `Halfblocks` fallback deliberately doesn't count as "capable" here:
/// this app's existing eighth-block `WaveformWidget` already does finer sub-row shading than
/// a generic halfblock image downsample would, so falling back to halfblocks would be a
/// strict downgrade from the text renderer, not an upgrade — only a real bitmap protocol is
/// worth switching to.
///
/// tmux/screen are always treated as incapable regardless of what the inner terminal could
/// otherwise do: kitty-graphics passthrough through tmux is widely reported as unreliable
/// even when `allow-passthrough` is nominally enabled, and the query/response round-trip has
/// been observed (kovidgoyal/kitty-adjacent tooling, see opentui#334) to leak into tmux's
/// pane title when not trapped carefully. Querying at all under a multiplexer risks exactly
/// that class of bug for a feature whose payoff (sharper waveform rendering) doesn't justify
/// the risk, so it's skipped entirely rather than attempted-then-distrusted.
fn detect_graphics_picker() -> Option<Picker> {
    if detect_multiplexer(|name| std::env::var(name)) {
        return None;
    }
    let picker = Picker::from_query_stdio().ok()?;
    (picker.protocol_type() != ProtocolType::Halfblocks).then_some(picker)
}

pub fn init() -> color_eyre::Result<(Tui, Option<Picker>)> {
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
    // Queried once at startup, alongside the keyboard-enhancement check above — never
    // re-queried per frame. Must happen after raw mode is enabled (the query reads a raw,
    // unbuffered response from stdin, same general shape as `supports_keyboard_enhancement`).
    let picker = detect_graphics_picker();
    let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    Ok((terminal, picker))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_lookup<'a>(vars: &'a HashMap<&str, &str>) -> impl Fn(&str) -> Result<String, VarError> + 'a {
        move |name| vars.get(name).map(|v| v.to_string()).ok_or(VarError::NotPresent)
    }

    #[test]
    fn detect_multiplexer_finds_tmux() {
        let vars = HashMap::from([("TMUX", "/tmp/tmux-1000/default,123,0")]);
        assert!(detect_multiplexer(env_lookup(&vars)));
    }

    #[test]
    fn detect_multiplexer_finds_gnu_screen() {
        let vars = HashMap::from([("STY", "12345.pts-0.host")]);
        assert!(detect_multiplexer(env_lookup(&vars)));
    }

    #[test]
    fn detect_multiplexer_finds_screen_via_term() {
        let vars = HashMap::from([("TERM", "screen-256color")]);
        assert!(detect_multiplexer(env_lookup(&vars)));
    }

    #[test]
    fn detect_multiplexer_false_outside_a_multiplexer() {
        let vars = HashMap::from([("TERM", "xterm-kitty")]);
        assert!(!detect_multiplexer(env_lookup(&vars)));
    }

    #[test]
    fn detect_multiplexer_false_with_no_relevant_vars_set() {
        let vars = HashMap::new();
        assert!(!detect_multiplexer(env_lookup(&vars)));
    }
}
