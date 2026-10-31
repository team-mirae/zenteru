//! The zencode "Sakura night" palette, including the pink → violet
//! gradient, in 24-bit RGB. Values match team-mirae/zencode `src/theme.rs`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(mix(self.0, other.0), mix(self.1, other.1), mix(self.2, other.2))
    }

    pub fn to_f32(self) -> [f32; 4] {
        [self.0 as f32 / 255.0, self.1 as f32 / 255.0, self.2 as f32 / 255.0, 1.0]
    }
}

pub const BG: Rgb = Rgb(24, 22, 34);
pub const FG: Rgb = Rgb(224, 219, 240);
pub const CUR_LINE_BG: Rgb = Rgb(38, 35, 56);
pub const BAR_BG: Rgb = Rgb(30, 28, 44);
pub const MUTED: Rgb = Rgb(92, 87, 124);
pub const LABEL: Rgb = Rgb(185, 180, 212);
pub const GRADIENT_FROM: Rgb = Rgb(255, 121, 176);
pub const GRADIENT_TO: Rgb = Rgb(130, 120, 255);
pub const ON_GRADIENT: Rgb = Rgb(22, 18, 32);
pub const ACCENT_GOLD: Rgb = Rgb(255, 205, 125);
pub const ACCENT_MINT: Rgb = Rgb(120, 225, 205);
pub const ERROR: Rgb = Rgb(255, 110, 120);
pub const SELECT_BG: Rgb = Rgb(86, 70, 140);
pub const ZENKAKU_SPACE_BG: Rgb = Rgb(58, 52, 86);
pub const CURSOR: Rgb = Rgb(255, 160, 200);

/// Gradient colour at `t` in 0..=1.
pub fn gradient(t: f32) -> Rgb {
    GRADIENT_FROM.lerp(GRADIENT_TO, t.clamp(0.0, 1.0))
}

/// The 16 ANSI colours, tuned to sit on the sakura background.
pub const ANSI: [Rgb; 16] = [
    Rgb(38, 35, 56),
    Rgb(255, 110, 120),
    Rgb(120, 225, 165),
    Rgb(255, 205, 125),
    Rgb(130, 150, 255),
    Rgb(255, 121, 176),
    Rgb(120, 225, 205),
    Rgb(224, 219, 240),
    Rgb(92, 87, 124),
    Rgb(255, 145, 155),
    Rgb(160, 240, 195),
    Rgb(255, 225, 165),
    Rgb(165, 180, 255),
    Rgb(255, 160, 200),
    Rgb(165, 240, 225),
    Rgb(255, 255, 255),
];

/// xterm 256-colour palette entry.
pub fn ansi256(i: u8) -> Rgb {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = i - 16;
            Rgb(STEPS[(i / 36) as usize], STEPS[(i / 6 % 6) as usize], STEPS[(i % 6) as usize])
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            Rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_ends() {
        assert_eq!(gradient(0.0), GRADIENT_FROM);
        assert_eq!(gradient(1.0), GRADIENT_TO);
        assert_eq!(ansi256(16), Rgb(0, 0, 0));
        assert_eq!(ansi256(231), Rgb(255, 255, 255));
        assert_eq!(ansi256(232), Rgb(8, 8, 8));
    }
}
