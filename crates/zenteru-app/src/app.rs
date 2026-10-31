//! Application state and behaviour, independent of AppKit plumbing (which
//! lives in `view.rs`). Everything here runs on the main thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use zenteru_term::api::{ApiServer, ConnId, Incoming};
use zenteru_term::input::{self, EditAction, Key, Mods, MouseAction, MouseButton};
use zenteru_term::json::Json;
use zenteru_term::session::{Session, SessionConfig};
use zenteru_term::term::{Event, MouseMode, PromptPhase};
use zenteru_text::secret::{SecretMemory, prompt_asks_for_secret};

use crate::platform;
use crate::render::Renderer;
use crate::view::WinHandle;

pub const TAB_BAR_PT: f64 = 38.0;
pub const PAD_PT: f64 = 6.0;
pub const PANEL_PT: f64 = 400.0;
pub const TRAFFIC_LIGHTS_PT: f64 = 78.0;
/// Gutter: relative index (6 cells, fits ゼロ and -99999), a space, the
/// 3-character line code, a space.
pub const GUTTER_CELLS: usize = 11;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Terminal,
    Panel,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub scale: f64,
    pub w: f32,
    pub h: f32,
    pub tabbar_h: f32,
    pub term_x: f32,
    pub term_y: f32,
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub panel_x: f32,
    pub panel_w: f32,
    /// Gutter left edge and width in pixels (0 when off).
    pub gutter_x: f32,
    pub gutter_w: f32,
}

