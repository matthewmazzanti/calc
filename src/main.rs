mod engine;
mod history;
mod tui;

fn main() -> std::io::Result<()> {
    tui::run()
}
