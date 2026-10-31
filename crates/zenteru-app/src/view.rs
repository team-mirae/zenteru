//! AppKit plumbing: the terminal view (keyboard with full IME support via
//! NSTextInputClient, mouse, Chrome-style tab dragging, drag & drop), the
//! window and application delegates, menus and the frame timer.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSColor, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSMenu, NSMenuItem,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF, NSResponder,
    NSTextInputClient, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTabbingMode, NSWindowButton, NSWindowCollectionBehavior, NSAutoresizingMaskOptions,
};
use objc2_core_foundation::CGSize;
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSNotification, NSPoint, NSRange, NSRect, NSSize, NSString,
    NSTimer, NSRunLoop, NSRunLoopCommonModes, NSUInteger, NSNotFound,
};
use objc2_quartz_core::CAMetalLayer;

use zenteru_term::input::{Key, Mods, MouseAction, MouseButton};
use zenteru_term::json::Json;

use crate::app::{App, Focus, TabDrag};
use crate::render::{Frame, Renderer};

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static PLUGIN_MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
    static DELEGATE: RefCell<Option<Retained<AppDelegate>>> = const { RefCell::new(None) };
}

/// Run `f` with the app, unless it is already borrowed (re-entrant AppKit
/// callbacks); then the event is dropped and the next tick catches up.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut b = a.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("main thread")
}

// ---------------------------------------------------------------------
// Window handle
// ---------------------------------------------------------------------

pub struct WinHandle {
    pub window: Retained<NSWindow>,
    pub view: Retained<TermView>,
    pub layer: Retained<CAMetalLayer>,
    _delegate: Retained<WinDelegate>,
}

impl WinHandle {
    /// (width pt, height pt, backing scale)
    pub fn size(&self) -> (f64, f64, f64) {
        let b = self.view.bounds();
        (b.size.width, b.size.height, self.window.backingScaleFactor())
    }

