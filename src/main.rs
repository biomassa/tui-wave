mod audio;
mod commands;
mod model;
mod ui;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    ui::terminal::install_panic_hook();

    let path = std::env::args().nth(1);
    let document = match path {
        Some(path) => Some(model::io::load_wav(path)?),
        None => None,
    };

    let mut terminal = ui::terminal::init()?;
    let mut app = ui::app::App::new(document);
    let result = app.run(&mut terminal);

    ui::terminal::restore()?;
    result
}
