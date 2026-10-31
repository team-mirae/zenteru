//! zenteru · 젠테루 · ゼンテール — a Metal terminal for macOS with first-class
//! 全角 support, tabs, and an iTerm2-style Python plugin API.

mod app;
mod font;
mod platform;
mod render;
mod sudo_auth;
mod ui;
mod view;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sudo-auth") => std::process::exit(sudo_auth::run(&args[1..])),
        Some("--version" | "-V") => {
            println!("{} {}", zenteru_text::brand::full_name(), zenteru_text::brand::VERSION);
        }
        Some("--help" | "-h") => {
            println!(
                "{}\n\nUSAGE:\n  zenteru              open a window\n  zenteru sudo-auth    set up Touch ID / YubiKey for sudo\n  zenteru --version",
                zenteru_text::brand::full_name()
            );
        }
        _ => view::run(),
    }
}