    pub fn set_drawable_size(&self, w: f32, h: f32, scale: f64) {
        self.layer.setContentsScale(scale);
        self.layer.setDrawableSize(CGSize { width: w as f64, height: h as f64 });
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn focus(&self) {
        self.window.makeKeyAndOrderFront(None);
        let app = NSApplication::sharedApplication(mtm());
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }

    pub fn close(&self) {
        self.window.setDelegate(None);
        self.window.orderOut(None);
        self.window.close();
    }

    pub fn is_key(&self) -> bool {
        self.window.isKeyWindow()
    }

    /// Screen frame in points (x, y from bottom-left, w, h).
    pub fn frame(&self) -> (f64, f64, f64, f64) {
        let f = self.window.frame();
        (f.origin.x, f.origin.y, f.size.width, f.size.height)
    }

    pub fn present(&self, r: &mut Renderer, f: &Frame, size: (f32, f32)) {
        r.render(&self.layer, f, size);
    }

    pub fn discard_marked_text(&self) {
        self.view.ivars().marked.borrow_mut().clear();
        if let Some(ctx) = self.view.inputContext() {
            ctx.discardMarkedText();
        }
    }

    /// Hand the last Tab key back to the IME (provider declined).
    pub fn redeliver_tab(&self) {
        let ev = self.view.ivars().last_event.borrow().clone();
        if let Some(ev) = ev {
            self.view.ivars().redelivering.set(true);
            let arr = NSArray::from_retained_slice(&[ev]);
            self.view.interpretKeyEvents(&arr);
            self.view.ivars().redelivering.set(false);
        }
    }
}

/// Height of the gradient top bar, in points (the traffic lights sit on it).
const BAR_PT: f64 = crate::app::TAB_BAR_PT;

pub fn create_window(id: &str, origin: Option<(f64, f64)>, device: &ProtocolObject<dyn objc2_metal::MTLDevice>, existing: usize) -> WinHandle {
    let mtm = mtm();
    // Borderless, so the corners are perfectly square; the standard traffic
    // light buttons are placed on our own gradient bar instead.
    let style = NSWindowStyleMask::Borderless
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(980.0, 620.0));
    let window: Retained<ZenteruWindow> = unsafe {
        msg_send![ZenteruWindow::alloc(mtm), initWithContentRect: rect, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
    };
    let window: Retained<NSWindow> = Retained::into_super(window);
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("zenteru"));
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window.setMinSize(NSSize::new(420.0, 240.0));
    window.setHasShadow(true);
    window.setMovable(true);
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary | NSWindowCollectionBehavior::Managed);
    let bg = zenteru_text::palette::BG;
    window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(bg.0 as f64 / 255.0, bg.1 as f64 / 255.0, bg.2 as f64 / 255.0, 1.0)));
    if let Some(a) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&a));
    }
    match origin {
        Some((x, y)) => {
            window.setFrameTopLeftPoint(NSPoint::new(x, y));
        }
        None => {
            window.center();
            if existing > 0 {
                let f = window.frame();
                let d = 26.0 * existing as f64;
                window.setFrameOrigin(NSPoint::new(f.origin.x + d, f.origin.y - d));
            }
        }
    }

    // Container: the Metal view fills it; traffic lights float above.
    let container = NSView::initWithFrame(NSView::alloc(mtm), rect);
    container.setWantsLayer(true);
    let view = TermView::new(mtm, id);
    view.setFrame(container.bounds());
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    let layer = CAMetalLayer::new();
    layer.setDevice(Some(device));
    layer.setPixelFormat(objc2_metal::MTLPixelFormat::BGRA8Unorm);
    layer.setFramebufferOnly(true);
    layer.setPresentsWithTransaction(false);
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    container.addSubview(&view);
    let titled = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable;
    let h = container.bounds().size.height;
    for (i, kind) in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton].into_iter().enumerate() {
        if let Some(b) = NSWindow::standardWindowButton_forStyleMask(kind, titled, mtm) {
            let bf = b.frame();
            let x = 13.0 + i as f64 * 20.0;
            let y = h - (BAR_PT + bf.size.height) / 2.0 - bf.size.height;
            b.setFrameOrigin(NSPoint::new(x, y + bf.size.height));
            b.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
            container.addSubview(&b);
            // The buttons act on their window.
            unsafe { b.setTarget(Some(&window)) };
        }
    }
    window.setContentView(Some(&container));
    let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }, unsafe { NSPasteboardTypePNG }, unsafe { NSPasteboardTypeTIFF }, unsafe { NSPasteboardTypeString }]);
    view.registerForDraggedTypes(&types);
    let opts = NSTrackingAreaOptions::MouseMoved | NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveInKeyWindow | NSTrackingAreaOptions::InVisibleRect;
    let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), NSRect::ZERO, opts, Some(AsRef::<AnyObject>::as_ref(&*view)), None) };
    view.addTrackingArea(&area);
    let delegate = WinDelegate::new(mtm, id);
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.makeFirstResponder(Some(&view));
    if std::env::var_os("ZENTERU_DEBUG").is_some() {
        eprintln!("zenteru: window {id} number {}", window.windowNumber());
    }
    WinHandle { window, view, layer, _delegate: delegate }
}

define_class!(
    /// Borderless windows can't become key by default; this one can.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZenteruWindow"]
    pub struct ZenteruWindow;

    impl ZenteruWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            true
        }
    }
);

pub fn run_alert(title: &str, body: &str, buttons: &[String]) -> i64 {
    let alert = NSAlert::new(mtm());
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    for b in buttons {
        alert.addButtonWithTitle(&NSString::from_str(b));
    }
    let r = alert.runModal();
    (r - 1000) as i64
}

// ---------------------------------------------------------------------
// Terminal view
// ---------------------------------------------------------------------

pub struct ViewIvars {
    window_id: RefCell<String>,
    marked: RefCell<String>,
    last_event: RefCell<Option<Retained<NSEvent>>>,
    /// insertText / setMarkedText happened during the current keyDown.
    ime_acted: Cell<bool>,
    redelivering: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZenteruTermView"]
    #[ivars = ViewIvars]
    pub struct TermView;

