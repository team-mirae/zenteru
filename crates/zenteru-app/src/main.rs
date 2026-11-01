//! zenteru · 젠테루 · ゼンテール — a Metal terminal for macOS with first-class
//! 全角 support, tabs, and an iTerm2-style Python plugin API.

mod app;
mod chord;
mod font;
mod blocks;
mod gutter;
mod settings;
mod splash;
mod magic;
mod music;
mod open_click;
mod platform;
mod render;
mod sudo_auth;
mod textfield;
mod ui;
mod view;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("sudo-auth") => std::process::exit(sudo_auth::run(&args[1..])),
        Some("info" | "version") => print!("{}", splash::render(&splash::cli_info())),
        Some("--version" | "-V") => {
            println!("{} {}", zenteru_text::brand::full_name(), zenteru_text::brand::VERSION);
        }
        Some("--help" | "-h") => {
            println!(
                "{}\n\nUSAGE:\n  zenteru              open zenteru (or focus it)\n  zenteru FILE|DIR…    run scripts in new tabs; open shells in folders\n  zenteru sudo-auth    set up Touch ID / YubiKey for sudo\n  zenteru info         the ゼ splash with version and system facts\n  zenteru --version",
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
            // `zenteru FILE…`: scripts run in a new tab, folders open a
            // shell there (LaunchServices delivers them as openURLs).
            let files: Vec<std::path::PathBuf> = args.iter().filter(|a| !a.starts_with("-psn")).map(std::path::PathBuf::from).collect();
            if let Some(missing) = files.iter().find(|f| !f.exists()) {
                eprintln!("zenteru: {}: no such file or directory", missing.display());
                std::process::exit(1);
            }
            if std::env::var_os("ZENTERU_DEBUG").is_none() && unsafe { isatty(0) } == 1 {
                if let Some(bundle) = platform::app_bundle() {
                    let ok = std::process::Command::new("/usr/bin/open").arg("-a").arg(&bundle).args(&files).status().is_ok_and(|s| s.success());
                    std::process::exit(if ok { 0 } else { 1 });
                }
            }
            view::run(files)
        }
    }
}
