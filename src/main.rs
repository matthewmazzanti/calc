mod engine;
mod history;
#[allow(dead_code)] // standalone memory-model sketch, not yet wired into the engine
mod rc_heap;
mod tui;

fn main() -> std::io::Result<()> {
    tui::run()
}