    impl TermView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.on_key_down(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Left, MouseAction::Press);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Left, MouseAction::Motion);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Left, MouseAction::Release);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_down(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Right, MouseAction::Press);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_up(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Right, MouseAction::Release);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_down(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Middle, MouseAction::Press);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_up(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::Middle, MouseAction::Release);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.on_mouse(event, MouseButton::None, MouseAction::Motion);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            let wid = self.wid();
            with_app(|a| {
                if let Some(w) = a.win(&wid) {
                    if w.hover_tab.take().is_some() {
                        w.dirty = true;
                    }
                }
            });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (x, y) = self.point_px(event);
            let precise = event.hasPreciseScrollingDeltas();
            let dy = event.scrollingDeltaY() * if precise { self.scale() } else { 1.0 };
            let wid = self.wid();
            with_app(|a| a.scroll(&wid, x, y, dy, precise));
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn backing_changed(&self) {
            let wid = self.wid();
            with_app(|a| a.relayout(&wid));
        }
    }

    unsafe impl NSObjectProtocol for TermView {}

    unsafe impl NSTextInputClient for TermView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, _range: NSRange) {
            let text = any_to_string(string);
            self.ivars().marked.borrow_mut().clear();
            self.ivars().ime_acted.set(true);
            let wid = self.wid();
            with_app(|a| {
                a.set_preedit(&wid, "");
                a.insert_text(&wid, &text);
            });
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, _sel: Sel) {
            // The IME didn't want the key: encode it ourselves.
            let ev = self.ivars().last_event.borrow().clone();
            if let Some(ev) = ev {
                let (key, mods) = translate(&ev);
                let wid = self.wid();
                with_app(|a| a.encode_and_send(&wid, &key, mods));
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, string: &AnyObject, _sel: NSRange, _repl: NSRange) {
            let text = any_to_string(string);
            *self.ivars().marked.borrow_mut() = text.clone();
            self.ivars().ime_acted.set(true);
            let wid = self.wid();
            with_app(|a| a.set_preedit(&wid, &text));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.ivars().marked.borrow_mut().clear();
            let wid = self.wid();
            with_app(|a| a.set_preedit(&wid, ""));
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let n = self.ivars().marked.borrow().encode_utf16().count();
            if n == 0 { NSRange::new(NSNotFound as NSUInteger, 0) } else { NSRange::new(0, n) }
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, _range: NSRange, _actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            // The cursor cell, in screen coordinates, so candidate windows
            // (한자, 変換) open right there.
            let wid = self.wid();
            let r = with_app(|a| a.win(&wid).map(|w| w.hits.ime)).flatten().unwrap_or([0.0; 4]);
            let s = self.scale();
            let local = NSRect::new(NSPoint::new(r[0] as f64 / s, (r[1] + r[3]) as f64 / s), NSSize::new(r[2] as f64 / s, r[3] as f64 / s));
            let in_window = self.convertRect_toView(local, None);
            match self.window() {
                Some(w) => w.convertRectToScreen(in_window),
                None => in_window,
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn char_index(&self, _p: NSPoint) -> NSUInteger {
            0
        }
    }

    unsafe impl NSDraggingDestination for TermView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let pb = unsafe { info.draggingPasteboard() };
            let items = crate::platform::pasteboard_items(&pb);
            let wid = self.wid();
            let text = unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|t| t.to_string());
            with_app(|a| {
                if items.is_empty() {
                    if let Some(t) = &text {
                        a.insert_text(&wid, t);
                    }
                } else if let Some(sid) = a.win(&wid).and_then(|w| w.active_session()).map(String::from) {
                    a.drop_items(&sid, Json::Arr(items.clone()), "drop");
                }
            });
            !items.is_empty() || text.is_some()
        }
    }
);

