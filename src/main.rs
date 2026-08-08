mod engine;
#[allow(dead_code)] // standalone memory-model sketch, not yet wired into the engine
mod handle_heap;
#[allow(dead_code)] // standalone memory-model sketch, not yet wired into the engine
mod rc_heap;
#[allow(dead_code)] // standalone memory-model sketch, not yet wired into the engine
mod heap;
mod history;
mod tui;

fn main() -> std::io::Result<()> {
    tui::run()
}
