//! ⌘W / ⌘Q on a timer (issue #1), decided by which ⌘ is held:
//!
//! - left ⌘W: closes the tab at once
//! - right ⌘W: closes it after a countdown; ⌘W again (either ⌘) forces it
//! - left or right ⌘Q: quits after a countdown; ⌘Q again forces it
//! - both ⌘ held: closes / quits at once
//!
//! Any other key or click cancels a countdown. Letting go of ⌘ doesn't, and
//! nor does key repeat from holding ⌘W / ⌘Q. Nothing is paused meanwhile:
//! the tab keeps running until the timer runs out.

use std::time::{Duration, Instant};

/// Device-dependent bits of NSEvent.modifierFlags (see src/chord.rs).
const LEFT_CMD: u64 = 0x08;
const RIGHT_CMD: u64 = 0x10;

pub const TAB_DELAY: Duration = Duration::from_millis(1500);
pub const QUIT_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// ⌘W: close the tab.
    Tab,
    /// ⌘Q: quit.
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Both,
}

impl Side {
    /// From raw modifierFlags. Neither bit set (a synthetic event): left.
    pub fn of(flags: u64) -> Side {
        match (flags & LEFT_CMD != 0, flags & RIGHT_CMD != 0) {
            (true, true) => Side::Both,
            (false, true) => Side::Right,
            _ => Side::Left,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub kind: Kind,
    /// The tab ⌘W was pressed on (`Kind::Tab`).
    pub tab: Option<String>,
    pub deadline: Instant,
    pub total: Duration,
}

impl Pending {
    pub fn left(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Close / quit now.
    Now,
    /// Start (or keep) a countdown.
    Wait,
    /// Key repeat while counting down: nothing.
    Ignore,
}

#[derive(Debug, Default)]
pub struct Guard {
    pub pending: Option<Pending>,
}

impl Guard {
    /// ⌘W / ⌘Q pressed on tab `tab`.
    pub fn press(&mut self, kind: Kind, side: Side, repeat: bool, tab: Option<&str>, now: Instant) -> Action {
        if repeat {
            // Held down: repeats never force, and never start one either.
            return Action::Ignore;
        }
        // The same key again forces it; the other close key cancels the
        // countdown and is judged afresh.
        if self.pending.take().is_some_and(|p| p.kind == kind) {
            return Action::Now;
        }
        match (kind, side) {
            (_, Side::Both) | (Kind::Tab, Side::Left) => Action::Now,
            _ => {
                let total = if kind == Kind::Tab { TAB_DELAY } else { QUIT_DELAY };
                self.pending = Some(Pending { kind, tab: tab.map(String::from), deadline: now + total, total });
                Action::Wait
            }
        }
    }

    /// Any other key or click. True when it cancelled a countdown.
    pub fn cancel(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// The countdown ran out: what to do now.
    pub fn expired(&mut self, now: Instant) -> Option<Pending> {
        if self.pending.as_ref().is_some_and(|p| now >= p.deadline) {
            return self.pending.take();
        }
        None
    }
}

/// The overlay text.
pub fn message(p: &Pending, now: Instant) -> String {
    let secs = p.left(now).as_secs_f32();
    match p.kind {
        Kind::Tab => format!("Closing tab in {secs:.1} s · ⌘W closes now · any key cancels"),
        Kind::Quit => format!("Quitting in {secs:.1} s · ⌘Q quits now · any key cancels"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: u64 = 1 << 20;
    const L: u64 = CMD | LEFT_CMD;
    const R: u64 = CMD | RIGHT_CMD;
    const B: u64 = CMD | LEFT_CMD | RIGHT_CMD;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn sides() {
        assert_eq!(Side::of(L), Side::Left);
        assert_eq!(Side::of(R), Side::Right);
        assert_eq!(Side::of(B), Side::Both);
        assert_eq!(Side::of(CMD), Side::Left);
    }

    #[test]
    fn left_w_closes_at_once() {
        let mut g = Guard::default();
        assert_eq!(g.press(Kind::Tab, Side::Left, false, Some("t1"), now()), Action::Now);
        assert!(g.pending.is_none());
    }

    #[test]
    fn right_w_waits_and_either_w_forces() {
        for again in [Side::Left, Side::Right, Side::Both] {
            let mut g = Guard::default();
            let t = now();
            assert_eq!(g.press(Kind::Tab, Side::Right, false, Some("t1"), t), Action::Wait);
            let p = g.pending.clone().unwrap();
            assert_eq!(p.tab.as_deref(), Some("t1"));
            assert_eq!(p.deadline, t + TAB_DELAY);
            assert_eq!(g.press(Kind::Tab, again, false, Some("t1"), t), Action::Now);
            assert!(g.pending.is_none());
        }
    }

    #[test]
    fn q_waits_on_either_side_and_forces_on_again() {
        for side in [Side::Left, Side::Right] {
            let mut g = Guard::default();
            let t = now();
            assert_eq!(g.press(Kind::Quit, side, false, None, t), Action::Wait);
            assert_eq!(g.pending.as_ref().unwrap().deadline, t + QUIT_DELAY);
            assert_eq!(g.press(Kind::Quit, Side::Left, false, None, t), Action::Now);
        }
    }

    #[test]
    fn both_cmds_act_at_once() {
        let mut g = Guard::default();
        assert_eq!(g.press(Kind::Tab, Side::Both, false, Some("t1"), now()), Action::Now);
        assert_eq!(g.press(Kind::Quit, Side::Both, false, None, now()), Action::Now);
        assert!(g.pending.is_none());
    }

    #[test]
    fn repeat_neither_forces_nor_starts() {
        let mut g = Guard::default();
        assert_eq!(g.press(Kind::Tab, Side::Right, true, Some("t1"), now()), Action::Ignore);
        assert!(g.pending.is_none());
        g.press(Kind::Quit, Side::Right, false, None, now());
        assert_eq!(g.press(Kind::Quit, Side::Right, true, None, now()), Action::Ignore);
        assert!(g.pending.is_some());
    }

    #[test]
    fn other_close_key_replaces_the_countdown() {
        let mut g = Guard::default();
        g.press(Kind::Quit, Side::Left, false, None, now());
        // Left ⌘W during a ⌘Q countdown: the quit is off, the tab closes.
        assert_eq!(g.press(Kind::Tab, Side::Left, false, Some("t1"), now()), Action::Now);
        assert!(g.pending.is_none());
        g.press(Kind::Tab, Side::Right, false, Some("t1"), now());
        assert_eq!(g.press(Kind::Quit, Side::Right, false, None, now()), Action::Wait);
        assert_eq!(g.pending.as_ref().unwrap().kind, Kind::Quit);
    }

    #[test]
    fn cancel_and_expiry() {
        let mut g = Guard::default();
        assert!(!g.cancel());
        let t = now();
        g.press(Kind::Tab, Side::Right, false, Some("t1"), t);
        assert!(g.cancel());
        assert!(g.pending.is_none());
        g.press(Kind::Tab, Side::Right, false, Some("t1"), t);
        assert!(g.expired(t + TAB_DELAY / 2).is_none());
        let p = g.expired(t + TAB_DELAY).unwrap();
        assert_eq!(p.tab.as_deref(), Some("t1"));
        assert!(g.pending.is_none());
        // After a cancel, ⌘W starts afresh rather than forcing.
        g.press(Kind::Tab, Side::Right, false, Some("t1"), t);
        g.cancel();
        assert_eq!(g.press(Kind::Tab, Side::Right, false, Some("t1"), t), Action::Wait);
    }

    #[test]
    fn message_counts_down() {
        let t = now();
        let p = Pending { kind: Kind::Quit, tab: None, deadline: t + QUIT_DELAY, total: QUIT_DELAY };
        assert!(message(&p, t).starts_with("Quitting in 3.0 s"));
        assert!(message(&p, t + QUIT_DELAY * 2).starts_with("Quitting in 0.0 s"));
    }
}