impl TermView {
    fn new(mtm: MainThreadMarker, id: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            window_id: RefCell::new(id.to_string()),
            marked: RefCell::new(String::new()),
            last_event: RefCell::new(None),
            ime_acted: Cell::new(false),
            redelivering: Cell::new(false),
        });
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    fn wid(&self) -> String {
        self.ivars().window_id.borrow().clone()
    }

    fn scale(&self) -> f64 {
        self.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0)
    }

    fn point_px(&self, e: &NSEvent) -> (f32, f32) {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let s = self.scale();
        ((p.x * s) as f32, (p.y * s) as f32)
    }

    fn on_key_down(&self, event: &NSEvent) {
        *self.ivars().last_event.borrow_mut() = Some(event.retain());
        let (key, mods) = translate(event);
        let has_marked = !self.ivars().marked.borrow().is_empty();
        let wid = self.wid();
        let handled = with_app(|a| a.key_down(&wid, key, mods, has_marked)).unwrap_or(true);
        if handled {
            return;
        }
        // Let the input method see it (한글 composition, かな漢字変換, JIS Korean…).
        self.ivars().ime_acted.set(false);
        let arr = NSArray::from_retained_slice(&[event.retain()]);
        self.interpretKeyEvents(&arr);
    }

    fn on_mouse(&self, event: &NSEvent, btn: MouseButton, action: MouseAction) {
        let (x, y) = self.point_px(event);
        let wid = self.wid();
        let mods = mods_of(event.modifierFlags());
        let clicks = if action == MouseAction::Press { event.clickCount() } else { 0 };
        let screen = NSEvent::mouseLocation();
        let view = self.retain();
        with_app(|a| mouse(a, &wid, x, y, btn, action, clicks, mods, (screen.x, screen.y), event, &view));
    }
}

fn any_to_string(obj: &AnyObject) -> String {
    if let Some(s) = obj.downcast_ref::<NSString>() {
        return s.to_string();
    }
    if let Some(a) = obj.downcast_ref::<NSAttributedString>() {
        return a.string().to_string();
    }
    String::new()
}

fn mods_of(f: NSEventModifierFlags) -> Mods {
    Mods {
        shift: f.contains(NSEventModifierFlags::Shift),
        ctrl: f.contains(NSEventModifierFlags::Control),
        alt: f.contains(NSEventModifierFlags::Option),
        cmd: f.contains(NSEventModifierFlags::Command),
    }
}

/// NSEvent → (Key, Mods).
pub fn translate(e: &NSEvent) -> (Key, Mods) {
    let mods = mods_of(e.modifierFlags());
    let key = match e.keyCode() {
        36 | 76 => Key::Enter,
        48 => Key::Tab,
        51 => Key::Backspace,
        53 => Key::Escape,
        117 => Key::Delete,
        123 => Key::Left,
        124 => Key::Right,
        125 => Key::Down,
        126 => Key::Up,
        115 => Key::Home,
        119 => Key::End,
        116 => Key::PageUp,
        121 => Key::PageDown,
        114 => Key::Insert,
        122 => Key::F(1),
        120 => Key::F(2),
        99 => Key::F(3),
        118 => Key::F(4),
        96 => Key::F(5),
        97 => Key::F(6),
        98 => Key::F(7),
        100 => Key::F(8),
        101 => Key::F(9),
        109 => Key::F(10),
        103 => Key::F(11),
        111 => Key::F(12),
        105 => Key::F(13),
        107 => Key::F(14),
        113 => Key::F(15),
        106 => Key::F(16),
        64 => Key::F(17),
        79 => Key::F(18),
        80 => Key::F(19),
        90 => Key::F(20),
        _ => {
            let s = if mods.ctrl || mods.cmd { e.charactersIgnoringModifiers() } else { e.characters() };
            Key::Text(s.map(|s| s.to_string()).unwrap_or_default())
        }
    };
    (key, mods)
}

// ---------------------------------------------------------------------
// Mouse: tab bar, panel and terminal
// ---------------------------------------------------------------------

fn inside(r: [f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]
}

