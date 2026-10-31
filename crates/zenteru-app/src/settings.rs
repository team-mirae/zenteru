//! Persistent user settings: `~/Library/Application Support/zenteru/settings.json`.

use zenteru_term::json::Json;

fn path() -> std::path::PathBuf {
    crate::platform::support_dir().join("settings.json")
}

pub fn load() -> Json {
    std::fs::read_to_string(path()).ok().and_then(|s| Json::parse(&s).ok()).unwrap_or_else(Json::obj)
}

pub fn set(key: &str, value: impl Into<Json>) {
    let mut s = load();
    if !matches!(s, Json::Obj(_)) {
        s = Json::obj();
    }
    s.set(key, value);
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, s.to_string());
}
