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
    /// Where the IME candidate window should appear (pixels).
    pub ime: [f32; 4],
}

pub struct Palette {
    pub filter: String,
    pub selected: usize,
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
    /// The command line contains a remembered password.
    pub secret_in_line: bool,
    pub confirm_enter: bool,
    pub selection: Option<Selection>,
    pub preedit: String,
    pub menu: Option<CompletionMenu>,
    pub comp_pending: Option<(String, Instant)>,
    pub queued: Vec<Vec<u8>>,
    pub vars: HashMap<String, String>,
    pub scroll_accum: f64,
    pub bell: Option<Instant>,
    pub last_phase: PromptPhase,
    pub mouse_down_cell: Option<(usize, usize)>,
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
                secret_in_line: false,
                confirm_enter: false,
                selection: None,
                preedit: String::new(),
                menu: None,
                comp_pending: None,
                queued: Vec::new(),
                vars: HashMap::new(),
                scroll_accum: 0.0,
                bell: None,
                last_phase: PromptPhase::Unknown,
                mouse_down_cell: None,
            },
        );
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
        let cols = (((w - panel_w - 2.0 * pad) / cw).floor() as usize).max(10);
        let rows = (((h - tabbar_h - 2.0 * pad) / ch).floor() as usize).max(3);
        let layout = Layout {
            scale,
            w,
            h,
            tabbar_h,
            term_x: pad,
            term_y: tabbar_h + pad,
            cols,
            rows,
            cell_w: cw,
            cell_h: ch,
            panel_x: w - panel_w,
            panel_w,
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
            let pw = s.s.password_mode();
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
                s.echo_warning = !pw && prompt_asks_for_secret(&line_before_cursor) && s.s.echo_on();
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
            return crate::ui::palette_key(self, wid, &key, mods);
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

    /// Quit once the last window is gone (after at least one existed).
    pub fn quit_when_empty(&self) -> bool {
        self.next_id > 1
    }

    /// Text from the IME / keyboard (insertText:).
    pub fn insert_text(&mut self, wid: &str, text: &str) {
        let Some(wi) = self.win_index(wid) else { return };
        self.windows[wi].dirty = true;
        if self.windows[wi].palette.is_some() {
            if let Some(p) = &mut self.windows[wi].palette {
                p.filter.push_str(text);
                p.selected = 0;
            }
            return;
        }
        if self.windows[wi].focus == Focus::Panel {
            if let Some(p) = self.windows[wi].panel.clone().and_then(|id| self.panels.get_mut(&id)) {
                let bi = byte_at(&p.input, p.cursor);
                p.input.insert_str(bi, text);
                p.cursor += text.chars().count();
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

    pub fn commands(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = BUILTIN_COMMANDS.iter().map(|(h, t)| (h.to_string(), t.to_string())).collect();
        if let Some(api) = &self.api {
            for r in api.registrations("command") {
                let title = r.params.get("title").as_str().unwrap_or(&r.handler).to_string();
                out.push((r.handler.clone(), title));
            }
        }
        out
    }

    pub fn invoke_command(&mut self, wid: &str, handler: &str) {
        if let Some(b) = handler.strip_prefix("zenteru.") {
            self.builtin(wid, b);
            return;
        }
        let Some(api) = &self.api else { return };
        let Some(reg) = api.registration(handler) else { return };
        let w = self.windows.iter().find(|w| w.id == wid);
        let tid = w.and_then(|w| w.tabs.get(w.active)).map(|t| t.id.clone());
        let sid = w.and_then(|w| w.active_session()).map(String::from);
        let args = Json::obj().with("window_id", wid).with("tab_id", tid).with("session_id", sid);
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
            "copy" => self.copy(wid),
            "paste" => self.paste(wid),
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
            "palette" => {
                if let Some(w) = self.win(wid) {
                    w.palette = Some(Palette { filter: String::new(), selected: 0 });
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
                self.insert_text(wid, &text.replace('\n', " "));
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
        let snap_col = |c: usize| -> usize {
            // Never start inside the right half of a zenkaku character.
            if let Some(r) = t.line(abs) {
                if c > 0 && r.cells.get(c).is_some_and(|x| x.is_spacer()) && r.cells[c - 1].is_wide() {
                    return c - 1;
                }
            }
            c
        };
        match action {
            MouseAction::Press => {
                s.menu = None;
                let col = snap_col(col);
                match clicks {
                    2 => {
                        // Letterblock under the cursor (zencode rules).
                        let (start, end) = match t.line(abs) {
                            Some(r) => block_cols(r, col),
                            None => (col, col + 1),
                        };
                        drop(t);
                        s.selection = Some(Selection { anchor: Pos { line: abs, col: start }, head: Pos { line: abs, col: end }, unit: 1 });
                    }
                    3 => {
                        let cols = t.cols;
                        drop(t);
                        s.selection = Some(Selection { anchor: Pos { line: abs, col: 0 }, head: Pos { line: abs, col: cols }, unit: 2 });
                    }
                    _ => {
                        drop(t);
                        s.selection = None;
                        s.mouse_down_cell = Some((col, row));
                        let _ = abs;
                    }
                }
            }
            MouseAction::Motion => {
                let col_end = col + 1;
                let cols = t.cols;
                drop(t);
                if let Some(sel) = &mut s.selection {
                    sel.head = Pos { line: abs, col: if sel.unit == 2 { cols } else { col_end } };
                } else if let Some((c0, r0)) = s.mouse_down_cell {
                    if (c0, r0) != (col, row) {
                        let t = s.s.term.lock().unwrap();
                        let a0 = t.view_abs(r0);
                        drop(t);
                        s.selection = Some(Selection { anchor: Pos { line: a0, col: c0 }, head: Pos { line: abs, col: col_end }, unit: 0 });
                    }
                }
            }
            MouseAction::Release => {
                drop(t);
                s.mouse_down_cell = None;
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
            Key::Escape => self.windows[wi].focus = Focus::Terminal,
            Key::Enter => {
                let text = std::mem::take(&mut p.input);
                p.cursor = 0;
                p.input_secret = false;
                p.scroll = 0;
                if !text.trim().is_empty() {
                    if let (Some(api), Some(_)) = (&self.api, conn) {
                        api.emit("panel", None, Json::obj().with("panel_id", pid.as_str()).with("type", "input").with("text", text));
                    }
                }
            }
            Key::Backspace => {
                if mods.alt || mods.cmd {
                    let start = if mods.cmd { 0 } else { word_left_chars(&p.input, p.cursor) };
                    let (a, b) = (byte_at(&p.input, start), byte_at(&p.input, p.cursor));
                    p.input.replace_range(a..b, "");
                    p.cursor = start;
                } else if p.cursor > 0 {
                    let (a, b) = (byte_at(&p.input, p.cursor - 1), byte_at(&p.input, p.cursor));
                    p.input.replace_range(a..b, "");
                    p.cursor -= 1;
                }
            }
            Key::Delete => {
                let n = p.input.chars().count();
                if p.cursor < n {
                    let (a, b) = (byte_at(&p.input, p.cursor), byte_at(&p.input, p.cursor + 1));
                    p.input.replace_range(a..b, "");
                }
            }
            Key::Left => {
                p.cursor = if mods.cmd { 0 } else if mods.alt { word_left_chars(&p.input, p.cursor) } else { p.cursor.saturating_sub(1) }
            }
            Key::Right => {
                let n = p.input.chars().count();
                p.cursor = if mods.cmd { n } else if mods.alt { word_right_chars(&p.input, p.cursor) } else { (p.cursor + 1).min(n) }
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
                        Json::obj()
                            .with("text", row.map(|r| r.text()).unwrap_or_default())
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

const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("zenteru.new_tab", "New Tab"),
    ("zenteru.new_window", "New Window"),
    ("zenteru.close_tab", "Close Tab"),
    ("zenteru.next_tab", "Next Tab"),
    ("zenteru.prev_tab", "Previous Tab"),
    ("zenteru.clear", "Clear Scrollback"),
    ("zenteru.bigger", "Bigger Text"),
    ("zenteru.smaller", "Smaller Text"),
    ("zenteru.reset_font", "Reset Text Size"),
    ("zenteru.focus_panel", "Focus Side Panel"),
    ("zenteru.sudo_auth", "Set Up Touch ID / YubiKey for sudo…"),
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