#[allow(clippy::too_many_arguments)]
fn mouse(a: &mut App, wid: &str, x: f32, y: f32, btn: MouseButton, action: MouseAction, clicks: isize, mods: Mods, screen: (f64, f64), event: &NSEvent, view: &TermView) {
    let Some(wi) = a.win_index(wid) else { return };
    let l = a.windows[wi].layout.clone();
    let s = l.scale as f32;

    // An in-progress tab drag owns the mouse.
    if a.windows[wi].drag.is_some() && btn != MouseButton::None {
        tab_drag(a, wid, x, y, action, screen);
        return;
    }

    if action == MouseAction::Motion && btn == MouseButton::None {
        // Hover state for the tab bar.
        let hover = if y < l.tabbar_h { a.windows[wi].hits.tabs.iter().find(|(_, tx, tw)| x >= *tx && x < tx + tw).map(|t| t.0.clone()) } else { None };
        if hover != a.windows[wi].hover_tab {
            a.windows[wi].hover_tab = hover;
            a.windows[wi].dirty = true;
        }
        if y >= l.tabbar_h && x < l.panel_x {
            a.terminal_mouse(wid, x, y, btn, action, clicks, mods);
        }
        return;
    }

    if y < l.tabbar_h && action == MouseAction::Press {
        let hits = a.windows[wi].hits.clone();
        if let Some((tid, ..)) = hits.close.iter().find(|(_, cx, cy, cw, chh)| inside([*cx, *cy, *cw, *chh], x, y)) {
            if btn == MouseButton::Left {
                a.close_tab(tid);
            }
            return;
        }
        if let Some((tid, tx, _)) = hits.tabs.iter().find(|(_, tx, tw)| x >= *tx && x < tx + tw) {
            if btn == MouseButton::Middle {
                a.close_tab(tid);
                return;
            }
            a.activate_tab(tid);
            a.windows[wi].drag = Some(TabDrag {
                tab_id: tid.clone(),
                grab_dx: x - tx,
                start_x: x,
                mouse_x: x,
                moved: false,
                detached: false,
                last_screen: screen,
            });
            return;
        }
        let (nx, ny, nw, nh) = hits.new_tab;
        if inside([nx, ny, nw, nh], x, y) {
            a.new_tab(wid, None, None, None);
            return;
        }
        // Empty bar: move the window; double click zooms.
        if clicks == 2 {
            a.windows[wi].handle.window.zoom(None);
        } else {
            a.windows[wi].handle.window.performWindowDragWithEvent(event);
        }
        return;
    }

    // Side panel.
    if l.panel_w > 0.0 && x >= l.panel_x {
        if action != MouseAction::Press {
            return;
        }
        let hits = a.windows[wi].hits.clone();
        if inside(hits.panel_close, x, y) {
            a.close_panel_ui(wid);
            return;
        }
        if let Some((pid, mid, aid, _)) = hits.actions.iter().find(|(_, _, _, r)| inside(*r, x, y)) {
            let (pid, mid, aid) = (pid.clone(), *mid, aid.clone());
            a.panel_action(&pid, mid, &aid);
            return;
        }
        if let Some((pid, mode, _)) = hits.modes.iter().find(|(_, _, r)| inside(*r, x, y)) {
            let (pid, mode) = (pid.clone(), mode.clone());
            a.panel_mode(&pid, &mode);
        }
        a.windows[wi].focus = Focus::Panel;
        a.windows[wi].dirty = true;
        let _ = (s, view);
        return;
    }

    // Completion menu clicks are handled as accept on the selected item.
    a.terminal_mouse(wid, x, y, btn, action, clicks, mods);
}

