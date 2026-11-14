#[path = "../app.rs"]
mod app;
#[path = "../cli.rs"]
mod cli;
#[path = "../store.rs"]
mod store;
#[path = "../ui.rs"]
mod ui;

fn main() -> anyhow::Result<()> {
    cli::run()
}
