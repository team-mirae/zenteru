//! Left ⌘ + right ⌘ opens Black Magic: both pressed, then released, with no
//! other key, click or modifier in between. It fires on release, so ordinary
//! ⌘ shortcuts, which always involve another key, are neither affected nor
//! delayed.

/// Device-dependent bits of NSEvent.modifierFlags (IOKit's
/// NX_DEVICELCMDKEYMASK / NX_DEVICERCMDKEYMASK): which ⌘ is down.
const LEFT_CMD: u64 = 0x08;
const RIGHT_CMD: u64 = 0x10;
/// Shift, Control and Option (device-independent bits).
const OTHER_MODS: u64 = (1 << 17) | (1 << 18) | (1 << 19);

#[derive(Default, Debug)]
pub struct CmdChord {
    /// A ⌘ key is down.
    held: bool,
    /// Both ⌘ keys were down at once since the first went down.
    both: bool,
    /// Something else happened meanwhile: this was a shortcut, not the chord.
    spoiled: bool,
}

impl CmdChord {
    /// A modifier key changed (`flags` = raw modifierFlags). True when the
    /// chord just completed.
    pub fn flags(&mut self, flags: u64) -> bool {
        let (left, right) = (flags & LEFT_CMD != 0, flags & RIGHT_CMD != 0);
        if !left && !right {
            let fire = self.both && !self.spoiled;
            *self = Self::default();
            return fire;
        }
        self.held = true;
        if flags & OTHER_MODS != 0 {
            self.spoiled = true;
        }
        if left && right {
            self.both = true;
        }
        false
    }

    /// Any key or click: a ⌘ held now belongs to a shortcut.
    pub fn other_input(&mut self) {
        if self.held {
            self.spoiled = true;
        }
    }

    /// Focus left the window: its key-up events won't arrive here.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: u64 = 1 << 20;
    const L: u64 = CMD | LEFT_CMD;
    const R: u64 = CMD | RIGHT_CMD;
    const SHIFT: u64 = 1 << 17;

    fn run(c: &mut CmdChord, steps: &[u64]) -> bool {
        steps.iter().map(|&f| c.flags(f)).fold(false, |_, fired| fired)
    }

    #[test]
    fn both_then_release_fires() {
        let mut c = CmdChord::default();
        assert!(run(&mut c, &[L, L | R, R, 0]));
        // Either order, either release order.
        assert!(run(&mut c, &[R, L | R, L, 0]));
        assert!(run(&mut c, &[L, L | R, 0]));
    }

    #[test]
    fn double_taps_do_not_fire() {
        // Both ⌘ keys must be held at the same time; taps in turn never are.
        let mut c = CmdChord::default();
        assert!(!run(&mut c, &[L, 0, L, 0]));
        assert!(!run(&mut c, &[R, 0, R, 0]));
        assert!(!run(&mut c, &[L, 0, R, 0]));
        assert!(!run(&mut c, &[R, 0, L, 0]));
    }

    #[test]
    fn one_cmd_alone_does_not_fire() {
        let mut c = CmdChord::default();
        assert!(!run(&mut c, &[L, 0]));
        assert!(!run(&mut c, &[R, 0]));
    }

    #[test]
    fn a_key_or_click_in_between_makes_it_a_shortcut() {
        let mut c = CmdChord::default();
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
        let mut c = CmdChord::default();
        assert!(!run(&mut c, &[L, L | R | SHIFT, L | R, 0]));
        assert!(!run(&mut c, &[SHIFT, SHIFT | L, SHIFT | L | R, SHIFT, 0]));
    }

    #[test]
    fn input_without_cmd_is_ignored() {
        let mut c = CmdChord::default();
        c.other_input();
        assert!(run(&mut c, &[L, L | R, 0]));
    }

    #[test]
    fn reset_forgets_a_half_chord() {
        let mut c = CmdChord::default();
        c.flags(L);
        c.flags(L | R);
        c.reset();
        assert!(!c.flags(0));
    }
}
