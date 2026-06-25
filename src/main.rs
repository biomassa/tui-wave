mod audio;
mod commands;
mod config;
mod model;
mod ui;

use std::path::Path;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    ui::terminal::install_panic_hook();

    let arg = std::env::args().nth(1);
    let (document, directory) = match arg {
        Some(ref p) if Path::new(p).is_dir() => {
            (None, Some(Path::new(p).to_path_buf()))
        }
        Some(ref p) if Path::new(p).is_file() => {
            let doc = Some(model::io::load_wav(p)?);
            let dir = Path::new(p).parent().map(|d| d.to_path_buf());
            (doc, dir)
        }
        Some(p) => {
            // Try as file anyway; load_wav will report the error
            let doc = Some(model::io::load_wav(&p)?);
            let dir = Path::new(&p).parent().map(|d| d.to_path_buf());
            (doc, dir)
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