fn tab_drag(a: &mut App, wid: &str, x: f32, y: f32, action: MouseAction, screen: (f64, f64)) {
    let Some(wi) = a.win_index(wid) else { return };
    let l = a.windows[wi].layout.clone();
    let s = l.scale as f32;
    let n_tabs = a.windows[wi].tabs.len();
    let Some(mut d) = a.windows[wi].drag.clone() else { return };
    match action {
        MouseAction::Motion => {
            if (x - d.start_x).abs() > 4.0 * s || (y - l.tabbar_h / 2.0).abs() > l.tabbar_h {
                d.moved = true;
            }
            d.mouse_x = x;
            let out = y < -24.0 * s || y > l.tabbar_h + 36.0 * s;
            if n_tabs == 1 && d.moved {
                // The only tab: drag the whole window, like Chrome.
                let f = a.windows[wi].handle.window.frame();
                let (dx, dy) = (screen.0 - d.last_screen.0, screen.1 - d.last_screen.1);
                a.windows[wi].handle.window.setFrameOrigin(NSPoint::new(f.origin.x + dx, f.origin.y + dy));
                d.detached = true;
            } else {
                d.detached = out;
                if !out && d.moved {
                    // Live reorder.
                    let tab_w = a.windows[wi].hits.tabs.first().map(|t| t.2).unwrap_or(100.0);
                    let start = a.windows[wi].hits.tabs.first().map(|t| t.1).unwrap_or(0.0);
                    let idx = (((x - d.grab_dx + tab_w / 2.0) - start) / tab_w).floor().clamp(0.0, (n_tabs - 1) as f32) as usize;
                    let cur = a.windows[wi].tabs.iter().position(|t| t.id == d.tab_id);
                    if cur.is_some_and(|c| c != idx) {
                        a.move_tab(&d.tab_id, Some(wid), idx, None);
                    }
                }
            }
            d.last_screen = screen;
            if let Some(wi) = a.win_index(wid) {
                a.windows[wi].drag = Some(d);
                a.windows[wi].dirty = true;
            }
        }
        MouseAction::Release => {
            a.windows[wi].drag = None;
            a.windows[wi].dirty = true;
            if !d.detached {
                return;
            }
            // Over another window's tab bar? Merge into it.
            let target = a.windows.iter().filter(|w| w.id != wid).find_map(|w| {
                let (fx, fy, fw, fh) = w.handle.frame();
                let bar = w.layout.tabbar_h as f64 / w.layout.scale.max(1.0);
                let in_bar = screen.0 >= fx && screen.0 <= fx + fw && screen.1 <= fy + fh && screen.1 >= fy + fh - bar - 12.0;
                in_bar.then(|| {
                    let local_x = ((screen.0 - fx) * w.layout.scale) as f32;
                    let idx = w.hits.tabs.iter().position(|(_, tx, tw)| local_x < tx + tw / 2.0).unwrap_or(w.tabs.len());
                    (w.id.clone(), idx)
                })
            });
            match target {
                Some((tw, idx)) => a.move_tab(&d.tab_id, Some(&tw), idx, None),
                None if n_tabs > 1 => {
                    let gx = d.grab_dx as f64 / l.scale;
                    a.move_tab(&d.tab_id, None, 0, Some((screen.0 - gx - crate::app::TRAFFIC_LIGHTS_PT, screen.1 + 16.0)))
                }
                None => {}
            }
        }
        MouseAction::Press => {}
    }
}

// ---------------------------------------------------------------------
// Window delegate
// ---------------------------------------------------------------------

pub struct WinIvars {
    window_id: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZenteruWinDelegate"]
    #[ivars = WinIvars]
    pub struct WinDelegate;

    unsafe impl NSObjectProtocol for WinDelegate {}

    unsafe impl NSWindowDelegate for WinDelegate {
        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _n: &NSNotification) {
            let id = self.ivars().window_id.clone();
            with_app(|a| {
                a.relayout(&id);
                crate::ui::draw(a, &id);
            });
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn did_become_key(&self, _n: &NSNotification) {
            let id = self.ivars().window_id.clone();
            with_app(|a| {
                a.key_window = Some(id.clone());
                if let Some(w) = a.win(&id) {
                    w.dirty = true;
                }
                a.focus_event(&id, true);
            });
        }

        #[unsafe(method(windowDidResignKey:))]
        fn did_resign_key(&self, _n: &NSNotification) {
            let id = self.ivars().window_id.clone();
            with_app(|a| {
                if let Some(w) = a.win(&id) {
                    w.dirty = true;
                }
                a.focus_event(&id, false);
            });
        }

        #[unsafe(method(windowDidChangeBackingProperties:))]
        fn did_change_backing(&self, _n: &NSNotification) {
            let id = self.ivars().window_id.clone();
            with_app(|a| a.relayout(&id));
        }

        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _sender: &NSWindow) -> bool {
            let id = self.ivars().window_id.clone();
            with_app(|a| a.close_window(&id));
            false
        }
    }
);

impl WinDelegate {
    fn new(mtm: MainThreadMarker, id: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WinIvars { window_id: id.to_string() });
        unsafe { msg_send![super(this), init] }
    }
}