pub struct Tab {
    pub id: String,
    pub session: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TabDrag {
    pub tab_id: String,
    pub grab_dx: f32,
    pub start_x: f32,
    pub mouse_x: f32,
    pub moved: bool,
    pub detached: bool,
    /// Screen position (points) of the last drag event, for window moves.
    pub last_screen: (f64, f64),
}

#[derive(Clone, Debug, Default)]
pub struct Hits {
    /// (tab id, x, w) in pixels.
    pub tabs: Vec<(String, f32, f32)>,
    pub close: Vec<(String, f32, f32, f32, f32)>,
    pub new_tab: (f32, f32, f32, f32),
    /// Panel buttons: (panel id, message id, action id, rect).
    pub actions: Vec<(String, u64, String, [f32; 4])>,
    pub modes: Vec<(String, String, [f32; 4])>,
    pub panel_input: [f32; 4],
    pub panel_close: [f32; 4],
    /// Laid-out input lines (first char, text), the first one shown, and the
    /// text origin — to map clicks to char indices.
    pub panel_lines: Vec<(usize, String)>,
    pub panel_first_line: usize,
    pub panel_text_origin: (f32, f32),
    /// Where the IME candidate window should appear (pixels).
    pub ime: [f32; 4],
}

/// The Black Magic bar (see magic.rs); ⌘⇧P opens the same thing.
#[derive(Default)]
pub struct Palette {
    pub text: String,
    pub selected: usize,
    /// The user picked a suggestion with ↑/↓ (Enter runs that one).
    pub moved: bool,
    pub message: Option<String>,
    pub preedit: String,
}

pub struct Win {
    pub id: String,
    pub handle: WinHandle,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub panel: Option<String>,
    pub focus: Focus,
    pub palette: Option<Palette>,
    pub drag: Option<TabDrag>,
    pub hover_tab: Option<String>,
    pub layout: Layout,
    pub hits: Hits,
    pub dirty: bool,
    /// The gutter is raining: redraw at a gentle frame rate.
    pub rain: bool,
    pub last_frame: Option<Instant>,
    /// Block-matrix size of the gutter rain area (columns, rows).
    pub matrix: (u32, u32),
    /// The gutter is showing rain (a custom driver should send frames).
    pub rain_shown: bool,
    /// A selection drag in the panel input is in progress.
    pub panel_drag: bool,
}

impl Win {
    pub fn active_session(&self) -> Option<&str> {
        self.tabs.get(self.active).map(|t| t.session.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub line: u64,
    pub col: usize,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    /// Extend by letterblocks (double click) or lines (triple click).
    pub unit: u8,
}

impl Selection {
    pub fn ordered(&self) -> (Pos, Pos) {
        if (self.anchor.line, self.anchor.col) <= (self.head.line, self.head.col) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Clone, Debug)]
pub struct MenuItem {
    pub text: String,
    pub delete_before: usize,
    pub display: String,
    pub annotation: String,
    pub source: String,
}

pub struct CompletionMenu {
    pub items: Vec<MenuItem>,
    pub selected: usize,
}

/// Password-field state for a session.
#[derive(Default)]
pub struct PasswordUi {
    pub active: bool,
    pub count: usize,
    /// Typed secret, kept only until Enter to fingerprint it, then wiped.
    buf: String,
}

impl PasswordUi {
    fn wipe(&mut self) {
        // Overwrite before dropping.
        let n = self.buf.len();
        unsafe { self.buf.as_bytes_mut().iter_mut().for_each(|b| *b = 0) };
        self.buf.clear();
        let _ = n;
        self.count = 0;
    }
}

pub struct Sess {
    pub s: Session,
    pub window_id: String,
    pub tab_id: String,
    pub title: String,
    pub pwd: PasswordUi,
    /// The prompt asks for a secret but the tty echoes: warn.
    pub echo_warning: bool,
    /// A raw-mode secret prompt (gh, inquirer, npm…): echo off without
    /// canonical mode, detected from the prompt text; sticky until Enter.
    pub raw_pw: bool,
    /// Absolute line of the raw secret prompt; leaving it ends the field.
    pub raw_pw_line: u64,
    /// The command line contains a remembered password.
    pub secret_in_line: bool,
    pub confirm_enter: bool,
    pub selection: Option<Selection>,
    pub preedit: String,
    pub menu: Option<CompletionMenu>,
    pub comp_pending: Option<(String, Instant)>,
    pub queued: Vec<Vec<u8>>,
    /// Something was typed on the current line (since Enter / ⌃C / ⌃U).
    pub line_typed: bool,
    /// Since when a non-shell program has had the terminal (gutter rain).
    pub app_since: Option<Instant>,
    pub vars: HashMap<String, String>,
    pub scroll_accum: f64,
    pub bell: Option<Instant>,
    pub last_phase: PromptPhase,
    /// The unit pressed on (start, end, unit) while a selection drag is live.
    pub sel_origin: Option<(Pos, Pos, u8)>,
}

#[derive(Clone, Debug)]
pub struct PanelMsg {
    pub id: u64,
    pub role: String,
    pub text: String,
    pub actions: Vec<(String, String)>,
}

pub struct Panel {
    pub id: String,
    pub conn: ConnId,
    pub title: String,
    pub window_id: String,
    pub messages: Vec<PanelMsg>,
    pub status: String,
    pub modes: Vec<String>,
    pub mode: String,
    pub input: String,
    pub cursor: usize,
    pub preedit: String,
    pub scroll: usize,
    pub visible: bool,
    /// Mask the input as ● (setenvvar values, tokens).
    pub input_secret: bool,
    /// Selection anchor in the input (char index); see textfield.rs.
    pub anchor: Option<usize>,
    next_msg: u64,
}

enum PendingCall {
    Completion { session: String },
    Drop { session: String, items: Json, source: String, rest: Vec<String> },
    TitleProvider,
    Command,
    /// `rpc.call` from another plugin: relay the answer.
    Relay { conn: ConnId, id: Json },
}

struct ShellWaiter {
    conn: ConnId,
    req: Json,
    session: String,
    deadline: Instant,
}

struct CallWait {
    kind: PendingCall,
    deadline: Instant,
}

pub struct App {
    pub renderer: Renderer,
    pub windows: Vec<Win>,
    pub sessions: HashMap<String, Sess>,
    pub panels: HashMap<String, Panel>,
    pub api: Option<ApiServer>,
    next_id: u64,
    pub secrets: SecretMemory,
    calls: HashMap<String, CallWait>,
    shell_waiters: Vec<ShellWaiter>,
    pub run_dir: PathBuf,
    pub transient_dir: PathBuf,
    plugins: Vec<(String, Child)>,
    pub cookie: String,
    pub resources: PathBuf,
    pub key_window: Option<String>,
    pending_alerts: Vec<(ConnId, Json, String, String, Vec<String>)>,
    /// Shell-mode line gutter (relative index + line code).
    pub gutter: bool,
    last_minute: i64,
    pub rain_style: crate::gutter::RainStyle,
    /// Latest frame from a custom gutter driver: (width, height, RGBA, when).
    pub gutter_frame: Option<(u32, u32, std::rc::Rc<Vec<u8>>, Instant)>,
    last_gutter_event: (u32, u32, bool, &'static str),
    /// Show the ゼ splash in the first session at launch.
    splash_pending: bool,
    last_screen_emit: Instant,
    screen_emit_due: Vec<String>,
}

fn rand_u64() -> u64 {
    let c = zenteru_term::api::random_cookie();
    u64::from_str_radix(&c[..16], 16).unwrap_or(0x9e37_79b9_7f4a_7c15)
}

impl App {
    pub fn new() -> App {
        let resources = platform::resources_dir();
        let rt = platform::runtime_dir();
        clean_stale_runs(&rt);
        let run_dir = rt.join(format!("run-{}", std::process::id()));
        let transient_dir = run_dir.join("transient");
        let _ = std::fs::create_dir_all(&transient_dir);
        set_private(&rt);
        set_private(&run_dir);
        set_private(&transient_dir);
        let cookie = zenteru_term::api::random_cookie();
        let api = match ApiServer::start(&rt, cookie.clone(), zenteru_text::brand::VERSION) {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("zenteru: plugin API unavailable: {e}");
                None
            }
        };
        App {
            renderer: Renderer::new(std::env::var("ZENTERU_FONT_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(13.0)),
            windows: Vec::new(),
            sessions: HashMap::new(),
            panels: HashMap::new(),
            api,
            next_id: 1,
            secrets: SecretMemory::new(rand_u64()),
            calls: HashMap::new(),
            shell_waiters: Vec::new(),
            run_dir,
            transient_dir,
            plugins: Vec::new(),
            cookie,
            resources,
            key_window: None,
            pending_alerts: Vec::new(),
            gutter: std::env::var("ZENTERU_GUTTER").map_or(true, |v| v != "0"),
            last_minute: 0,
            rain_style: std::env::var("ZENTERU_RAIN")
                .ok()
                .or_else(|| crate::settings::load().get("rain").as_str().map(String::from))
                .and_then(|v| crate::gutter::RainStyle::parse(&v))
                .unwrap_or(crate::gutter::RainStyle::Drops),
            gutter_frame: None,
            last_gutter_event: (0, 0, false, ""),
            splash_pending: std::env::var("ZENTERU_SPLASH").map_or_else(
                |_| crate::settings::load().get("splash").as_bool().unwrap_or(true),
                |v| v != "0",
            ),
            last_screen_emit: Instant::now(),
            screen_emit_due: Vec::new(),
        }
    }

    fn id(&mut self, prefix: char) -> String {
        let n = self.next_id;
        self.next_id += 1;
        format!("{prefix}{n}")
    }

    pub fn win_index(&self, id: &str) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    pub fn win(&mut self, id: &str) -> Option<&mut Win> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn emit(&self, kind: &str, session: Option<&str>, data: Json) {
        if let Some(api) = &self.api {
            api.emit(kind, session, data);
        }
    }

    // ---- windows / tabs / sessions ------------------------------------

    /// Create a window with one new tab. `frame_origin` is a screen point
    /// (for tabs dragged out of another window).
    pub fn new_window(&mut self, command: Option<&str>, cwd: Option<&str>, frame_origin: Option<(f64, f64)>) -> (String, String, String) {
        let wid = self.id('w');
        let handle = crate::view::create_window(&wid, frame_origin, self.renderer.device(), self.windows.len());
        let win = Win {
            id: wid.clone(),
            handle,
            tabs: Vec::new(),
            active: 0,
            panel: None,
            focus: Focus::Terminal,
            palette: None,
            drag: None,
            hover_tab: None,
            layout: Layout::default(),
            hits: Hits::default(),
            dirty: true,
            rain: false,
            last_frame: None,
            matrix: (0, 0),
            rain_shown: false,
            panel_drag: false,
        };
        self.windows.push(win);
        self.relayout(&wid);
        let (tid, sid) = self.new_tab(&wid, command, cwd, None).unwrap_or_default();
        if let Some(w) = self.win(&wid) {
            w.handle.show();
        }
        self.emit("layout", None, Json::obj());
        (wid, tid, sid)
    }

    pub fn new_tab(&mut self, wid: &str, command: Option<&str>, cwd: Option<&str>, index: Option<usize>) -> Option<(String, String)> {
        let wi = self.win_index(wid)?;
        // Inherit the working directory of the current tab.
        let inherit = self.windows[wi].active_session().and_then(|s| self.sessions.get(s)).and_then(|s| s.s.cwd());
        let cwd = cwd.map(String::from).or(inherit);
        let l = self.windows[wi].layout.clone();
        let (cols, rows) = (l.cols.max(10), l.rows.max(3));
        let sid = self.id('s');
        let tid = self.id('t');
        let fonts = self.renderer.fonts(l.scale.max(1.0));
        let cell_px = (fonts.cell_w as u16, fonts.cell_h as u16);
        let si = self.resources.join("shell");
        let sock = self.api.as_ref().map(|a| a.socket_path.clone());
        let session = Session::spawn(SessionConfig {
            id: sid.clone(),
            cols: cols as u16,
            rows: rows as u16,
            cell_px,
            cwd: cwd.as_deref().map(std::path::Path::new),
            command,
            shell_integration: si.is_dir().then_some(si.as_path()),
            dir: self.run_dir.join("sessions").join(&sid),
            api_socket: sock.as_deref(),
            extra_env: vec![],
        });
        let session = match session {
            Ok(s) => s,
            Err(e) => {
                platform::notify("zenteru", &format!("Could not start a shell: {e}"));
                return None;
            }
        };
        self.sessions.insert(
            sid.clone(),
            Sess {
                s: session,
                window_id: wid.to_string(),
                tab_id: tid.clone(),
                title: String::new(),
                pwd: PasswordUi::default(),
                echo_warning: false,
                raw_pw: false,
                raw_pw_line: 0,
                secret_in_line: false,
                confirm_enter: false,
                selection: None,
                preedit: String::new(),
                menu: None,
                comp_pending: None,
                queued: Vec::new(),
                line_typed: false,
                app_since: None,
                vars: HashMap::new(),
                scroll_accum: 0.0,
                bell: None,
                last_phase: PromptPhase::Unknown,
                sel_origin: None,
            },
        );
        if std::mem::take(&mut self.splash_pending) {
            // Launch splash: printed before the shell's first prompt.
            let art = crate::splash::render(&self.splash_info(Some(&sid)));
            self.with_term(&sid, |t| t.feed(art.as_bytes()));
        }
        let w = &mut self.windows[wi];
        let at = index.unwrap_or(if w.tabs.is_empty() { 0 } else { w.active + 1 }).min(w.tabs.len());
        w.tabs.insert(at, Tab { id: tid.clone(), session: sid.clone(), title: None });
        w.active = at;
        w.dirty = true;
        self.emit("new_session", Some(&sid), Json::obj().with("session_id", sid.as_str()).with("tab_id", tid.as_str()).with("window_id", wid));
        self.emit("layout", None, Json::obj());
        Some((tid, sid))
    }

    pub fn find_tab(&self, tid: &str) -> Option<(usize, usize)> {
        self.windows.iter().enumerate().find_map(|(wi, w)| w.tabs.iter().position(|t| t.id == tid).map(|ti| (wi, ti)))
    }

    pub fn close_tab(&mut self, tid: &str) {
        let Some((wi, ti)) = self.find_tab(tid) else { return };
        let tab = self.windows[wi].tabs.remove(ti);
        if let Some(s) = self.sessions.remove(&tab.session) {
            s.s.hangup();
            self.emit("terminate_session", Some(&tab.session), Json::obj().with("session_id", tab.session.as_str()));
        }
        let w = &mut self.windows[wi];
        if w.tabs.is_empty() {
            let w = self.windows.remove(wi);
            w.handle.close();
        } else {
            if w.active >= ti && w.active > 0 {
                w.active -= 1;
            }
            w.dirty = true;
        }
        self.emit("layout", None, Json::obj());
    }

    pub fn close_window(&mut self, wid: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        let ids: Vec<String> = self.windows[wi].tabs.iter().map(|t| t.id.clone()).collect();
        for t in ids {
            self.close_tab(&t);
        }
    }

    pub fn activate_tab(&mut self, tid: &str) {
        let Some((wi, ti)) = self.find_tab(tid) else { return };
        let w = &mut self.windows[wi];
        w.active = ti;
        w.dirty = true;
        w.handle.focus();
        let sid = w.tabs[ti].session.clone();
        self.emit("focus", Some(&sid), Json::obj().with("tab_id", tid).with("session_id", sid.as_str()).with("active", true));
    }

    /// Move a tab to `to_win` at `index` ("new" window when `to_win` is None).
    pub fn move_tab(&mut self, tid: &str, to_win: Option<&str>, index: usize, origin: Option<(f64, f64)>) {
        let Some((wi, ti)) = self.find_tab(tid) else { return };
        let from_id = self.windows[wi].id.clone();
        let target = match to_win {
            Some(t) if t == from_id => {
                let w = &mut self.windows[wi];
                let tab = w.tabs.remove(ti);
                let idx = index.min(w.tabs.len());
                w.tabs.insert(idx, tab);
                w.active = idx;
                w.dirty = true;
                self.emit("layout", None, Json::obj());
                return;
            }
            Some(t) => t.to_string(),
            None => {
                if self.windows[wi].tabs.len() == 1 {
                    return;
                }
                let wid = self.id('w');
                let handle = crate::view::create_window(&wid, origin, self.renderer.device(), self.windows.len());
                self.windows.push(Win {
                    id: wid.clone(),
                    handle,
                    tabs: Vec::new(),
                    active: 0,
                    panel: None,
                    focus: Focus::Terminal,
                    palette: None,
                    drag: None,
                    hover_tab: None,
                    layout: Layout::default(),
                    hits: Hits::default(),
                    dirty: true,
                    rain: false,
                    last_frame: None,
                    matrix: (0, 0),
                    rain_shown: false,
                    panel_drag: false,
                });
                wid
            }
        };
        let Some(wi) = self.windows.iter().position(|w| w.id == from_id) else { return };
        let tab = self.windows[wi].tabs.remove(ti);
        let sid = tab.session.clone();
        if self.windows[wi].tabs.is_empty() {
            let w = self.windows.remove(wi);
            w.handle.close();
        } else {
            let w = &mut self.windows[wi];
            w.active = w.active.min(w.tabs.len() - 1);
            w.dirty = true;
        }
        let Some(ti2) = self.win_index(&target) else { return };
        let w = &mut self.windows[ti2];
        let idx = index.min(w.tabs.len());
        w.tabs.insert(idx, tab);
        w.active = idx;
        w.dirty = true;
        w.handle.show();
        if let Some(s) = self.sessions.get_mut(&sid) {
            s.window_id = target.clone();
        }
        self.relayout(&target);
        self.emit("layout", None, Json::obj());
    }

    /// Recompute the layout from the view size and resize every session.
    pub fn relayout(&mut self, wid: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        let (wpt, hpt, scale) = self.windows[wi].handle.size();
        let panel_visible = self.windows[wi].panel.as_ref().and_then(|p| self.panels.get(p)).is_some_and(|p| p.visible);
        let fonts = self.renderer.fonts(scale);
        let (cw, ch) = (fonts.cell_w, fonts.cell_h);
        let s = scale as f32;
        let w = wpt as f32 * s;
        let h = hpt as f32 * s;
        let tabbar_h = (TAB_BAR_PT as f32 * s).round();
        let pad = (PAD_PT as f32 * s).round();
        let panel_w = if panel_visible { (PANEL_PT as f32 * s).min(w * 0.5).round() } else { 0.0 };
        let gutter_w = if self.gutter { GUTTER_CELLS as f32 * cw } else { 0.0 };
        let cols = (((w - panel_w - 2.0 * pad - gutter_w) / cw).floor() as usize).max(10);
        let rows = (((h - tabbar_h - 2.0 * pad) / ch).floor() as usize).max(3);
        let layout = Layout {
            scale,
            w,
            h,
            tabbar_h,
            term_x: pad + gutter_w,
            term_y: tabbar_h + pad,
            cols,
            rows,
            cell_w: cw,
            cell_h: ch,
            panel_x: w - panel_w,
            panel_w,
            gutter_x: pad,
            gutter_w,
        };
        let win = &mut self.windows[wi];
        win.handle.set_drawable_size(w, h, scale);
        let changed = win.layout.cols != cols || win.layout.rows != rows || win.layout.scale != scale;
        win.layout = layout;
        win.dirty = true;
        if changed {
            let sids: Vec<String> = win.tabs.iter().map(|t| t.session.clone()).collect();
            for sid in sids {
                if let Some(s) = self.sessions.get(&sid) {
                    s.s.resize(cols as u16, rows as u16, (cw as u16, ch as u16));
                }
            }
        }
    }

    pub fn set_font_size(&mut self, pt: f64) {
        self.renderer.set_font_size(pt);
        let ids: Vec<String> = self.windows.iter().map(|w| w.id.clone()).collect();
        for id in ids {
            if let Some(w) = self.win(&id) {
                w.layout.cols = 0;
            }
            self.relayout(&id);
        }
    }

    // ---- the main loop ------------------------------------------------

    pub fn tick(&mut self) {
        self.poll_api();
        self.poll_sessions();
        self.expire_waits();
        // The gutter clock: redraw when the minute turns.
        if self.gutter {
            let (_, _, minute) = crate::gutter::local_clock();
            if minute != self.last_minute {
                self.last_minute = minute;
                self.windows.iter_mut().for_each(|w| w.dirty = true);
            }
        }
        // Rain: ~30 fps in the key window, ~10 fps elsewhere.
        for w in &mut self.windows {
            if w.rain {
                let every = if w.handle.is_key() { 33 } else { 100 };
                if w.last_frame.is_none_or(|t| t.elapsed().as_millis() >= every) {
                    w.dirty = true;
                }
            }
        }
        let ids: Vec<String> = self.windows.iter().map(|w| w.id.clone()).collect();
        for id in ids {
            let needs = {
                let Some(w) = self.windows.iter().find(|w| w.id == id) else { continue };
                w.dirty || w.tabs.iter().any(|t| self.sessions.get(&t.session).is_some_and(|s| s.s.dirty.load(Ordering::Acquire)))
            };
            if needs {
                crate::ui::draw(self, &id);
            }
        }
        // Tell custom gutter drivers about size / activity changes.
        let key = self.key_window.clone().and_then(|k| self.windows.iter().find(|w| w.id == k)).or(self.windows.first());
        if let Some(w) = key {
            let now = (w.matrix.0, w.matrix.1, w.rain_shown, self.rain_style.name());
            if now != self.last_gutter_event {
                self.last_gutter_event = now;
                self.emit("gutter", None, Json::obj().with("width", now.0).with("height", now.1).with("active", now.2).with("style", now.3));
            }
        }
        if !self.pending_alerts.is_empty() {
            let alerts = std::mem::take(&mut self.pending_alerts);
            for (conn, id, title, body, buttons) in alerts {
                let idx = crate::view::run_alert(&title, &body, &buttons);
                if let Some(api) = &self.api {
                    api.reply(conn, &id, Ok(Json::obj().with("index", idx)));
                }
            }
        }
    }

    fn poll_sessions(&mut self) {
        let mut exited = Vec::new();
        let sids: Vec<String> = self.sessions.keys().cloned().collect();
        let screen_subs = self.api.as_ref().is_some_and(|a| a.has_subscribers("screen_update"));
        for sid in sids {
            let Some(s) = self.sessions.get_mut(&sid) else { continue };
            if s.s.exited.load(Ordering::Acquire) {
                exited.push(s.tab_id.clone());
                continue;
            }
            let (events, phase, cmd, status, cwd) = {
                let mut t = s.s.term.lock().unwrap();
                let ev = t.take_events();
                (ev, t.prompt.phase, t.prompt.command.clone(), t.prompt.exit_status, t.prompt.cwd.clone())
            };
            // Password mode: echo off + canonical input.
            let pw = s.s.password_mode() || s.raw_pw;
            if pw != s.pwd.active {
                if !pw {
                    s.pwd.wipe();
                }
                s.pwd.active = pw;
                let w = s.window_id.clone();
                if let Some(api) = &self.api {
                    api.emit("password_mode", Some(&sid), Json::obj().with("session_id", sid.as_str()).with("active", pw));
                }
                if let Some(win) = self.windows.iter_mut().find(|x| x.id == w) {
                    win.dirty = true;
                }
            }
            if s.s.dirty.load(Ordering::Acquire) {
                // Warnings depend on what's on screen.
                let (line_before_cursor, edit) = {
                    let t = s.s.term.lock().unwrap();
                    let row = t.screen().get(t.cursor.y).map(|r| {
                        let mut txt = String::new();
                        for c in r.cells.iter().take(t.cursor.x) {
                            if !c.is_spacer() {
                                c.push_text(&mut txt);
                            }
                        }
                        txt
                    });
                    (row.unwrap_or_default(), t.edit_line().0)
                };
                let echo = s.s.echo_on();
                let (alt, cur_line, editing) = {
                    let t = s.s.term.lock().unwrap();
                    (t.alt_screen(), t.cursor_abs().0, t.prompt.phase == PromptPhase::Editing)
                };
                if echo || alt || (s.raw_pw && (cur_line != s.raw_pw_line || (editing && s.last_phase != PromptPhase::Editing))) {
                    s.raw_pw = false;
                } else if !s.raw_pw && prompt_asks_for_secret(&line_before_cursor) {
                    s.raw_pw_line = cur_line;
                    // Echo off, not canonical (else password_mode() caught it):
                    // a TUI prompt asking for a secret. It draws its own
                    // asterisks, so detection happens before typing starts.
                    s.raw_pw = true;
                    s.s.dirty.store(true, Ordering::Release);
                }
                s.echo_warning = !pw && prompt_asks_for_secret(&line_before_cursor) && echo;
                let matched = !pw && self.secrets.matches(&edit);
                if !matched {
                    s.confirm_enter = false;
                }
                s.secret_in_line = matched;
                if screen_subs {
                    self.screen_emit_due.push(sid.clone());
                }
            }
            for e in events {
                match e {
                    Event::Title(t) => {
                        s.title = t;
                        if let Some(api) = &self.api {
                            api.emit("layout", None, Json::obj());
                        }
                    }
                    Event::Bell => s.bell = Some(Instant::now()),
                    Event::Clipboard(t) => platform::set_clipboard_text(&t),
                    Event::Cwd(_) => {}
                    Event::Notify(t, b) => platform::notify(&t, &b),
                    Event::UserVar(n, v) => {
                        s.vars.insert(format!("user.{n}"), v.clone());
                        if let Some(api) = &self.api {
                            api.emit("variable", Some(&sid), Json::obj().with("session_id", sid.as_str()).with("name", format!("user.{n}")).with("value", v));
                        }
                    }
                    Event::Prompt(p) => {
                        if p == PromptPhase::Running {
                            if let Some(c) = &cmd {
                                if !c.is_empty() {
                                    s.s.history.push(c.clone());
                                    if s.s.history.len() > 200 {
                                        s.s.history.remove(0);
                                    }
                                }
                            }
                        }
                        if let Some(api) = &self.api {
                            let state = phase_name(p);
                            api.emit(
                                "prompt",
                                Some(&sid),
                                Json::obj()
                                    .with("session_id", sid.as_str())
                                    .with("state", state)
                                    .with("command", cmd.clone())
                                    .with("exit_status", status.map(|v| v as i64))
                                    .with("cwd", cwd.clone()),
                            );
                        }
                    }
                    Event::Completions(payload) => {
                        let mut done = Vec::new();
                        for (i, w) in self.shell_waiters.iter().enumerate() {
                            if w.session == sid {
                                done.push(i);
                            }
                        }
                        for i in done.into_iter().rev() {
                            let w = self.shell_waiters.remove(i);
                            let v = Json::parse(&payload).unwrap_or_else(|_| Json::obj().with("candidates", Json::Arr(vec![])));
                            if let Some(api) = &self.api {
                                api.reply(w.conn, &w.req, Ok(v));
                            }
                        }
                    }
                }
            }
            s.last_phase = phase;
        }
        if !self.screen_emit_due.is_empty() && self.last_screen_emit.elapsed() > Duration::from_millis(33) {
            let due = std::mem::take(&mut self.screen_emit_due);
            let mut seen = std::collections::HashSet::new();
            for sid in due {
                if seen.insert(sid.clone()) {
                    self.emit("screen_update", Some(&sid), Json::obj().with("session_id", sid.as_str()));
                }
            }
            self.last_screen_emit = Instant::now();
        }
        for tid in exited {
            self.close_tab(&tid);
        }
    }

    fn expire_waits(&mut self) {
        let now = Instant::now();
        let expired: Vec<String> = self.calls.iter().filter(|(_, c)| c.deadline <= now).map(|(k, _)| k.clone()).collect();
        for id in expired {
            self.call_done(&id, Ok(Json::Null));
        }
        let mut i = 0;
        while i < self.shell_waiters.len() {
            if self.shell_waiters[i].deadline <= now {
                let w = self.shell_waiters.remove(i);
                if let Some(api) = &self.api {
                    api.reply(w.conn, &w.req, Err(("unsupported", "no shell integration answered".into())));
                }
            } else {
                i += 1;
            }
        }
    }

    // ---- keyboard -----------------------------------------------------

    /// Handle a key press. Returns false when it should go to the IME
    /// (`interpretKeyEvents`) instead.
    pub fn key_down(&mut self, wid: &str, key: Key, mods: Mods, has_marked: bool) -> bool {
        let Some(wi) = self.win_index(wid) else { return true };
        self.windows[wi].dirty = true;

        if self.windows[wi].palette.is_some() {
            if has_marked {
                // Composing (한글 / かな) inside the bar: the IME owns the keys.
                return false;
            }
            return self.magic_key(wid, &key, mods);
        }
        if let Some(handler) = self.keybinding_handler(&key, mods) {
            self.invoke_command(wid, &handler);
            return true;
        }
        if self.windows[wi].focus == Focus::Panel {
            return self.panel_key(wid, &key, mods, has_marked);
        }
        let Some(sid) = self.windows[wi].active_session().map(String::from) else { return true };

        // Completion menu navigation.
        if self.sessions.get(&sid).is_some_and(|s| s.menu.is_some()) && !has_marked {
            if self.menu_key(&sid, &key, mods) {
                return true;
            }
        }

        let (alt_screen, modes) = {
            let Some(s) = self.sessions.get(&sid) else { return true };
            let t = s.s.term.lock().unwrap();
            (t.alt_screen(), t.modes.clone())
        };

        // Password guard on a normal field.
        if let Some(s) = self.sessions.get_mut(&sid) {
            if s.secret_in_line && !s.pwd.active {
                match key {
                    Key::Enter if !s.confirm_enter => {
                        s.confirm_enter = true;
                        return true;
                    }
                    Key::Escape => {
                        s.s.write(&[0x15]);
                        s.secret_in_line = false;
                        s.confirm_enter = false;
                        return true;
                    }
                    _ => {}
                }
            }
        }

        // Function keys go through the input method first (JIS Korean uses
        // F6–F10); doCommandBySelector encodes them if it passes.
        if matches!(key, Key::F(_)) {
            return false;
        }
        if has_marked {
            if key == Key::Tab && self.ime_provider_for_current().is_some() {
                self.start_completion(&sid, mods.shift, true);
                return true;
            }
            return false;
        }

        // Natural text editing.
        let action = match (&key, mods.alt, mods.cmd) {
            (Key::Left, true, false) => Some(EditAction::WordLeft),
            (Key::Right, true, false) => Some(EditAction::WordRight),
            (Key::Backspace, true, false) => Some(EditAction::DeleteWordBack),
            (Key::Delete, true, false) => Some(EditAction::DeleteWordForward),
            (Key::Left, false, true) => Some(EditAction::LineStart),
            (Key::Right, false, true) => Some(EditAction::LineEnd),
            (Key::Backspace, false, true) => Some(EditAction::DeleteToLineStart),
            (Key::Delete, false, true) => Some(EditAction::DeleteToLineEnd),
            _ => None,
        };
        if let Some(a) = action {
            let bytes = {
                let s = &self.sessions[&sid];
                let t = s.s.term.lock().unwrap();
                let (line, cur) = t.edit_line();
                input::encode_edit(a, &line, cur, alt_screen)
            };
            self.send_user(&sid, &bytes);
            return true;
        }
        if mods.cmd {
            return match key {
                Key::Up => {
                    self.with_term(&sid, |t| t.jump_to_prompt(-1));
                    true
                }
                Key::Down => {
                    self.with_term(&sid, |t| t.jump_to_prompt(1));
                    true
                }
                _ => true,
            };
        }

        if key == Key::Tab && !mods.ctrl && !alt_screen {
            let shell_editing = self.sessions.get(&sid).is_some_and(|s| {
                s.s.term.lock().unwrap().prompt.phase == PromptPhase::Editing && s.s.is_shell_foreground() && !s.pwd.active
            });
            if shell_editing && self.completion_provider().is_some() {
                self.start_completion(&sid, mods.shift, false);
                return true;
            }
        }

        let special = !matches!(key, Key::Text(_));
        if special || mods.ctrl {
            let bytes = input::encode_key(&key, mods, &modes);
            if let Some(s) = self.sessions.get_mut(&sid) {
                if s.pwd.active {
                    match key {
                        Key::Backspace => {
                            s.pwd.buf.pop();
                            s.pwd.count = s.pwd.count.saturating_sub(1);
                        }
                        Key::Enter => {
                            let secret = std::mem::take(&mut s.pwd.buf);
                            self.secrets.remember(&secret);
                            let s = self.sessions.get_mut(&sid).unwrap();
                            s.pwd.wipe();
                            s.raw_pw = false;
                        }
                        Key::Text(ref t) if mods.ctrl && (t == "u" || t == "c") => s.pwd.wipe(),
                        _ => {}
                    }
                }
            }
            self.send_user(&sid, &bytes);
            return true;
        }
        false
    }

    /// Encode a key the IME passed on (doCommandBySelector:) and send it.
    pub fn encode_and_send(&mut self, wid: &str, key: &Key, mods: Mods) {
        let Some(wi) = self.win_index(wid) else { return };
        if self.windows[wi].palette.is_some() || self.windows[wi].focus == Focus::Panel {
            // Re-run through our own handling without the IME.
            self.key_down(wid, key.clone(), mods, false);
            return;
        }
        let Some(sid) = self.windows[wi].active_session().map(String::from) else { return };
        let modes = self.sessions[&sid].s.term.lock().unwrap().modes.clone();
        let bytes = input::encode_key(key, mods, &modes);
        self.send_user(&sid, &bytes);
    }

    pub fn focus_event(&mut self, wid: &str, active: bool) {
        let sid = self.win(wid).and_then(|w| w.active_session()).map(String::from);
        if let Some(sid) = &sid {
            if let Some(s) = self.sessions.get(sid) {
                let fe = s.s.term.lock().unwrap().modes.focus_events;
                if fe {
                    s.s.write(if active { b"\x1b[I" } else { b"\x1b[O" });
                }
            }
        }
        self.emit("focus", sid.as_deref(), Json::obj().with("window_id", wid).with("active", active));
    }

    pub fn splash_info(&mut self, sid: Option<&str>) -> crate::splash::Info {
        let scale = self.windows.first().map(|w| w.layout.scale).filter(|s| *s > 0.0).unwrap_or(2.0);
        let font = self.renderer.fonts(scale).describe();
        let shell = sid.and_then(|s| self.sessions.get(s)).map(|s| s.s.shell.clone()).filter(|s| !s.is_empty());
        let mut plugins: Vec<String> = self.plugins.iter().map(|(n, _)| n.clone()).collect();
        plugins.sort();
        crate::splash::Info { version: zenteru_text::brand::VERSION.to_string(), shell, font: Some(font), plugins }
    }

    /// `@info` / `@version` / `@zenteru`: print the splash above the prompt.
    pub fn show_splash(&mut self, wid: &str) {
        let Some(sid) = self.win(wid).and_then(|w| w.active_session()).map(String::from) else { return };
        let art = crate::splash::render(&self.splash_info(Some(&sid)));
        let Some(s) = self.sessions.get(&sid) else { return };
        let (alt, editing) = {
            let t = s.s.term.lock().unwrap();
            (t.alt_screen(), t.prompt.phase == PromptPhase::Editing && t.prompt.input_start.is_some())
        };
        if alt {
            return;
        }
        if editing && matches!(s.s.shell.as_str(), "zsh" | "bash") {
            // The shell prints it above the prompt and redraws the command
            // line, so nothing lands in history (shell/zenteru.zsh).
            let path = s.s.dir.join("info.ans");
            if std::fs::write(&path, art.as_bytes()).is_ok() {
                s.s.write(b"\x1b[9236~");
                return;
            }
        }
        self.with_term(&sid, |t| t.feed(art.as_bytes()));
    }

    pub fn set_rain_style(&mut self, style: crate::gutter::RainStyle) {
        self.rain_style = style;
        crate::settings::set("rain", style.name());
        self.windows.iter_mut().for_each(|w| w.dirty = true);
    }

    /// Quit once the last window is gone (after at least one existed).
    pub fn quit_when_empty(&self) -> bool {
        self.next_id > 1
    }

    /// Text from the IME / keyboard (insertText:).
    pub fn insert_text(&mut self, wid: &str, text: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        self.windows[wi].dirty = true;
        if self.windows[wi].palette.is_some() {
            self.magic_text(wid, text);
            return;
        }
        if let Some(sid) = self.windows[wi].active_session().map(String::from) {
            if self.windows[wi].focus == Focus::Terminal && self.magic_at(wid, &sid, text) {
                return;
            }
        }
        if self.windows[wi].focus == Focus::Panel {
            if let Some(p) = self.windows[wi].panel.clone().and_then(|id| self.panels.get_mut(&id)) {
                p.insert(text);
                p.preedit.clear();
            }
            return;
        }
        let Some(sid) = self.windows[wi].active_session().map(String::from) else { return };
        if let Some(s) = self.sessions.get_mut(&sid) {
            s.preedit.clear();
            s.menu = None;
            if s.pwd.active {
                s.pwd.buf.push_str(text);
                s.pwd.count += zenteru_text::text::grapheme_count(text);
            }
        }
        self.send_user(&sid, text.as_bytes());
    }

    pub fn set_preedit(&mut self, wid: &str, text: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        self.windows[wi].dirty = true;
        if let Some(p) = &mut self.windows[wi].palette {
            p.preedit = text.to_string();
            return;
        }
        if self.windows[wi].focus == Focus::Panel {
            if let Some(p) = self.windows[wi].panel.clone().and_then(|id| self.panels.get_mut(&id)) {
                p.preedit = text.to_string();
            }
            return;
        }
        if let Some(sid) = self.windows[wi].active_session().map(String::from) {
            if let Some(s) = self.sessions.get_mut(&sid) {
                s.preedit = text.to_string();
            }
        }
    }

    /// Bytes typed by the user: scroll to bottom, clear selection, queue
    /// behind a pending completion.
    pub fn send_user(&mut self, sid: &str, bytes: &[u8]) {
        let Some(s) = self.sessions.get_mut(sid) else { return };
        if s.comp_pending.is_some() {
            s.queued.push(bytes.to_vec());
            return;
        }
        s.selection = None;
        if bytes.iter().any(|&b| matches!(b, b'\r' | b'\n' | 0x03 | 0x15)) {
            s.line_typed = false;
        } else if bytes.iter().any(|&b| b >= 0x20 && b != 0x7f) {
            s.line_typed = true;
        }
        s.s.term.lock().unwrap().scroll_to_bottom();
        if let Some(api) = &self.api {
            if !s.pwd.active && api.has_subscribers("keystroke") {
                api.emit(
                    "keystroke",
                    Some(sid),
                    Json::obj().with("session_id", sid).with("characters", String::from_utf8_lossy(bytes).into_owned()).with("modifiers", Json::Arr(vec![])).with("key_code", 0),
                );
            }
        }
        s.s.write(bytes);
    }

    fn with_term(&mut self, sid: &str, f: impl FnOnce(&mut zenteru_term::term::Terminal)) {
        if let Some(s) = self.sessions.get(sid) {
            f(&mut s.s.term.lock().unwrap());
            s.s.dirty.store(true, Ordering::Release);
        }
    }

    // ---- completion ---------------------------------------------------

    fn completion_provider(&self) -> Option<zenteru_term::api::Registration> {
        let api = self.api.as_ref()?;
        let regs = api.registrations("completion_provider");
        if regs.is_empty() {
            return None;
        }
        let src = platform::input_source_id();
        let prio = |r: &zenteru_term::api::Registration| r.params.get("priority").as_i64().unwrap_or(0);
        let matching = |r: &zenteru_term::api::Registration| {
            r.params.get("input_sources").as_arr().iter().filter_map(Json::as_str).any(|p| p != "*" && src.starts_with(p))
        };
        let wildcard = |r: &zenteru_term::api::Registration| r.params.get("input_sources").as_arr().iter().filter_map(Json::as_str).any(|p| p == "*");
        regs.iter().filter(|r| matching(r)).max_by_key(|r| prio(r)).or_else(|| regs.iter().filter(|r| wildcard(r)).max_by_key(|r| prio(r))).cloned()
    }

    fn ime_provider_for_current(&self) -> Option<zenteru_term::api::Registration> {
        let api = self.api.as_ref()?;
        let src = platform::input_source_id();
        api.registrations("completion_provider")
            .into_iter()
            .find(|r| r.params.get("input_sources").as_arr().iter().filter_map(Json::as_str).any(|p| p != "*" && src.starts_with(p)))
    }

    fn start_completion(&mut self, sid: &str, shift: bool, with_preedit: bool) {
        let Some(reg) = (if with_preedit { self.ime_provider_for_current() } else { self.completion_provider() }) else { return };
        let Some(s) = self.sessions.get(sid) else { return };
        let (line, cursor, cwd) = {
            let t = s.s.term.lock().unwrap();
            let (l, c) = t.edit_line();
            (l, c, t.prompt.cwd.clone())
        };
        let history: Vec<Json> = s.s.history.iter().rev().take(50).rev().map(|h| Json::from(h.as_str())).collect();
        let args = Json::obj()
            .with("session_id", sid)
            .with("shift", shift)
            .with("line", line)
            .with("cursor", cursor)
            .with("preedit", s.preedit.clone())
            .with("input_source_id", platform::input_source_id())
            .with("cwd", cwd.or_else(|| s.s.cwd()))
            .with("shell", s.s.shell.clone())
            .with("history", Json::Arr(history));
        let timeout = reg.params.get("timeout_ms").as_i64().unwrap_or(400).clamp(50, 400) as u64;
        let Some(api) = &self.api else { return };
        let id = api.call(reg.conn, &reg.handler, args);
        self.calls.insert(id.clone(), CallWait { kind: PendingCall::Completion { session: sid.to_string() }, deadline: Instant::now() + Duration::from_millis(timeout) });
        if let Some(s) = self.sessions.get_mut(sid) {
            s.comp_pending = Some((id, Instant::now()));
        }
    }

    fn finish_completion(&mut self, sid: &str, result: Json) {
        let Some(s) = self.sessions.get_mut(sid) else { return };
        s.comp_pending = None;
        let had_preedit = !s.preedit.is_empty();
        let action = result.get("action").as_str().unwrap_or("passthrough").to_string();
        match action.as_str() {
            "insert" => {
                let item = MenuItem {
                    text: result.get("text").as_str().unwrap_or("").to_string(),
                    delete_before: result.get("delete_before").as_i64().unwrap_or(0).max(0) as usize,
                    display: String::new(),
                    annotation: String::new(),
                    source: String::new(),
                };
                self.apply_item(sid, &item);
            }
            "menu" => {
                let items: Vec<MenuItem> = result
                    .get("items")
                    .as_arr()
                    .iter()
                    .map(|i| MenuItem {
                        text: i.get("text").as_str().unwrap_or("").to_string(),
                        delete_before: i.get("delete_before").as_i64().unwrap_or(0).max(0) as usize,
                        display: i.get("display").as_str().or(i.get("text").as_str()).unwrap_or("").to_string(),
                        annotation: i.get("annotation").as_str().unwrap_or("").to_string(),
                        source: i.get("source").as_str().unwrap_or("").to_string(),
                    })
                    .filter(|i| !i.text.is_empty())
                    .take(200)
                    .collect();
                if items.is_empty() {
                    self.passthrough_tab(sid, had_preedit);
                } else {
                    let sel = result.get("selected").as_i64().unwrap_or(0).max(0) as usize;
                    let s = self.sessions.get_mut(sid).unwrap();
                    s.menu = Some(CompletionMenu { selected: sel.min(items.len() - 1), items });
                }
            }
            _ => self.passthrough_tab(sid, had_preedit),
        }
        let queued = self.sessions.get_mut(sid).map(|s| std::mem::take(&mut s.queued)).unwrap_or_default();
        for q in queued {
            self.send_user(sid, &q);
        }
        self.mark_session_dirty(sid);
    }

    fn passthrough_tab(&mut self, sid: &str, had_preedit: bool) {
        if had_preedit {
            // The IME keeps Tab: re-deliver it through interpretKeyEvents.
            if let Some(s) = self.sessions.get(sid) {
                let wid = s.window_id.clone();
                if let Some(w) = self.windows.iter().find(|w| w.id == wid) {
                    w.handle.redeliver_tab();
                }
            }
            return;
        }
        if let Some(s) = self.sessions.get_mut(sid) {
            s.comp_pending = None;
            s.s.write(b"\t");
        }
    }

    fn apply_item(&mut self, sid: &str, item: &MenuItem) {
        let Some(s) = self.sessions.get_mut(sid) else { return };
        if !s.preedit.is_empty() {
            s.preedit.clear();
            let wid = s.window_id.clone();
            if let Some(w) = self.windows.iter().find(|w| w.id == wid) {
                w.handle.discard_marked_text();
            }
        }
        let s = self.sessions.get_mut(sid).unwrap();
        let mut bytes = vec![0x7f; item.delete_before];
        bytes.extend_from_slice(item.text.as_bytes());
        s.menu = None;
        s.s.write(&bytes);
    }

    fn menu_key(&mut self, sid: &str, key: &Key, mods: Mods) -> bool {
        let Some(s) = self.sessions.get_mut(sid) else { return false };
        let Some(m) = &mut s.menu else { return false };
        let n = m.items.len();
        match key {
            Key::Tab if mods.shift => m.selected = (m.selected + n - 1) % n,
            Key::Tab | Key::Down => m.selected = (m.selected + 1) % n,
            Key::Up => m.selected = (m.selected + n - 1) % n,
            Key::Enter | Key::Right => {
                let item = m.items[m.selected].clone();
                self.apply_item(sid, &item);
            }
            Key::Escape => s.menu = None,
            _ => {
                s.menu = None;
                return false;
            }
        }
        self.mark_session_dirty(sid);
        true
    }

    pub fn menu_click(&mut self, sid: &str, idx: usize) {
        let item = self.sessions.get(sid).and_then(|s| s.menu.as_ref()).and_then(|m| m.items.get(idx)).cloned();
        if let Some(item) = item {
            self.apply_item(sid, &item);
        }
    }

    fn mark_session_dirty(&mut self, sid: &str) {
        if let Some(s) = self.sessions.get(sid) {
            s.s.dirty.store(true, Ordering::Release);
            let wid = s.window_id.clone();
            if let Some(w) = self.win(&wid) {
                w.dirty = true;
            }
        }
    }

    // ---- registrations / commands -------------------------------------

    fn keybinding_handler(&self, key: &Key, mods: Mods) -> Option<String> {
        let api = self.api.as_ref()?;
        if !(mods.cmd || mods.ctrl) {
            return None;
        }
        let ch = match key {
            Key::Text(t) => t.to_lowercase(),
            _ => return None,
        };
        api.registrations("command").into_iter().find_map(|r| {
            let kb = r.params.get("keybinding").as_str()?.to_lowercase();
            let parts: Vec<&str> = kb.split('+').collect();
            let k = *parts.last()?;
            let want = |m: &str| parts.contains(&m);
            (k == ch
                && want("cmd") == mods.cmd
                && want("shift") == mods.shift
                && (want("ctrl") || want("control")) == mods.ctrl
                && (want("alt") || want("opt") || want("option")) == mods.alt)
                .then(|| r.handler.clone())
        })
    }

    pub fn invoke_command(&mut self, wid: &str, handler: &str) {
        self.invoke_command_with(wid, handler, None);
    }

    /// Call a registered handler (fire and forget).
    pub fn call_handler(&mut self, reg: &zenteru_term::api::Registration, args: Json) {
        let Some(api) = &self.api else { return };
        let id = api.call(reg.conn, &reg.handler, args);
        self.calls.insert(id, CallWait { kind: PendingCall::Command, deadline: Instant::now() + Duration::from_secs(5) });
    }

    /// Run a palette command; `line` is the text after the command name
    /// when it came from Black Magic (`@name args…`).
    pub fn invoke_command_with(&mut self, wid: &str, handler: &str, line: Option<&str>) {
        if let Some(b) = handler.strip_prefix("zenteru.") {
            self.builtin(wid, b);
            return;
        }
        let Some(api) = &self.api else { return };
        let Some(reg) = api.registration(handler) else { return };
        let w = self.windows.iter().find(|w| w.id == wid);
        let tid = w.and_then(|w| w.tabs.get(w.active)).map(|t| t.id.clone());
        let sid = w.and_then(|w| w.active_session()).map(String::from);
        let mut args = Json::obj().with("window_id", wid).with("tab_id", tid).with("session_id", sid);
        if let Some(line) = line {
            let argv: Vec<Json> = line.split_whitespace().map(Json::from).collect();
            args.set("line", line);
            args.set("argv", Json::Arr(argv));
            args.set("source", "black_magic");
        }
        let id = api.call(reg.conn, handler, args);
        self.calls.insert(id, CallWait { kind: PendingCall::Command, deadline: Instant::now() + Duration::from_secs(5) });
    }

    pub fn builtin(&mut self, wid: &str, name: &str) {
        match name {
            "new_tab" => {
                self.new_tab(wid, None, None, None);
            }
            "new_window" => {
                self.new_window(None, None, None);
            }
            "close_tab" => {
                if let Some(t) = self.win(wid).and_then(|w| w.tabs.get(w.active)).map(|t| t.id.clone()) {
                    self.close_tab(&t);
                }
            }
            "next_tab" | "prev_tab" => {
                if let Some(w) = self.win(wid) {
                    let n = w.tabs.len();
                    if n > 0 {
                        w.active = if name == "next_tab" { (w.active + 1) % n } else { (w.active + n - 1) % n };
                        w.dirty = true;
                    }
                }
            }
            "copy" | "cut" if self.win(wid).is_some_and(|w| w.focus == Focus::Panel) => {
                let pid = self.win(wid).and_then(|w| w.panel.clone());
                if let Some(p) = pid.and_then(|id| self.panels.get_mut(&id)) {
                    // Never copy a hidden (secret) input.
                    if let Some(t) = p.selected_text().filter(|_| !p.input_secret) {
                        platform::set_clipboard_text(&t);
                    }
                    if name == "cut" {
                        p.delete_selection();
                    }
                }
                if let Some(w) = self.win(wid) {
                    w.dirty = true;
                }
            }
            "copy" => self.copy(wid),
            "cut" => {}
            "paste" => self.paste(wid),
            "select_all" if self.win(wid).is_some_and(|w| w.focus == Focus::Panel) => {
                let pid = self.win(wid).and_then(|w| w.panel.clone());
                if let Some(p) = pid.and_then(|id| self.panels.get_mut(&id)) {
                    p.select_all();
                }
                if let Some(w) = self.win(wid) {
                    w.dirty = true;
                }
            }
            "select_all" => {
                if let Some(sid) = self.win(wid).and_then(|w| w.active_session()).map(String::from) {
                    if let Some(s) = self.sessions.get_mut(&sid) {
                        let t = s.s.term.lock().unwrap();
                        let start = Pos { line: t.dropped, col: 0 };
                        let end = Pos { line: t.abs_end() - 1, col: t.cols };
                        drop(t);
                        s.selection = Some(Selection { anchor: start, head: end, unit: 0 });
                    }
                    self.mark_session_dirty(&sid);
                }
            }
            "clear" => {
                if let Some(sid) = self.win(wid).and_then(|w| w.active_session()).map(String::from) {
                    self.with_term(&sid, |t| t.feed(b"\x1b[3J"));
                    self.send_user(&sid, &[0x0c]);
                }
            }
            "bigger" => {
                let s = self.renderer.font_size + 1.0;
                self.set_font_size(s)
            }
            "smaller" => {
                let s = self.renderer.font_size - 1.0;
                self.set_font_size(s)
            }
            "reset_font" => self.set_font_size(13.0),
            "info" | "version" | "zenteru" => self.show_splash(wid),
            n if n.starts_with("rain_") => {
                if let Some(style) = crate::gutter::RainStyle::parse(&n[5..]) {
                    self.set_rain_style(style);
                }
            }
            "toggle_gutter" => {
                self.gutter = !self.gutter;
                let ids: Vec<String> = self.windows.iter().map(|w| w.id.clone()).collect();
                for id in ids {
                    if let Some(w) = self.win(&id) {
                        w.layout.cols = 0;
                    }
                    self.relayout(&id);
                }
            }
            "palette" => {
                if let Some(w) = self.win(wid) {
                    w.palette = Some(Palette::default());
                    w.dirty = true;
                }
            }
            "focus_panel" => {
                if let Some(w) = self.win(wid) {
                    if w.panel.is_some() {
                        w.focus = if w.focus == Focus::Panel { Focus::Terminal } else { Focus::Panel };
                        w.dirty = true;
                    }
                }
            }
            "sudo_auth" => {
                if let Some(sid) = self.win(wid).and_then(|w| w.active_session()).map(String::from) {
                    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "zenteru".into());
                    let cmd = format!("{} sudo-auth", input::shell_quote(&exe));
                    // Typed, not run: the user reviews and presses Enter.
                    self.send_user(&sid, cmd.as_bytes());
                }
            }
            n if n.starts_with("tab_") => {
                if let Ok(i) = n[4..].parse::<usize>() {
                    if let Some(w) = self.win(wid) {
                        if i == 9 {
                            w.active = w.tabs.len().saturating_sub(1);
                        } else if i >= 1 && i <= w.tabs.len() {
                            w.active = i - 1;
                        }
                        w.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    // ---- clipboard / selection / drop ---------------------------------

    pub fn selection_text(&self, sid: &str) -> Option<String> {
        let s = self.sessions.get(sid)?;
        let sel = s.selection.as_ref()?;
        let (a, b) = sel.ordered();
        let t = s.s.term.lock().unwrap();
        let text = t.text_range((a.line, a.col), (b.line, b.col));
        Some(text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n"))
    }

    pub fn copy(&mut self, wid: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        if self.windows[wi].focus == Focus::Panel {
            return;
        }
        if let Some(sid) = self.windows[wi].active_session().map(String::from) {
            if let Some(t) = self.selection_text(&sid) {
                if !t.is_empty() {
                    platform::set_clipboard_text(&t);
                }
            }
        }
    }

    pub fn paste(&mut self, wid: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        if let Some(text) = platform::clipboard_text() {
            if self.windows[wi].focus == Focus::Panel {
                self.insert_text(wid, &text.replace("\r\n", " ").replace('\n', " "));
                return;
            }
            let Some(sid) = self.windows[wi].active_session().map(String::from) else { return };
            let modes = self.sessions[&sid].s.term.lock().unwrap().modes.clone();
            let bytes = input::encode_paste(&text, &modes);
            self.send_user(&sid, &bytes);
            return;
        }
        let items = platform::pasteboard_items(&platform::pasteboard());
        if !items.is_empty() {
            if let Some(sid) = self.windows[wi].active_session().map(String::from) {
                self.drop_items(&sid, Json::Arr(items), "paste");
            }
        }
    }

    /// Files / image data dropped or pasted: ask drop handlers in turn,
    /// then fall back to inserting quoted paths.
    pub fn drop_items(&mut self, sid: &str, items: Json, source: &str) {
        let handlers: Vec<String> = self.api.as_ref().map(|a| a.registrations("drop_handler").into_iter().map(|r| r.handler).collect()).unwrap_or_default();
        self.try_drop(sid, items, source, handlers);
    }

    fn try_drop(&mut self, sid: &str, items: Json, source: &str, mut rest: Vec<String>) {
        if rest.is_empty() {
            let paths: Vec<String> = items.as_arr().iter().filter_map(|i| i.get("path").as_str()).map(input::shell_quote).collect();
            if !paths.is_empty() {
                let text = paths.join(" ") + " ";
                self.send_user(sid, text.as_bytes());
            }
            return;
        }
        let handler = rest.remove(0);
        let Some(api) = &self.api else { return };
        let Some(reg) = api.registration(&handler) else {
            self.try_drop(sid, items, source, rest);
            return;
        };
        let args = Json::obj().with("session_id", sid).with("source", source).with("items", items.clone());
        let id = api.call(reg.conn, &handler, args);
        let timeout = reg.params.get("timeout_ms").as_i64().unwrap_or(5000).clamp(100, 15000) as u64;
        self.calls.insert(
            id,
            CallWait {
                kind: PendingCall::Drop { session: sid.to_string(), items, source: source.to_string(), rest },
                deadline: Instant::now() + Duration::from_millis(timeout),
            },
        );
    }

    fn call_done(&mut self, id: &str, result: Result<Json, Json>) {
        let Some(c) = self.calls.remove(id) else { return };
        let v = result.unwrap_or(Json::Null);
        match c.kind {
            PendingCall::Completion { session } => self.finish_completion(&session, v),
            PendingCall::Drop { session, items, source, rest } => match v.get("text").as_str() {
                Some(t) => {
                    let t = t.to_string();
                    self.send_user(&session, t.as_bytes());
                }
                None => self.try_drop(&session, items, &source, rest),
            },
            PendingCall::TitleProvider | PendingCall::Command => {}
            PendingCall::Relay { conn, id } => {
                if let Some(api) = &self.api {
                    api.reply(conn, &id, Ok(v));
                }
            }
        }
    }

    // ---- mouse --------------------------------------------------------

    /// Cell under a point (pixels), clamped to the grid.
    pub fn cell_at(&self, wi: usize, x: f32, y: f32) -> (usize, usize) {
        let l = &self.windows[wi].layout;
        let col = ((x - l.term_x) / l.cell_w).floor().clamp(0.0, (l.cols - 1) as f32) as usize;
        let row = ((y - l.term_y) / l.cell_h).floor().clamp(0.0, (l.rows - 1) as f32) as usize;
        (col, row)
    }

    pub fn terminal_mouse(&mut self, wid: &str, x: f32, y: f32, btn: MouseButton, action: MouseAction, clicks: isize, mods: Mods) {
        let Some(wi) = self.win_index(wid) else { return };
        let Some(sid) = self.windows[wi].active_session().map(String::from) else { return };
        let (col, row) = self.cell_at(wi, x, y);
        self.windows[wi].focus = Focus::Terminal;
        self.windows[wi].dirty = true;
        let Some(s) = self.sessions.get_mut(&sid) else { return };
        let (mouse_mode, modes) = {
            let t = s.s.term.lock().unwrap();
            (t.modes.mouse, t.modes.clone())
        };
        // Programs that track the mouse get events, unless ⇧ is held.
        if mouse_mode != MouseMode::Off && !mods.shift {
            if let Some(b) = input::encode_mouse(btn, action, col, row, mods, &modes) {
                s.s.write(&b);
            }
            return;
        }
        if btn != MouseButton::Left {
            return;
        }
        let t = s.s.term.lock().unwrap();
        let abs = t.view_abs(row);
        let range_at = |unit: u8, line: u64, col: usize| unit_range(&t, unit, line, col);
        match action {
            MouseAction::Press => {
                s.menu = None;
                let unit = match clicks {
                    2 => 1,
                    3 => 2,
                    _ => 0,
                };
                let origin = range_at(unit, abs, col);
                drop(t);
                s.sel_origin = Some((origin.0, origin.1, unit));
                // Double / triple click select right away; a single click
                // only selects once the pointer moves.
                s.selection = (unit > 0).then(|| Selection { anchor: origin.0, head: origin.1, unit });
            }
            MouseAction::Motion => {
                let Some((o0, o1, unit)) = s.sel_origin else { return };
                let cur = range_at(unit, abs, col);
                drop(t);
                s.selection = extend_selection((o0, o1), cur, unit).map(|(a, h)| Selection { anchor: a, head: h, unit });
            }
            MouseAction::Release => {
                drop(t);
                s.sel_origin = None;
            }
        }
        s.s.dirty.store(true, Ordering::Release);
    }

    pub fn scroll(&mut self, wid: &str, x: f32, y: f32, dy_px: f64, precise: bool) {
        let Some(wi) = self.win_index(wid) else { return };
        let l = self.windows[wi].layout.clone();
        if x >= l.panel_x && l.panel_w > 0.0 {
            if let Some(p) = self.windows[wi].panel.clone().and_then(|id| self.panels.get_mut(&id)) {
                let lines = (dy_px / l.cell_h as f64).round() as isize;
                p.scroll = (p.scroll as isize + lines).max(0) as usize;
            }
            self.windows[wi].dirty = true;
            return;
        }
        let Some(sid) = self.windows[wi].active_session().map(String::from) else { return };
        let (col, row) = self.cell_at(wi, x, y);
        let Some(s) = self.sessions.get_mut(&sid) else { return };
        s.scroll_accum += if precise { dy_px / l.cell_h as f64 } else { dy_px * 3.0 / l.cell_h as f64 * l.scale };
        let lines = s.scroll_accum.trunc() as isize;
        if lines == 0 {
            return;
        }
        s.scroll_accum -= lines as f64;
        let mut t = s.s.term.lock().unwrap();
        let modes = t.modes.clone();
        if modes.mouse != MouseMode::Off {
            let btn = if lines > 0 { MouseButton::WheelUp } else { MouseButton::WheelDown };
            let mut out = Vec::new();
            for _ in 0..lines.unsigned_abs().min(10) {
                if let Some(b) = input::encode_mouse(btn, MouseAction::Press, col, row, Mods::default(), &modes) {
                    out.extend(b);
                }
            }
            drop(t);
            s.s.write(&out);
        } else if t.alt_screen() && modes.alt_scroll {
            let k = if lines > 0 { Key::Up } else { Key::Down };
            let b = input::encode_key(&k, Mods::default(), &modes).repeat(lines.unsigned_abs());
            drop(t);
            s.s.write(&b);
        } else {
            t.scroll_view(lines);
            drop(t);
        }
        s.s.dirty.store(true, Ordering::Release);
    }

    // ---- panels -------------------------------------------------------

    fn panel_key(&mut self, wid: &str, key: &Key, mods: Mods, has_marked: bool) -> bool {
        let Some(wi) = self.win_index(wid) else { return true };
        let Some(pid) = self.windows[wi].panel.clone() else {
            self.windows[wi].focus = Focus::Terminal;
            return true;
        };
        if has_marked {
            return false;
        }
        let conn = self.panels.get(&pid).map(|p| p.conn);
        let Some(p) = self.panels.get_mut(&pid) else { return true };
        match key {
            Key::Escape => {
                if p.selection().is_some() {
                    p.anchor = None;
                } else {
                    self.windows[wi].focus = Focus::Terminal;
                }
            }
            Key::Enter => {
                let text = std::mem::take(&mut p.input);
                p.cursor = 0;
                p.anchor = None;
                p.input_secret = false;
                p.scroll = 0;
                if !text.trim().is_empty() {
                    if let (Some(api), Some(_)) = (&self.api, conn) {
                        api.emit("panel", None, Json::obj().with("panel_id", pid.as_str()).with("type", "input").with("text", text));
                    }
                }
            }
            Key::Backspace | Key::Delete => {
                if !p.delete_selection() {
                    let back = *key == Key::Backspace;
                    let n = p.len_chars();
                    let (from, to) = match (back, mods.cmd, mods.alt) {
                        (true, true, _) => (0, p.cursor),
                        (true, _, true) => (word_left_chars(&p.input, p.cursor), p.cursor),
                        (true, _, _) => (p.cursor.saturating_sub(1), p.cursor),
                        (false, true, _) => (p.cursor, n),
                        (false, _, true) => (p.cursor, word_right_chars(&p.input, p.cursor)),
                        (false, _, _) => (p.cursor, (p.cursor + 1).min(n)),
                    };
                    if from < to {
                        p.anchor = Some(from);
                        p.cursor = to;
                        p.delete_selection();
                    }
                }
            }
            Key::Left | Key::Right | Key::Home | Key::End => {
                let n = p.len_chars();
                let left = matches!(key, Key::Left | Key::Home);
                // Without ⇧, an arrow collapses a selection to its edge.
                if !mods.shift && matches!(key, Key::Left | Key::Right) && !mods.cmd && !mods.alt {
                    if let Some((a, b)) = p.selection() {
                        p.move_to(if left { a } else { b }, false);
                        self.windows[wi].dirty = true;
                        return true;
                    }
                }
                let to = match (key, mods.cmd, mods.alt) {
                    (Key::Home, ..) | (Key::Left, true, _) => 0,
                    (Key::End, ..) | (Key::Right, true, _) => n,
                    (Key::Left, _, true) => word_left_chars(&p.input, p.cursor),
                    (Key::Right, _, true) => word_right_chars(&p.input, p.cursor),
                    (Key::Left, ..) => p.cursor.saturating_sub(1),
                    _ => (p.cursor + 1).min(n),
                };
                p.move_to(to, mods.shift);
            }
            Key::Tab => {
                if !p.modes.is_empty() {
                    let i = p.modes.iter().position(|m| *m == p.mode).map_or(0, |i| (i + 1) % p.modes.len());
                    p.mode = p.modes[i].clone();
                    let mode = p.mode.clone();
                    if let Some(api) = &self.api {
                        api.emit("panel", None, Json::obj().with("panel_id", pid.as_str()).with("type", "mode").with("mode", mode));
                    }
                }
            }
            Key::Up => p.scroll += 1,
            Key::Down => p.scroll = p.scroll.saturating_sub(1),
            Key::Text(t) if mods.ctrl && t == "u" => {
                p.input.clear();
                p.cursor = 0;
                p.anchor = None;
            }
            Key::Text(t) if mods.ctrl && t == "a" => p.move_to(0, mods.shift),
            Key::Text(t) if mods.ctrl && t == "e" => {
                let n = p.len_chars();
                p.move_to(n, mods.shift);
            }
            Key::Text(_) if mods.ctrl => {}
            Key::Text(_) => return false,
            _ => {}
        }
        self.windows[wi].dirty = true;
        true
    }

    pub fn panel_action(&mut self, pid: &str, msg: u64, action: &str) {
        self.emit("panel", None, Json::obj().with("panel_id", pid).with("type", "action").with("message_id", msg).with("action_id", action));
    }

    pub fn panel_mode(&mut self, pid: &str, mode: &str) {
        if let Some(p) = self.panels.get_mut(pid) {
            p.mode = mode.to_string();
        }
        self.emit("panel", None, Json::obj().with("panel_id", pid).with("type", "mode").with("mode", mode));
    }

    pub fn close_panel_ui(&mut self, wid: &str) {
        if let Some(w) = self.win(wid) {
            if let Some(pid) = w.panel.clone() {
                w.focus = Focus::Terminal;
                if let Some(p) = self.panels.get_mut(&pid) {
                    p.visible = false;
                }
                self.relayout(wid);
            }
        }
    }

    // ---- plugins ------------------------------------------------------

    pub fn start_plugins(&mut self) {
        let Some(api) = &self.api else { return };
        let sock = api.socket_path.clone();
        let Some(python) = platform::find_python() else {
            eprintln!("zenteru: no Python 3.11+ found; plugins disabled (set ZENTERU_PYTHON)");
            return;
        };
        let mut dirs = vec![self.resources.join("plugins"), platform::support_dir().join("plugins")];
        dirs.dedup();
        let mut entries: Vec<(String, PathBuf, PathBuf)> = Vec::new();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let manifest = e.path().join("plugin.json");
                let Ok(txt) = std::fs::read_to_string(&manifest) else { continue };
                let Ok(m) = Json::parse(&txt) else { continue };
                if m.get("autostart").as_bool() == Some(false) {
                    continue;
                }
                let name = m.get("name").as_str().unwrap_or("plugin").to_string();
                if entries.iter().any(|(n, _, _)| *n == name) {
                    continue;
                }
                let entry = e.path().join(m.get("entry").as_str().unwrap_or("main.py"));
                entries.push((name, entry, e.path()));
            }
        }
        // iTerm2-style AutoLaunch scripts.
        if let Ok(rd) = std::fs::read_dir(platform::support_dir().join("Scripts/AutoLaunch")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "py") {
                    let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = p.parent().unwrap().to_path_buf();
                    entries.push((name, p, dir));
                }
            }
        }
        let logs = platform::log_dir();
        let _ = std::fs::create_dir_all(&logs);
        let pythonpath = self.resources.join("python");
        for (name, entry, dir) in entries {
            let log = std::fs::File::create(logs.join(format!("{name}.log")));
            let mut cmd = std::process::Command::new(&python);
            cmd.arg(&entry)
                .current_dir(&dir)
                .env("ZENTERU_API_SOCKET", &sock)
                .env("ZENTERU_COOKIE", &self.cookie)
                .env("ZENTERU_PLUGIN_DIR", &dir)
                .env("ZENTERU_TRANSIENT_DIR", &self.transient_dir)
                .env("PYTHONPATH", &pythonpath)
                .env("PYTHONUNBUFFERED", "1")
                // Never write bytecode into the (signed) app bundle.
                .env("PYTHONPYCACHEPREFIX", platform::cache_dir().join("pycache"))
                .stdin(std::process::Stdio::null());
            if let Ok(l) = log {
                if let Ok(l2) = l.try_clone() {
                    cmd.stdout(l).stderr(l2);
                }
            }
            match cmd.spawn() {
                Ok(c) => self.plugins.push((name, c)),
                Err(e) => eprintln!("zenteru: plugin {name}: {e}"),
            }
        }
    }

    pub fn shutdown(&mut self) {
        for (_, c) in &mut self.plugins {
            let _ = c.kill();
            let _ = c.wait();
        }
        self.sessions.clear();
        let _ = std::fs::remove_dir_all(&self.run_dir);
        if let Some(api) = &self.api {
            let _ = std::fs::remove_file(&api.socket_path);
        }
    }

    // ---- the API ------------------------------------------------------

    fn poll_api(&mut self) {
        let Some(api) = &self.api else { return };
        let incoming = api.poll();
        for inc in incoming {
            match inc {
                Incoming::Request { conn, id, method, params } => {
                    if method == "_registered" {
                        crate::view::refresh_plugin_menu();
                        continue;
                    }
                    let r = self.handle(conn, &id, &method, &params);
                    if let Some(r) = r {
                        if let Some(api) = &self.api {
                            api.reply(conn, &id, r);
                        }
                    }
                }
                Incoming::Response { id, result, .. } => self.call_done(&id, result),
                Incoming::Closed(conn) => {
                    let gone: Vec<String> = self.panels.iter().filter(|(_, p)| p.conn == conn).map(|(k, _)| k.clone()).collect();
                    for pid in gone {
                        self.destroy_panel(&pid);
                    }
                    crate::view::refresh_plugin_menu();
                }
            }
        }
    }

    fn destroy_panel(&mut self, pid: &str) {
        if let Some(p) = self.panels.remove(pid) {
            self.emit("panel", None, Json::obj().with("panel_id", pid).with("type", "closed"));
            let wid = p.window_id.clone();
            if let Some(w) = self.win(&wid) {
                if w.panel.as_deref() == Some(pid) {
                    w.panel = None;
                    w.focus = Focus::Terminal;
                }
            }
            self.relayout(&wid);
        }
    }

    fn session_json(&self, sid: &str) -> Option<Json> {
        let s = self.sessions.get(sid)?;
        let t = s.s.term.lock().unwrap();
        let title = if s.title.is_empty() { s.s.job_name().unwrap_or_default() } else { s.title.clone() };
        Some(
            Json::obj()
                .with("session_id", sid)
                .with("title", title)
                .with("cols", t.cols)
                .with("rows", t.rows)
                .with("pid", s.s.pid())
                .with("cwd", t.prompt.cwd.clone())
                .with("tty", s.s.tty())
                .with("alt_screen", t.alt_screen())
                .with("password_mode", s.pwd.active),
        )
    }

    fn window_json(&self, w: &Win) -> Json {
        let (x, y, ww, hh) = w.handle.frame();
        let tabs: Vec<Json> = w
            .tabs
            .iter()
            .map(|t| {
                let title = t.title.clone().unwrap_or_else(|| self.sessions.get(&t.session).map(|s| s.title.clone()).unwrap_or_default());
                Json::obj()
                    .with("tab_id", t.id.as_str())
                    .with("title", title)
                    .with("current_session_id", t.session.as_str())
                    .with("sessions", Json::Arr(self.session_json(&t.session).into_iter().collect()))
            })
            .collect();
        Json::obj()
            .with("window_id", w.id.as_str())
            .with("frame", Json::obj().with("x", x).with("y", y).with("w", ww).with("h", hh))
            .with("current_tab_id", w.tabs.get(w.active).map(|t| t.id.clone()))
            .with("tabs", Json::Arr(tabs))
    }

    fn handle(&mut self, conn: ConnId, id: &Json, method: &str, p: &Json) -> Option<Result<Json, (&'static str, String)>> {
        let sid = p.get("session_id").as_str().map(String::from);
        let need_session = |app: &App| -> Result<String, (&'static str, String)> {
            match &sid {
                Some(s) if app.sessions.contains_key(s) => Ok(s.clone()),
                Some(s) => Err(("not_found", format!("no session {s}"))),
                None => Err(("bad_request", "session_id required".into())),
            }
        };
        let ok = |v: Json| Some(Ok(v));
        let empty = || Some(Ok(Json::obj()));
        macro_rules! sess {
            () => {
                match need_session(self) {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                }
            };
        }
        match method {
            "app.get" => {
                let cur = self.key_window.clone().or_else(|| self.windows.first().map(|w| w.id.clone()));
                let wins: Vec<Json> = self.windows.iter().map(|w| self.window_json(w)).collect();
                ok(Json::obj().with("current_window_id", cur).with("windows", Json::Arr(wins)))
            }
            "app.transient_dir" => ok(Json::obj().with("path", self.transient_dir.to_string_lossy().into_owned())),
            "window.create" => {
                let (w, t, s) = self.new_window(p.get("command").as_str(), p.get("cwd").as_str(), None);
                ok(Json::obj().with("window_id", w).with("tab_id", t).with("session_id", s))
            }
            "window.activate" => {
                let wid = p.get("window_id").as_str().unwrap_or("");
                match self.windows.iter().find(|w| w.id == wid) {
                    Some(w) => {
                        w.handle.focus();
                        empty()
                    }
                    None => Some(Err(("not_found", format!("no window {wid}")))),
                }
            }
            "tab.create" => {
                let wid = p.get("window_id").as_str().map(String::from).or_else(|| self.key_window.clone()).or_else(|| self.windows.first().map(|w| w.id.clone()));
                match wid {
                    Some(w) if self.win_index(&w).is_some() => {
                        let idx = p.get("index").as_i64().map(|i| i.max(0) as usize);
                        match self.new_tab(&w, p.get("command").as_str(), p.get("cwd").as_str(), idx) {
                            Some((t, s)) => ok(Json::obj().with("tab_id", t).with("session_id", s)),
                            None => Some(Err(("internal", "could not spawn".into()))),
                        }
                    }
                    _ => {
                        let (_, t, s) = self.new_window(p.get("command").as_str(), p.get("cwd").as_str(), None);
                        ok(Json::obj().with("tab_id", t).with("session_id", s))
                    }
                }
            }
            "tab.activate" | "tab.close" | "tab.move" | "tab.set_title" => {
                let tid = p.get("tab_id").as_str().unwrap_or("").to_string();
                if self.find_tab(&tid).is_none() {
                    return Some(Err(("not_found", format!("no tab {tid}"))));
                }
                match method {
                    "tab.activate" => self.activate_tab(&tid),
                    "tab.close" => self.close_tab(&tid),
                    "tab.set_title" => {
                        let (wi, ti) = self.find_tab(&tid).unwrap();
                        self.windows[wi].tabs[ti].title = p.get("title").as_str().map(String::from);
                        self.windows[wi].dirty = true;
                    }
                    _ => {
                        let idx = p.get("index").as_i64().unwrap_or(0).max(0) as usize;
                        let to = p.get("window_id").as_str().map(String::from);
                        match to.as_deref() {
                            Some("new") => self.move_tab(&tid, None, 0, None),
                            Some(w) => self.move_tab(&tid, Some(w), idx, None),
                            None => {
                                let wid = self.windows[self.find_tab(&tid).unwrap().0].id.clone();
                                self.move_tab(&tid, Some(&wid), idx, None)
                            }
                        }
                    }
                }
                empty()
            }
            "session.get" => {
                let s = sess!();
                ok(self.session_json(&s).unwrap())
            }
            "session.send_text" => {
                let s = sess!();
                let text = p.get("text").as_str().unwrap_or("");
                let as_user = p.get("as_user").as_bool().unwrap_or(false);
                let sess = &self.sessions[&s];
                if sess.pwd.active && !as_user {
                    return Some(Err(("unsupported", "session is in password mode".into())));
                }
                if as_user {
                    sess.s.write(text.as_bytes());
                } else {
                    sess.s.write_as_plugin(text.as_bytes());
                }
                empty()
            }
            "session.inject" => {
                let s = sess!();
                let data = p.get("data").as_str().unwrap_or("").to_string();
                self.with_term(&s, |t| t.feed(data.as_bytes()));
                empty()
            }
            "session.get_screen" => {
                let s = sess!();
                let sess = &self.sessions[&s];
                let pw = sess.pwd.active;
                let t = sess.s.term.lock().unwrap();
                let base = t.screen_base();
                let first = p.get("first_line").as_i64().map(|v| (v.max(0) as u64).max(t.dropped)).unwrap_or(base);
                let count = p.get("count").as_i64().map(|v| v.clamp(0, 10_000) as u64).unwrap_or(t.rows as u64);
                let end = (first + count).min(t.abs_end());
                let cursor_abs = base + t.cursor.y as u64;
                let lines: Vec<Json> = (first..end)
                    .map(|l| {
                        let row = t.line(l);
                        line_json(&t, l)
                            .with("hard_eol", row.is_none_or(|r| !r.wrapped))
                            .with("password", pw && l == cursor_abs)
                    })
                    .collect();
                ok(Json::obj()
                    .with("cols", t.cols)
                    .with("rows", t.rows)
                    .with("cursor", Json::obj().with("x", t.cursor.x).with("y", t.cursor.y))
                    .with("first_line", first)
                    .with("alt_screen", t.alt_screen())
                    .with("lines", Json::Arr(lines)))
            }
            "session.resolve_line" => {
                let s = sess!();
                let Some(r) = p.get("ref").as_str().and_then(zenteru_term::lineref::LineRef::parse) else {
                    return Some(Err(("bad_request", "ref must be ゼロ/0, -N, a 3-character code or a line UUID".into())));
                };
                let t = self.sessions[&s].s.term.lock().unwrap();
                let hits = zenteru_term::lineref::resolve(&t, &r);
                let matches: Vec<Json> = hits.iter().map(|&l| line_json(&t, l)).collect();
                ok(Json::obj().with("ambiguous", matches.len() > 1).with("matches", Json::Arr(matches)))
            }
            "session.get_line_info" => {
                let s = sess!();
                let t = self.sessions[&s].s.term.lock().unwrap();
                ok(Json::obj()
                    .with("scrollback_lines", t.scrollback.len())
                    .with("rows", t.rows)
                    .with("cols", t.cols)
                    .with("first_visible_line", t.screen_base()))
            }
            "session.get_prompt" => {
                let s = sess!();
                let sess = &self.sessions[&s];
                let t = sess.s.term.lock().unwrap();
                let prompt_line = t.prompt.marks.last().and_then(|&m| t.line(m)).map(|r| r.text());
                ok(Json::obj()
                    .with("state", phase_name(t.prompt.phase))
                    .with("command", t.prompt.command.clone())
                    .with("cwd", t.prompt.cwd.clone())
                    .with("exit_status", t.prompt.exit_status.map(|v| v as i64))
                    .with("prompt_line", prompt_line))
            }
            "session.get_selection" => {
                let s = sess!();
                ok(Json::obj().with("text", self.selection_text(&s).unwrap_or_default()))
            }
            "session.activate" => {
                let s = sess!();
                let tid = self.sessions[&s].tab_id.clone();
                self.activate_tab(&tid);
                empty()
            }
            "session.close" => {
                let s = sess!();
                let tid = self.sessions[&s].tab_id.clone();
                self.close_tab(&tid);
                empty()
            }
            "session.set_variable" => {
                let s = sess!();
                let name = p.get("name").as_str().unwrap_or("");
                if !name.starts_with("user.") {
                    return Some(Err(("bad_request", "only user.* variables are writable".into())));
                }
                let v = match p.get("value") {
                    Json::Str(x) => x.clone(),
                    other => other.to_string(),
                };
                self.sessions.get_mut(&s).unwrap().vars.insert(name.to_string(), v.clone());
                self.emit("variable", Some(&s), Json::obj().with("session_id", s.as_str()).with("name", name).with("value", v));
                empty()
            }
            "session.get_variable" => {
                let s = sess!();
                let name = p.get("name").as_str().unwrap_or("");
                let sess = &self.sessions[&s];
                let t = sess.s.term.lock().unwrap();
                let v: Json = match name {
                    "session.id" => s.as_str().into(),
                    "session.name" => sess.title.as_str().into(),
                    "session.cols" => t.cols.into(),
                    "session.rows" => t.rows.into(),
                    "session.pid" => sess.s.pid().into(),
                    "session.tty" => sess.s.tty().into(),
                    "session.path" => {
                        drop(t);
                        return ok(Json::obj().with("value", sess.s.cwd()));
                    }
                    "session.hostname" => std::env::var("HOST").ok().or_else(hostname).into(),
                    "session.username" => std::env::var("USER").ok().into(),
                    "session.lastCommand" => sess.s.history.last().cloned().into(),
                    "session.jobName" => {
                        drop(t);
                        return ok(Json::obj().with("value", sess.s.job_name()));
                    }
                    n => sess.vars.get(n).cloned().into(),
                };
                ok(Json::obj().with("value", v))
            }
            "session.env.set" | "session.env.unset" => {
                let s = sess!();
                let name = p.get("name").as_str().unwrap_or("").to_string();
                let sess = self.sessions.get_mut(&s).unwrap();
                let r = if method == "session.env.set" {
                    sess.s.env_set(&name, p.get("value").as_str().unwrap_or(""))
                } else {
                    sess.s.env_unset(&name)
                };
                match r {
                    Ok(()) => empty(),
                    Err(e) => Some(Err(("bad_request", e.to_string()))),
                }
            }
            "session.env.list" => {
                let s = sess!();
                let names: Vec<Json> = self.sessions[&s].s.env_names().iter().map(|n| Json::from(n.as_str())).collect();
                ok(Json::obj().with("names", Json::Arr(names)))
            }
            "session.complete_shell" => {
                let s = sess!();
                let sess = &self.sessions[&s];
                // With an explicit line: ESC[9235~ b64(before);b64(after)\n
                // (IME_CONTRACT §11); else complete the shell's own buffer.
                match p.get("line").as_str() {
                    Some(line) => {
                        let cur = p.get("cursor").as_i64().unwrap_or(i64::MAX).max(0) as usize;
                        let gs = zenteru_text::seg::graphemes(line);
                        let cut = cur.min(gs.len());
                        let before: String = gs[..cut].concat();
                        let after: String = gs[cut..].concat();
                        let mut msg = b"\x1b[9235~".to_vec();
                        msg.extend(zenteru_term::base64::encode(before.as_bytes()).bytes());
                        msg.push(b';');
                        msg.extend(zenteru_term::base64::encode(after.as_bytes()).bytes());
                        msg.push(b'\n');
                        sess.s.write(&msg);
                    }
                    None => sess.s.write(b"\x1b[9234~"),
                }
                let ms = p.get("timeout_ms").as_i64().unwrap_or(800).clamp(50, 5000) as u64;
                self.shell_waiters.push(ShellWaiter { conn, req: id.clone(), session: s, deadline: Instant::now() + Duration::from_millis(ms) });
                None
            }
            "panel.create" => {
                let pid = self.id('p');
                let wid = p.get("window_id").as_str().map(String::from).or_else(|| self.key_window.clone()).or_else(|| self.windows.first().map(|w| w.id.clone()));
                let Some(wid) = wid else { return Some(Err(("not_found", "no window".into()))) };
                let modes: Vec<String> = p.get("modes").as_arr().iter().filter_map(|m| m.as_str().map(String::from)).collect();
                let mode = p.get("mode").as_str().map(String::from).or_else(|| modes.first().cloned()).unwrap_or_default();
                self.panels.insert(
                    pid.clone(),
                    Panel {
                        id: pid.clone(),
                        conn,
                        title: p.get("title").as_str().unwrap_or("panel").to_string(),
                        window_id: wid.clone(),
                        messages: Vec::new(),
                        status: String::new(),
                        modes,
                        mode,
                        input: String::new(),
                        cursor: 0,
                        preedit: String::new(),
                        scroll: 0,
                        visible: false,
                        input_secret: false,
                        anchor: None,
                        next_msg: 1,
                    },
                );
                ok(Json::obj().with("panel_id", pid))
            }
            m if m.starts_with("panel.") => {
                let pid = p.get("panel_id").as_str().unwrap_or("").to_string();
                if !self.panels.contains_key(&pid) {
                    return Some(Err(("not_found", format!("no panel {pid}"))));
                }
                let wid = self.panels[&pid].window_id.clone();
                // Panels follow the focused window when shown.
                let target_wid = self.key_window.clone().filter(|k| self.win_index(k).is_some()).unwrap_or(wid.clone());
                let result = {
                    let pan = self.panels.get_mut(&pid).unwrap();
                    match m {
                        "panel.show" | "panel.hide" | "panel.toggle" => {
                            let show = match m {
                                "panel.show" => true,
                                "panel.hide" => false,
                                _ => !(pan.visible && pan.window_id == target_wid),
                            };
                            pan.visible = show;
                            if show {
                                pan.window_id = target_wid.clone();
                            }
                            Json::obj()
                        }
                        "panel.append" => {
                            let mid = pan.next_msg;
                            pan.next_msg += 1;
                            pan.messages.push(PanelMsg {
                                id: mid,
                                role: p.get("role").as_str().unwrap_or("assistant").to_string(),
                                text: p.get("text").as_str().unwrap_or("").to_string(),
                                actions: parse_actions(p.get("actions")),
                            });
                            if pan.messages.len() > 2000 {
                                pan.messages.remove(0);
                            }
                            pan.scroll = 0;
                            Json::obj().with("message_id", mid)
                        }
                        "panel.update" => {
                            let mid = p.get("message_id").as_i64().unwrap_or(-1) as u64;
                            if let Some(msg) = pan.messages.iter_mut().find(|x| x.id == mid) {
                                if let Some(t) = p.get("text").as_str() {
                                    msg.text = t.to_string();
                                }
                                if !p.get("actions").is_null() {
                                    msg.actions = parse_actions(p.get("actions"));
                                }
                            }
                            Json::obj()
                        }
                        "panel.clear" => {
                            pan.messages.clear();
                            Json::obj()
                        }
                        "panel.set_status" => {
                            pan.status = p.get("text").as_str().unwrap_or("").to_string();
                            Json::obj()
                        }
                        "panel.set_mode" => {
                            pan.mode = p.get("mode").as_str().unwrap_or("").to_string();
                            Json::obj()
                        }
                        "panel.set_input" => {
                            pan.input = p.get("text").as_str().unwrap_or("").to_string();
                            pan.cursor = pan.input.chars().count();
                            pan.anchor = None;
                            pan.input_secret = p.get("secret").as_bool().unwrap_or(false);
                            Json::obj()
                        }
                        "panel.destroy" => Json::obj(),
                        _ => return Some(Err(("unsupported", format!("unknown method {m}")))),
                    }
                };
                if m == "panel.destroy" {
                    self.destroy_panel(&pid);
                    return ok(result);
                }
                if matches!(m, "panel.show" | "panel.hide" | "panel.toggle") {
                    let visible = self.panels[&pid].visible;
                    // Detach from any window currently showing it.
                    for w in &mut self.windows {
                        if w.panel.as_deref() == Some(pid.as_str()) && (w.id != target_wid || !visible) {
                            w.panel = None;
                            w.focus = Focus::Terminal;
                        }
                    }
                    if visible {
                        if let Some(w) = self.win(&target_wid) {
                            w.panel = Some(pid.clone());
                            w.focus = Focus::Panel;
                        }
                    }
                    let ids: Vec<String> = self.windows.iter().map(|w| w.id.clone()).collect();
                    for i in ids {
                        self.relayout(&i);
                    }
                    if !visible {
                        self.emit("panel", None, Json::obj().with("panel_id", pid.as_str()).with("type", "hidden"));
                    }
                }
                let wid_now = self.panels.get(&pid).map(|p| p.window_id.clone());
                if let Some(w) = wid_now.and_then(|w| self.win(&w)) {
                    w.dirty = true;
                }
                ok(result)
            }
            // Test hooks: drive zenteru's own keyboard path. Only with ZENTERU_DEBUG.
            "debug.type" | "debug.key" | "debug.state" | "debug.zoom" if std::env::var_os("ZENTERU_DEBUG").is_some() => {
                let wid = p.get("window_id").as_str().map(String::from).or_else(|| self.windows.first().map(|w| w.id.clone()));
                let Some(wid) = wid else { return Some(Err(("not_found", "no window".into()))) };
                match method {
                    "debug.type" => {
                        let t = p.get("text").as_str().unwrap_or("").to_string();
                        for g in zenteru_text::seg::graphemes(&t) {
                            self.insert_text(&wid, g);
                        }
                    }
                    "debug.zoom" => {
                        // Exactly what a title-bar double-click does: zoom
                        // while the app state is borrowed.
                        if let Some(w) = self.windows.iter().find(|w| w.id == wid) {
                            w.handle.window.zoom(None);
                        }
                    }
                    "debug.key" => {
                        let key = match p.get("key").as_str().unwrap_or("") {
                            "Enter" => Key::Enter,
                            "Escape" => Key::Escape,
                            "Backspace" => Key::Backspace,
                            "Tab" => Key::Tab,
                            "Up" => Key::Up,
                            "Down" => Key::Down,
                            other => Key::Text(other.to_string()),
                        };
                        let m = p.get("mods").as_str().unwrap_or("");
                        let mods = Mods { shift: m.contains("shift"), ctrl: m.contains("ctrl"), alt: m.contains("alt"), cmd: m.contains("cmd") };
                        let key = match key {
                            Key::Text(t) if t == "Left" => Key::Left,
                            Key::Text(t) if t == "Right" => Key::Right,
                            Key::Text(t) if t == "Delete" => Key::Delete,
                            other => other,
                        };
                        if !self.key_down(&wid, key.clone(), mods, false) {
                            if let Key::Text(t) = key {
                                self.insert_text(&wid, &t);
                            }
                        }
                    }
                    _ => {}
                }
                let w = self.windows.iter().find(|w| w.id == wid);
                let pal = w.and_then(|w| w.palette.as_ref());
                let panel = w.and_then(|w| w.panel.clone()).and_then(|id| self.panels.get(&id));
                ok(Json::obj()
                    .with("panel_input", panel.map(|p| p.input.clone()))
                    .with("panel_selection", panel.and_then(|p| p.selected_text()))
                    .with("magic_open", pal.is_some())
                    .with("magic_text", pal.map(|p| p.text.clone()))
                    .with("magic_message", pal.and_then(|p| p.message.clone())))
            }
            "gutter.get_size" => {
                let key = self.key_window.clone().and_then(|k| self.windows.iter().find(|w| w.id == k)).or(self.windows.first());
                let (w, h, active, block) = key.map(|w| (w.matrix.0, w.matrix.1, w.rain_shown, (w.layout.cell_w * 0.5).round())).unwrap_or((0, 0, false, 0.0));
                ok(Json::obj().with("width", w).with("height", h).with("active", active).with("block_px", block).with("style", self.rain_style.name()))
            }
            "gutter.set_style" => match p.get("style").as_str().and_then(crate::gutter::RainStyle::parse) {
                Some(style) => {
                    self.set_rain_style(style);
                    empty()
                }
                None => Some(Err(("bad_request", "style must be drops, blocks, custom or off".into()))),
            },
            "gutter.set_frame" => {
                let (w, h) = (p.get("width").as_i64().unwrap_or(0), p.get("height").as_i64().unwrap_or(0));
                if !(1..=512).contains(&w) || !(1..=4096).contains(&h) {
                    return Some(Err(("bad_request", "width 1..512 and height 1..4096 required".into())));
                }
                let (w, h) = (w as u32, h as u32);
                let n = (w * h * 4) as usize;
                let px = match (p.get("rgba").as_str(), p.get("cells").as_arr()) {
                    (Some(b64), _) => zenteru_term::base64::decode(b64).filter(|v| v.len() == n),
                    (None, rows) if !rows.is_empty() => {
                        let mut v = vec![0u8; n];
                        for (y, row) in rows.iter().take(h as usize).enumerate() {
                            for (x, cell) in row.as_arr().iter().take(w as usize).enumerate() {
                                let i = (y * w as usize + x) * 4;
                                v[i..i + 4].copy_from_slice(&parse_cell(cell));
                            }
                        }
                        Some(v)
                    }
                    _ => None,
                };
                let Some(px) = px else { return Some(Err(("bad_request", "rgba must be base64 of width*height*4 bytes, or cells a list of rows".into()))) };
                self.gutter_frame = Some((w, h, std::rc::Rc::new(px), Instant::now()));
                for win in &mut self.windows {
                    if win.rain_shown {
                        win.dirty = true;
                    }
                }
                empty()
            }
            "ui.notify" => {
                platform::notify(p.get("title").as_str().unwrap_or(""), p.get("body").as_str().unwrap_or(""));
                empty()
            }
            "ui.alert" => {
                let buttons: Vec<String> = p.get("buttons").as_arr().iter().filter_map(|b| b.as_str().map(String::from)).collect();
                self.pending_alerts.push((
                    conn,
                    id.clone(),
                    p.get("title").as_str().unwrap_or("").to_string(),
                    p.get("body").as_str().unwrap_or("").to_string(),
                    buttons,
                ));
                None
            }
            "clipboard.get" => ok(Json::obj().with("text", platform::clipboard_text())),
            "clipboard.set" => {
                platform::set_clipboard_text(p.get("text").as_str().unwrap_or(""));
                empty()
            }
            "rpc.call" => {
                let handler = p.get("handler").as_str().unwrap_or("");
                let Some(api) = &self.api else { return Some(Err(("internal", "no api".into()))) };
                if let Some(b) = handler.strip_prefix("zenteru.") {
                    let wid = p.get("args").get("window_id").as_str().map(String::from).or_else(|| self.key_window.clone()).or_else(|| self.windows.first().map(|w| w.id.clone()));
                    if let Some(w) = wid {
                        let b = b.to_string();
                        self.builtin(&w, &b);
                    }
                    return empty();
                }
                match api.registration(handler) {
                    Some(reg) => {
                        let mut args = p.get("args").clone();
                        if args.is_null() {
                            args = Json::obj();
                        }
                        // Commands get the usual window/tab/session context.
                        if reg.kind == "command" && args.get("window_id").is_null() {
                            let w = self.key_window.clone().and_then(|k| self.windows.iter().find(|w| w.id == k)).or(self.windows.first());
                            if let Some(w) = w {
                                args.set("window_id", w.id.as_str());
                                args.set("tab_id", w.tabs.get(w.active).map(|t| t.id.clone()));
                                args.set("session_id", w.active_session().map(String::from));
                            }
                        }
                        let fid = api.call(reg.conn, handler, args);
                        self.calls.insert(fid, CallWait { kind: PendingCall::Relay { conn, id: id.clone() }, deadline: Instant::now() + Duration::from_secs(30) });
                        None
                    }
                    None => Some(Err(("not_found", format!("no handler {handler}")))),
                }
            }
            _ => Some(Err(("unsupported", format!("unknown method {method}")))),
        }
    }
}

/// (handler, title, Black Magic name)
pub const BUILTIN_COMMANDS: &[(&str, &str, &str)] = &[
    ("zenteru.new_tab", "New Tab", "new-tab"),
    ("zenteru.new_window", "New Window", "new-window"),
    ("zenteru.close_tab", "Close Tab", "close-tab"),
    ("zenteru.next_tab", "Next Tab", "next-tab"),
    ("zenteru.prev_tab", "Previous Tab", "prev-tab"),
    ("zenteru.clear", "Clear Scrollback", "clear"),
    ("zenteru.bigger", "Bigger Text", "bigger"),
    ("zenteru.smaller", "Smaller Text", "smaller"),
    ("zenteru.reset_font", "Reset Text Size", "reset-font"),
    ("zenteru.focus_panel", "Focus Side Panel", "panel"),
    ("zenteru.toggle_gutter", "Toggle Line Numbers (ゼロ / codes)", "line-numbers"),
    ("zenteru.sudo_auth", "Set Up Touch ID / YubiKey for sudo…", "sudo-auth"),
    ("zenteru.info", "zenteru: info (ゼ splash)", "info"),
    ("zenteru.version", "zenteru: version", "version"),
    ("zenteru.zenteru", "zenteru", "zenteru"),
    ("zenteru.rain_drops", "Gutter rain: drops (default)", "rain-drops"),
    ("zenteru.rain_blocks", "Gutter rain: blocks (fixed matrix)", "rain-blocks"),
    ("zenteru.rain_custom", "Gutter rain: custom (plugin driver)", "rain-custom"),
    ("zenteru.rain_off", "Gutter rain: off", "rain-off"),
];

fn parse_actions(v: &Json) -> Vec<(String, String)> {
    v.as_arr()
        .iter()
        .filter_map(|a| Some((a.get("id").as_str()?.to_string(), a.get("label").as_str()?.to_string())))
        .collect()
}

fn phase_name(p: PromptPhase) -> &'static str {
    match p {
        PromptPhase::Unknown => "unknown",
        PromptPhase::Editing => "editing",
        PromptPhase::Running => "running",
        PromptPhase::Finished => "finished",
    }
}

fn hostname() -> Option<String> {
    std::process::Command::new("/bin/hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn set_private(p: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}

pub fn byte_at(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}

fn word_left_chars(s: &str, cursor: usize) -> usize {
    // Letterblocks work on graphemes; for the panel input chars are close enough.
    let g = zenteru_text::letterblock::left(s, cursor);
    g.min(cursor)
}

fn word_right_chars(s: &str, cursor: usize) -> usize {
    zenteru_text::letterblock::right(s, cursor)
}

/// Columns of the letterblock under `col` in `row` (for double click).
fn block_cols(row: &zenteru_term::cell::Row, col: usize) -> (usize, usize) {
    // Map cells → graphemes.
    let mut text = String::new();
    let mut starts: Vec<usize> = Vec::new();
    for (i, c) in row.cells.iter().enumerate() {
        if c.is_spacer() {
            continue;
        }
        starts.push(i);
        c.push_text(&mut text);
    }
    let gidx = starts.iter().rposition(|&s| s <= col).unwrap_or(0);
    let r = zenteru_text::letterblock::block_at(&text, gidx);
    let start = starts.get(r.start).copied().unwrap_or(col);
    let end = starts.get(r.end).copied().unwrap_or(row.cells.len());
    (start, end)
}

/// Remove per-launch dirs (session env stores, transient files) left by
/// zenteru processes that are gone, e.g. after a crash.
fn clean_stale_runs(rt: &std::path::Path) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let Ok(rd) = std::fs::read_dir(rt) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("run-").and_then(|p| p.parse::<i32>().ok()) else { continue };
        if pid != std::process::id() as i32 && unsafe { kill(pid, 0) } != 0 {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The selection unit at (line, col) as a [start, end) range: a cell (both
/// halves of a zenkaku character), a letterblock (unit 1) or a line (2).
pub fn unit_range(t: &zenteru_term::term::Terminal, unit: u8, line: u64, col: usize) -> (Pos, Pos) {
    let r = t.line(line);
    match unit {
        1 => {
            let (a, b) = r.map(|r| block_cols(r, col)).unwrap_or((col, col + 1));
            (Pos { line, col: a }, Pos { line, col: b })
        }
        2 => (Pos { line, col: 0 }, Pos { line, col: t.cols }),
        _ => {
            let mut c = col;
            if let Some(r) = r {
                if c > 0 && r.cells.get(c).is_some_and(|x| x.is_spacer()) && r.cells[c - 1].is_wide() {
                    c -= 1;
                }
            }
            let w = if r.and_then(|r| r.cells.get(c)).is_some_and(|x| x.is_wide()) { 2 } else { 1 };
            (Pos { line, col: c }, Pos { line, col: c + w })
        }
    }
}

/// Selection from the unit pressed on (`origin`) to the unit under the
/// pointer (`cur`), in either direction, including both. A plain click
/// that hasn't left its cell selects nothing.
pub fn extend_selection(origin: (Pos, Pos), cur: (Pos, Pos), unit: u8) -> Option<(Pos, Pos)> {
    let key = |p: Pos| (p.line, p.col);
    if unit == 0 && key(cur.0) == key(origin.0) {
        return None;
    }
    let start = if key(cur.0) < key(origin.0) { cur.0 } else { origin.0 };
    let end = if key(cur.1) > key(origin.1) { cur.1 } else { origin.1 };
    Some((start, end))
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use zenteru_term::term::Terminal;

    fn select(t: &Terminal, unit: u8, from: usize, to: usize) -> String {
        let o = unit_range(t, unit, 0, from);
        let c = unit_range(t, unit, 0, to);
        let (a, b) = extend_selection(o, c, unit).expect("selection");
        t.text_range((a.line, a.col), (b.line, b.col))
    }

    #[test]
    fn drag_includes_both_ends_in_either_direction() {
        let mut t = Terminal::new(20, 2);
        t.feed(b"hello world");
        assert_eq!(select(&t, 0, 0, 10), "hello world");
        assert_eq!(select(&t, 0, 10, 0), "hello world");
        assert_eq!(select(&t, 0, 6, 0), "hello w");
        // Past the end of the text, to the right margin.
        assert_eq!(select(&t, 0, 19, 0).trim_end(), "hello world");
    }

    #[test]
    fn zenkaku_cells_select_whole() {
        let mut t = Terminal::new(20, 2);
        t.feed("한국어를 배웁니다".as_bytes());
        // Pointer on the right half of 한 (col 1) still takes 한.
        assert_eq!(select(&t, 0, 7, 1), "한국어를");
        assert_eq!(select(&t, 0, 1, 7), "한국어를");
    }

    #[test]
    fn letterblock_drag_backwards() {
        let mut t = Terminal::new(30, 2);
        t.feed("echo 한국어를 배웁니다".as_bytes());
        // Double-click on 배 then drag back onto 한국어: whole blocks.
        assert_eq!(select(&t, 1, 14, 6), "한국어를 배웁니다");
    }

    #[test]
    fn click_without_moving_selects_nothing() {
        let mut t = Terminal::new(10, 1);
        t.feed(b"abc");
        let o = unit_range(&t, 0, 0, 1);
        assert!(extend_selection(o, o, 0).is_none());
    }
}

/// A line for the API: absolute index, relative index, code, UUIDv7 and text.
fn line_json(t: &zenteru_term::term::Terminal, l: u64) -> Json {
    use zenteru_term::lineref;
    let row = t.line(l);
    let id = row.map_or(0, |r| r.id);
    Json::obj()
        .with("line", l)
        .with("rel", lineref::rel_of(t, l))
        .with("rel_label", lineref::rel_label(lineref::rel_of(t, l)))
        .with("code", (id != 0).then(|| lineref::code(id)))
        .with("id", (id != 0).then(|| lineref::uuid_string(id)))
        .with("landed_ms", (id != 0).then(|| lineref::landed_ms(id)))
        .with("text", row.map(|r| r.text()).unwrap_or_default())
}

/// A matrix cell from JSON: "#rrggbb", "#rrggbbaa", [r, g, b] or [r, g, b, a]
/// (0–255), or null for transparent.
fn parse_cell(v: &Json) -> [u8; 4] {
    match v {
        Json::Str(s) => {
            let h = s.trim_start_matches('#');
            let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
            match h.len() {
                6 => [byte(0).unwrap_or(0), byte(2).unwrap_or(0), byte(4).unwrap_or(0), 255],
                8 => [byte(0).unwrap_or(0), byte(2).unwrap_or(0), byte(4).unwrap_or(0), byte(6).unwrap_or(0)],
                _ => [0; 4],
            }
        }
        Json::Arr(a) => {
            let c = |i: usize, d: f64| a.get(i).and_then(Json::as_f64).unwrap_or(d).clamp(0.0, 255.0) as u8;
            [c(0, 0.0), c(1, 0.0), c(2, 0.0), c(3, 255.0)]
        }
        _ => [0; 4],
    }
}

impl App {
    /// Mouse in the panel's input box: click places the caret, drag
    /// selects, double-click selects a letterblock, triple-click all.
    pub fn panel_input_mouse(&mut self, wid: &str, x: f32, y: f32, action: zenteru_term::input::MouseAction, clicks: isize, shift: bool) {
        use zenteru_term::input::MouseAction;
        let Some(wi) = self.win_index(wid) else { return };
        let w = &self.windows[wi];
        let Some(pid) = w.panel.clone() else { return };
        let (ox, oy) = w.hits.panel_text_origin;
        let (cw, ch) = (w.layout.cell_w, w.layout.cell_h);
        let lines = w.hits.panel_lines.clone();
        let first = w.hits.panel_first_line;
        if lines.is_empty() {
            return;
        }
        let line = (((y - oy) / ch).floor().max(0.0) as usize + first).min(lines.len() - 1);
        let at = crate::textfield::char_at(&lines, line, ((x - ox) / cw).max(0.0));
        let Some(p) = self.panels.get_mut(&pid) else { return };
        match action {
            MouseAction::Press => {
                match clicks {
                    2 => p.select_block(at),
                    n if n >= 3 => p.select_all(),
                    _ => p.move_to(at, shift),
                }
                self.windows[wi].panel_drag = clicks <= 1;
                self.windows[wi].focus = Focus::Panel;
            }
            MouseAction::Motion => {
                if self.windows[wi].panel_drag {
                    p.move_to(at, true);
                }
            }
            MouseAction::Release => self.windows[wi].panel_drag = false,
        }
        self.windows[wi].dirty = true;
    }
}
