//! zenteru · 젠테루 · ゼンテール — a Metal terminal for macOS with first-class
//! 全角 support, tabs, and an iTerm2-style Python plugin API.

mod app;
mod font;
mod gutter;
mod magic;
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
                "{}\n\nUSAGE:\n  zenteru              open zenteru (or focus it)\n  zenteru sudo-auth    set up Touch ID / YubiKey for sudo\n  zenteru --version",
                zenteru_text::brand::full_name()
            );
        }
        Some(other) if other.starts_with('-') && other != "-psn" && !other.starts_with("-psn_") => {
            eprintln!("zenteru: unknown option {other}; see --help");
            std::process::exit(2);
        }
        _ => {
            // Typed in a shell: hand off to LaunchServices so the app isn't
            // a child of that terminal (and focuses an existing instance).
            unsafe extern "C" {
                fn isatty(fd: i32) -> i32;
            }
            if std::env::var_os("ZENTERU_DEBUG").is_none() && unsafe { isatty(0) } == 1 {
                if let Some(bundle) = platform::app_bundle() {
                    let ok = std::process::Command::new("/usr/bin/open").arg("-a").arg(&bundle).status().is_ok_and(|s| s.success());
                    std::process::exit(if ok { 0 } else { 1 });
                }
            }
            view::run()
        }
    }
}