// ---------------------------------------------------------------------
// Application delegate, menus, timer
// ---------------------------------------------------------------------

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZenteruAppDelegate"]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish(&self, _n: &NSNotification) {
            build_menu(self);
            with_app(|a| {
                a.start_plugins();
                a.new_window(None, None, None);
            });
            let timer = unsafe {
                NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(1.0 / 120.0, self, sel!(tick:), None, true)
            };
            unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
            let app = NSApplication::sharedApplication(mtm());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn last_window(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            with_app(|a| a.shutdown());
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _app: &NSApplication, visible: bool) -> bool {
            if !visible {
                with_app(|a| {
                    a.new_window(None, None, None);
                });
            }
            true
        }
    }

    impl AppDelegate {
        #[unsafe(method(tick:))]
        fn tick(&self, _t: &NSTimer) {
            with_app(|a| {
                a.tick();
                if a.windows.is_empty() && a.quit_when_empty() {
                    NSApplication::sharedApplication(mtm()).terminate(None);
                }
            });
        }

        #[unsafe(method(builtin:))]
        fn builtin(&self, sender: &NSMenuItem) {
            let name = sender.representedObject().and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
            let wid = key_window_id();
            with_app(|a| match wid {
                Some(w) => a.builtin(&w, &name),
                None if name == "new_window" || name == "new_tab" => {
                    a.new_window(None, None, None);
                }
                None => {}
            });
        }

        #[unsafe(method(pluginCommand:))]
        fn plugin_command(&self, sender: &NSMenuItem) {
            let handler = sender.representedObject().and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
            if let Some(w) = key_window_id() {
                with_app(|a| a.invoke_command(&w, &handler));
            }
        }
    }
);

fn key_window_id() -> Option<String> {
    let app = NSApplication::sharedApplication(mtm());
    let key = app.keyWindow()?;
    with_app(|a| a.windows.iter().find(|w| Retained::as_ptr(&w.handle.window) == Retained::as_ptr(&key)).map(|w| w.id.clone())).flatten()
}

fn item(title: &str, action: Option<Sel>, key: &str, mods: NSEventModifierFlags, target: &AppDelegate, repr: Option<&str>) -> Retained<NSMenuItem> {
    let mtm = mtm();
    let it = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, &NSString::from_str(key)) };
    it.setKeyEquivalentModifierMask(mods);
    if action.is_some_and(|a| a == sel!(builtin:) || a == sel!(pluginCommand:)) {
        unsafe { it.setTarget(Some(target)) };
    }
    if let Some(r) = repr {
        unsafe { it.setRepresentedObject(Some(&NSString::from_str(r))) };
    }
    it
}

fn submenu(title: &str, items: Vec<Retained<NSMenuItem>>) -> (Retained<NSMenuItem>, Retained<NSMenu>) {
    let mtm = mtm();
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in items {
        menu.addItem(&i);
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&menu));
    (holder, menu)
}

