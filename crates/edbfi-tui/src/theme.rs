// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dashboard's visual vocabulary: named tones, the colour depth to draw them with, and the glyphs.
//!
//! Tones are semantic, so a screen never picks a colour directly. The terminal's own background is always kept;
//! only foreground colour and weight change. Glyphs come from the WGL4 repertoire that Windows console fonts
//! carry, with an ASCII set for hosts whose font lacks them.
#![forbid(unsafe_code)]

use ratatui::style::{Color, Modifier, Style};

/// What a piece of text means, and so how it looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Ordinary text.
    Text,
    /// Secondary text: rules, hints, unbound keys, cleared rows.
    Muted,
    /// Titles and the brand.
    Accent,
    /// Applied in the game.
    On,
    /// Applied through another selected cheat.
    Via,
    /// Something waits: a saved choice before the hook, a change the game has not confirmed, or a warning.
    Armed,
    /// Selected while every cheat is paused.
    Paused,
    /// Something failed or needs the user.
    Error,
    /// The focused row or button.
    Focus,
}

/// How many colours the terminal can show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Colour {
    /// 24-bit colour, for hosts known to support it.
    TrueColour,
    /// The 16 standard colours, which every Windows console supports.
    #[default]
    Ansi16,
    /// No colour: weight, reverse video and words only. Used for `NO_COLOR`.
    Mono,
}

/// Which characters draw rules, markers and ellipses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Glyphs {
    /// Box drawing, triangles and bullets from WGL4.
    #[default]
    Unicode,
    /// Plain ASCII, for fonts that lack WGL4.
    Ascii,
}

/// The colour depth and glyph set together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Appearance {
    /// The colour depth.
    pub colour: Colour,
    /// The glyph set.
    pub glyphs: Glyphs,
}

impl Appearance {
    /// The appearance for the terminal the environment describes, read through `var`: no colour when
    /// `NO_COLOR` is set and not empty, 24-bit colour in Windows Terminal (`WT_SESSION`), otherwise the 16
    /// standard colours. Glyphs are always WGL4; a font that lacks them needs an explicit override.
    #[must_use]
    pub fn detect(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        let set = |name| var(name).is_some_and(|value| !value.is_empty());
        let colour = if set("NO_COLOR") {
            Colour::Mono
        } else if set("WT_SESSION") {
            Colour::TrueColour
        } else {
            Colour::Ansi16
        };
        Self {
            colour,
            glyphs: Glyphs::Unicode,
        }
    }
}

/// The characters one [`Glyphs`] set draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphSet {
    /// A horizontal rule.
    pub rule: &'static str,
    /// A vertical rule.
    pub bar: &'static str,
    /// The focused row.
    pub focus: &'static str,
    /// A selected row.
    pub selected: &'static str,
    /// More rows below.
    pub more_below: &'static str,
    /// More rows above.
    pub more_above: &'static str,
    /// A separator between short facts.
    pub dot: &'static str,
    /// Text that was cut short.
    pub ellipsis: &'static str,
    /// Box corners: top left, top right, bottom left, bottom right.
    pub corners: [&'static str; 4],
}

const UNICODE: GlyphSet = GlyphSet {
    rule: "─",
    bar: "│",
    focus: "►",
    selected: "●",
    more_below: "▼",
    more_above: "▲",
    dot: "·",
    ellipsis: "…",
    corners: ["┌", "┐", "└", "┘"],
};

const ASCII: GlyphSet = GlyphSet {
    rule: "-",
    bar: "|",
    focus: ">",
    selected: "*",
    more_below: "v",
    more_above: "^",
    dot: "-",
    ellipsis: "~",
    corners: ["+", "+", "+", "+"],
};

