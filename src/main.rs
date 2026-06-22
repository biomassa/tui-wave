mod ui;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    ui::terminal::install_panic_hook();

    let mut terminal = ui::terminal::init()?;
    let mut app = ui::app::App::new();
    let result = app.run(&mut terminal);

    ui::terminal::restore()?;
    result
}
