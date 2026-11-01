//! Both-modifier chords: left ⌘ + right ⌘ opens Black Magic, left ⌥ +
//! right ⌥ opens the agentic helper. Both keys pressed, then released, with
//! no other key, click or modifier in between. They fire on release, so
//! ordinary shortcuts, which always involve another key, are neither
//! affected nor delayed, and a double tap (one key at a time) never fires.

/// Device-dependent bits of NSEvent.modifierFlags (IOKit's
/// NX_DEVICE{L,R}{CMD,ALT}KEYMASK): which of the pair is down.
const LEFT_CMD: u64 = 0x08;
const RIGHT_CMD: u64 = 0x10;
const LEFT_OPT: u64 = 0x20;
const RIGHT_OPT: u64 = 0x40;
/// Device-independent bits.
const SHIFT: u64 = 1 << 17;
const CONTROL: u64 = 1 << 18;
const OPTION: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;

#[derive(Debug)]
pub struct Chord {
    left: u64,
    right: u64,
    /// Modifiers that make it something else (⌘⇧, ⌥⌘, …).
    others: u64,
    /// One of the pair is down.
    held: bool,
    /// Both were down at once since the first went down.
    both: bool,
    /// Something else happened meanwhile: this was a shortcut, not the chord.
    spoiled: bool,
}

impl Chord {
    pub fn command() -> Self {
        Self::new(LEFT_CMD, RIGHT_CMD, SHIFT | CONTROL | OPTION)
    }

    pub fn option() -> Self {
        Self::new(LEFT_OPT, RIGHT_OPT, SHIFT | CONTROL | COMMAND)
    }

    fn new(left: u64, right: u64, others: u64) -> Self {
        Self { left, right, others, held: false, both: false, spoiled: false }
    }

    /// A modifier key changed (`flags` = raw modifierFlags). True when the
    /// chord just completed.
    pub fn flags(&mut self, flags: u64) -> bool {
        let (left, right) = (flags & self.left != 0, flags & self.right != 0);
        if !left && !right {
            let fire = self.both && !self.spoiled;
            self.reset();
            return fire;
        }
        self.held = true;
        if flags & self.others != 0 {
            self.spoiled = true;
        }
        if left && right {
            self.both = true;
        }
        false
    }

    /// Any key or click: a modifier held now belongs to a shortcut.
    pub fn other_input(&mut self) {
        if self.held {
            self.spoiled = true;
        }
    }

    /// Focus left the window: its key-up events won't arrive here.
    pub fn reset(&mut self) {
        (self.held, self.both, self.spoiled) = (false, false, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: u64 = COMMAND | LEFT_CMD;
    const R: u64 = COMMAND | RIGHT_CMD;
    const LO: u64 = OPTION | LEFT_OPT;
    const RO: u64 = OPTION | RIGHT_OPT;

    fn run(c: &mut Chord, steps: &[u64]) -> bool {
        steps.iter().map(|&f| c.flags(f)).fold(false, |_, fired| fired)
    }

    #[test]
    fn both_then_release_fires() {
        let mut c = Chord::command();
        assert!(run(&mut c, &[L, L | R, R, 0]));
        // Either order, either release order.
        assert!(run(&mut c, &[R, L | R, L, 0]));
        assert!(run(&mut c, &[L, L | R, 0]));
    }

    #[test]
    fn double_taps_do_not_fire() {
        // Both keys must be held at the same time; taps in turn never are.
        let mut c = Chord::command();
        assert!(!run(&mut c, &[L, 0, L, 0]));
        assert!(!run(&mut c, &[R, 0, R, 0]));
        assert!(!run(&mut c, &[L, 0, R, 0]));
        assert!(!run(&mut c, &[R, 0, L, 0]));
        let mut o = Chord::option();
        assert!(!run(&mut o, &[LO, 0, LO, 0]));
        assert!(!run(&mut o, &[LO, 0, RO, 0]));
    }

    #[test]
    fn one_key_alone_does_not_fire() {
        let mut c = Chord::command();
        assert!(!run(&mut c, &[L, 0]));
        assert!(!run(&mut c, &[R, 0]));
    }

    #[test]
    fn a_key_or_click_in_between_makes_it_a_shortcut() {
        let mut c = Chord::command();
        c.flags(L);
        c.flags(L | R);
        c.other_input();
        assert!(!run(&mut c, &[R, 0]));
        // ⌘T with one ⌘, then the other ⌘ joins: still a shortcut.
        c.flags(L);
        c.other_input();
        assert!(!run(&mut c, &[L | R, 0]));
        // Clean afterwards.
        assert!(run(&mut c, &[L, L | R, 0]));
    }

    #[test]
    fn other_modifiers_spoil_it() {
        let mut c = Chord::command();
        assert!(!run(&mut c, &[L, L | R | SHIFT, L | R, 0]));
        assert!(!run(&mut c, &[SHIFT, SHIFT | L, SHIFT | L | R, SHIFT, 0]));
    }

    #[test]
    fn input_without_the_modifier_is_ignored() {
        let mut c = Chord::command();
        c.other_input();
        assert!(run(&mut c, &[L, L | R, 0]));
    }

    #[test]
    fn reset_forgets_a_half_chord() {
        let mut c = Chord::command();
        c.flags(L);
        c.flags(L | R);
        c.reset();
        assert!(!c.flags(0));
    }

    #[test]
    fn option_pair_is_its_own_chord() {
        let mut o = Chord::option();
        assert!(run(&mut o, &[LO, LO | RO, RO, 0]));
        // ⌘ held with both ⌥: ⌥⌘, not the ⌥ chord.
        assert!(!run(&mut o, &[LO, LO | RO | L, LO | RO, 0]));
        // The two chords don't see each other's keys.
        let mut c = Chord::command();
        assert!(!run(&mut c, &[LO, LO | RO, 0]));
        assert!(!run(&mut o, &[L, L | R, 0]));
    }
}