impl Appearance {
    /// The glyphs to draw with.
    #[must_use]
    pub const fn glyphs(self) -> &'static GlyphSet {
        match self.glyphs {
            Glyphs::Unicode => &UNICODE,
            Glyphs::Ascii => &ASCII,
        }
    }

    /// The style of `tone`.
    #[must_use]
    pub fn style(self, tone: Tone) -> Style {
        let plain = Style::new();
        match self.colour {
            Colour::Mono => match tone {
                Tone::Text | Tone::Via => plain,
                Tone::Muted => plain.add_modifier(Modifier::DIM),
                Tone::Accent | Tone::On | Tone::Armed => plain.add_modifier(Modifier::BOLD),
                Tone::Paused => plain.add_modifier(Modifier::ITALIC),
                Tone::Error => plain.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                Tone::Focus => plain.add_modifier(Modifier::REVERSED),
            },
            Colour::Ansi16 => match tone {
                Tone::Text => plain,
                Tone::Muted => plain.fg(Color::DarkGray),
                Tone::Accent => plain.fg(Color::Yellow),
                Tone::On => plain.fg(Color::LightGreen),
                Tone::Via => plain.fg(Color::Cyan),
                Tone::Armed => plain.fg(Color::LightYellow),
                Tone::Paused => plain.fg(Color::LightBlue),
                Tone::Error => plain.fg(Color::LightRed),
                Tone::Focus => plain.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            },
            Colour::TrueColour => match tone {
                Tone::Text => plain,
                Tone::Muted => plain.fg(Color::Rgb(138, 132, 120)),
                Tone::Accent => plain.fg(Color::Rgb(196, 160, 86)),
                Tone::On => plain.fg(Color::Rgb(126, 196, 120)),
                Tone::Via => plain.fg(Color::Rgb(108, 180, 190)),
                Tone::Armed => plain.fg(Color::Rgb(232, 196, 110)),
                Tone::Paused => plain.fg(Color::Rgb(150, 160, 222)),
                Tone::Error => plain.fg(Color::Rgb(232, 110, 96)),
                Tone::Focus => plain
                    .fg(Color::Rgb(232, 196, 110))
                    .add_modifier(Modifier::BOLD),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_picks_a_conservative_colour_depth() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        assert_eq!(Appearance::detect(env(&[])), Appearance::default());
        let wt = Appearance::detect(env(&[("WT_SESSION", "x")]));
        assert_eq!(wt.colour, Colour::TrueColour);
        let mono = Appearance::detect(env(&[("WT_SESSION", "x"), ("NO_COLOR", "1")]));
        assert_eq!(mono.colour, Colour::Mono);
        let empty = Appearance::detect(env(&[("NO_COLOR", "")]));
        assert_eq!(empty.colour, Colour::Ansi16);
    }

    #[test]
    fn every_tone_is_distinct_within_a_colour_mode() {
        const TONES: [Tone; 8] = [
            Tone::Muted,
            Tone::Accent,
            Tone::On,
            Tone::Via,
            Tone::Armed,
            Tone::Paused,
            Tone::Error,
            Tone::Focus,
        ];
        for colour in [Colour::TrueColour, Colour::Ansi16] {
            let appearance = Appearance {
                colour,
                glyphs: Glyphs::Unicode,
            };
            for (i, a) in TONES.iter().enumerate() {
                for b in &TONES[i + 1..] {
                    assert_ne!(appearance.style(*a), appearance.style(*b), "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn mono_and_glyph_sets_avoid_colour_and_non_ascii() {
        let mono = Appearance {
            colour: Colour::Mono,
            glyphs: Glyphs::Ascii,
        };
        for tone in [Tone::Text, Tone::Error, Tone::Focus, Tone::On] {
            let style = mono.style(tone);
            assert_eq!((style.fg, style.bg), (None, None));
        }
        let set = mono.glyphs();
        let all = [
            set.rule,
            set.bar,
            set.focus,
            set.selected,
            set.more_below,
            set.more_above,
            set.dot,
            set.ellipsis,
        ];
        assert!(all.iter().chain(&set.corners).all(|glyph| glyph.is_ascii()));
        assert!(all.iter().all(|glyph| crate::text::width(glyph) == 1));
        let unicode = Appearance::default().glyphs();
        assert!(
            [unicode.rule, unicode.focus, unicode.ellipsis]
                .iter()
                .all(|glyph| crate::text::width(glyph) == 1)
        );
    }
}