fn build_menu(d: &AppDelegate) {
    let mtm = mtm();
    let cmd = NSEventModifierFlags::Command;
    let cs = NSEventModifierFlags::Command | NSEventModifierFlags::Shift;
    let b = |t: &str, k: &str, m: NSEventModifierFlags, name: &str| item(t, Some(sel!(builtin:)), k, m, d, Some(name));
    let sep = || NSMenuItem::separatorItem(mtm);
    let main = NSMenu::new(mtm);
    let names = zenteru_text::brand::full_name();
    let (app_item, _) = submenu(
        "zenteru",
        vec![
            item(&format!("About {names}"), Some(sel!(orderFrontStandardAboutPanel:)), "", cmd, d, None),
            sep(),
            item("Hide zenteru", Some(sel!(hide:)), "h", cmd, d, None),
            item("Hide Others", Some(sel!(hideOtherApplications:)), "h", NSEventModifierFlags::Command | NSEventModifierFlags::Option, d, None),
            sep(),
            item("Quit zenteru", Some(sel!(terminate:)), "q", cmd, d, None),
        ],
    );
    main.addItem(&app_item);
    let (shell, _) = submenu(
        "Shell",
        vec![
            b("New Tab", "t", cmd, "new_tab"),
            b("New Window", "n", cmd, "new_window"),
            sep(),
            b("Close Tab", "w", cmd, "close_tab"),
        ],
    );
    main.addItem(&shell);
    let (edit, _) = submenu(
        "Edit",
        vec![
            b("Copy", "c", cmd, "copy"),
            b("Paste", "v", cmd, "paste"),
            b("Select All", "a", cmd, "select_all"),
            sep(),
            b("Clear Scrollback", "k", cmd, "clear"),
        ],
    );
    main.addItem(&edit);
    let (view, _) = submenu(
        "View",
        vec![
            b("Bigger", "+", cmd, "bigger"),
            b("Bigger", "=", cmd, "bigger"),
            b("Smaller", "-", cmd, "smaller"),
            b("Actual Size", "0", cmd, "reset_font"),
            sep(),
            b("Focus Side Panel", "`", NSEventModifierFlags::Control, "focus_panel"),
            item("Enter Full Screen", Some(sel!(toggleFullScreen:)), "f", NSEventModifierFlags::Command | NSEventModifierFlags::Control, d, None),
        ],
    );
    main.addItem(&view);
    let mut win_items = vec![
        item("Minimize", Some(sel!(performMiniaturize:)), "m", cmd, d, None),
        sep(),
        b("Next Tab", "]", cs, "next_tab"),
        b("Previous Tab", "[", cs, "prev_tab"),
        b("Next Tab", "\t", NSEventModifierFlags::Control, "next_tab"),
        b("Previous Tab", "\t", NSEventModifierFlags::Control | NSEventModifierFlags::Shift, "prev_tab"),
        sep(),
    ];
    for i in 1..=9 {
        win_items.push(b(&format!("Tab {i}"), &i.to_string(), cmd, &format!("tab_{i}")));
    }
    let (window, wmenu) = submenu("Window", win_items);
    main.addItem(&window);
    let (plugins, pmenu) = submenu("Plugins", vec![b("Command Palette…", "p", cs, "palette"), sep()]);
    main.addItem(&plugins);
    PLUGIN_MENU.with(|m| *m.borrow_mut() = Some(pmenu));
    let app = NSApplication::sharedApplication(mtm);
    app.setMainMenu(Some(&main));
    app.setWindowsMenu(Some(&wmenu));
}

/// Rebuild the Plugins menu from registered palette commands.
pub fn refresh_plugin_menu() {
    let cmds: Vec<(String, String, Option<String>)> = with_app(|a| {
        a.api
            .as_ref()
            .map(|api| {
                api.registrations("command")
                    .into_iter()
                    .map(|r| (r.handler.clone(), r.params.get("title").as_str().unwrap_or(&r.handler).to_string(), r.params.get("keybinding").as_str().map(String::from)))
                    .collect()
            })
            .unwrap_or_default()
    })
    .unwrap_or_default();
    let Some(d) = DELEGATE.with(|d| d.borrow().clone()) else { return };
    PLUGIN_MENU.with(|m| {
        let Some(menu) = m.borrow().clone() else { return };
        while menu.numberOfItems() > 2 {
            menu.removeItemAtIndex(2);
        }
        for (h, t, kb) in cmds {
            let (key, mods) = kb.as_deref().map(parse_keybinding).unwrap_or((String::new(), NSEventModifierFlags::empty()));
            menu.addItem(&item(&t, Some(sel!(pluginCommand:)), &key, mods, &d, Some(&h)));
        }
    });
}

fn parse_keybinding(kb: &str) -> (String, NSEventModifierFlags) {
    let mut m = NSEventModifierFlags::empty();
    let mut key = String::new();
    for p in kb.to_lowercase().split('+') {
        match p {
            "cmd" | "command" => m |= NSEventModifierFlags::Command,
            "shift" => m |= NSEventModifierFlags::Shift,
            "ctrl" | "control" => m |= NSEventModifierFlags::Control,
            "alt" | "opt" | "option" => m |= NSEventModifierFlags::Option,
            k => key = k.to_string(),
        }
    }
    (key, m)
}

pub fn run() {
    let mtm = mtm();
    APP.with(|a| *a.borrow_mut() = Some(App::new()));
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<AppDelegate> = unsafe { msg_send![AppDelegate::alloc(mtm), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with(|d| *d.borrow_mut() = Some(delegate));
    app.run();
}
