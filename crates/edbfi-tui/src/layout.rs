// SPDX-License-Identifier: AGPL-3.0-only
//! Which screen fits the terminal, and where its parts go.
//!
//! There is one breakpoint function and three screens: two panes when the whole profile fits, tabs from 40×12,
//! and a status strip below that. Every size computation saturates, so no terminal size can underflow.
#![forbid(unsafe_code)]

/// The three screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Cheats and options on the left, weapons on the right. At least 99×30.
    Wide,
    /// One tab at a time. At least 40×12.
    Compact,
    /// A status strip; hotkeys keep working.
    Tiny,
}

impl Layout {
    /// The smallest wide screen.
    pub const WIDE: (u16, u16) = (99, 30);
    /// The smallest compact screen.
    pub const COMPACT: (u16, u16) = (40, 12);

    /// The screen for a terminal of `width` × `height` cells.
    #[must_use]
    pub const fn of(width: u16, height: u16) -> Self {
        if width >= Self::WIDE.0 && height >= Self::WIDE.1 {
            Self::Wide
        } else if width >= Self::COMPACT.0 && height >= Self::COMPACT.1 {
            Self::Compact
        } else {
            Self::Tiny
        }
    }
}

/// A list's visible slice, with the indicator lines for rows above and below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    /// The first item shown.
    pub top: usize,
    /// How many items are shown.
    pub shown: usize,
    /// Whether the first line says how many items are above.
    pub above: bool,
    /// Whether the last line says how many items are below.
    pub below: bool,
}

impl Window {
    /// The slice of `total` items that starts at `top` in `height` lines. Indicators appear only when the list
    /// overflows and there are at least three lines.
    pub(crate) fn at(total: usize, height: usize, top: usize) -> Self {
        let top = top.min(total);
        if total <= height {
            return Self {
                top: 0,
                shown: total,
                above: false,
                below: false,
            };
        }
        let marks = height >= 3;
        let above = marks && top > 0;
        let mut room = height - usize::from(above);
        let below = marks && top + room < total;
        room -= usize::from(below);
        Self {
            top,
            shown: room.min(total - top),
            above,
            below,
        }
    }

    /// The slice that keeps `focus` visible and moves as little as possible from `top`.
    pub(crate) fn follow(total: usize, height: usize, top: usize, focus: Option<usize>) -> Self {
        if total <= height || height == 0 {
            return Self::at(total, height, 0);
        }
        let last_top = total - if height >= 3 { height - 1 } else { height };
        let mut top = top.min(last_top);
        // Each step either moves up to the focus or down by the overflow; an indicator appearing can force one
        // more step, so the loop settles in a few rounds.
        for _ in 0..4 {
            let window = Self::at(total, height, top);
            match focus {
                Some(focus) if focus < top => top = focus,
                Some(focus) if focus >= top + window.shown => {
                    top = (top + focus + 1 - (top + window.shown)).min(last_top);
                }
                _ => return window,
            }
        }
        Self::at(total, height, top)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn breakpoints_follow_the_documented_order() {
        assert_eq!(Layout::of(100, 30), Layout::Wide);
        assert_eq!(Layout::of(99, 30), Layout::Wide);
        assert_eq!(Layout::of(98, 30), Layout::Compact);
        assert_eq!(Layout::of(99, 29), Layout::Compact);
        assert_eq!(Layout::of(99, 20), Layout::Compact);
        assert_eq!(Layout::of(40, 12), Layout::Compact);
        assert_eq!(Layout::of(39, 30), Layout::Tiny);
        assert_eq!(Layout::of(200, 11), Layout::Tiny);
        assert_eq!(Layout::of(0, 0), Layout::Tiny);
    }

    #[test]
    fn indicators_take_lines_only_when_needed() {
        assert_eq!(
            Window::at(8, 7, 0),
            Window {
                top: 0,
                shown: 6,
                above: false,
                below: true
            }
        );
        assert_eq!(
            Window::follow(8, 7, 0, Some(7)),
            Window {
                top: 2,
                shown: 6,
                above: true,
                below: false
            }
        );
        assert_eq!(Window::follow(3, 7, 5, Some(2)).top, 0);
    }

    proptest! {
        #[test]
        fn the_focus_is_always_shown(
            total in 0usize..60,
            height in 0usize..30,
            top in 0usize..80,
            focus in 0usize..60,
        ) {
            let focus = (total > 0).then(|| focus % total);
            let window = Window::follow(total, height, top, focus);
            let lines = window.shown + usize::from(window.above) + usize::from(window.below);
            prop_assert!(lines <= height);
            prop_assert!(window.top + window.shown <= total);
            if let (Some(focus), true) = (focus, height > 0) {
                prop_assert!(window.top <= focus && focus < window.top + window.shown);
            }
            if height >= 3 && total > height {
                prop_assert_eq!(window.above, window.top > 0);
                prop_assert_eq!(window.below, window.top + window.shown < total);
            } else {
                prop_assert!(!window.above && !window.below);
            }
        }
    }
}
