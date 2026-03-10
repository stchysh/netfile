#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--cli") {
        netfile_gui::run_cli();
    } else if args.iter().any(|a| a == "--tui") {
        netfile_gui::run_tui();
    } else {
        netfile_gui::run();
    }
}
