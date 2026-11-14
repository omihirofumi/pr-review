mod app;
mod cli;
mod store;
mod ui;

fn main() -> anyhow::Result<()> {
    cli::run()
}
