// SPDX-License-Identifier: AGPL-3.0-only
//! Drawing primitives. Every string passes through [`text::sanitize`] on its way to the buffer, and every
//! position is clipped to the drawing area, so no input can escape its cell range or the terminal's control.
#![forbid(unsafe_code)]

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::text;
use crate::theme::{Appearance, GlyphSet, Tone};

/// A paragraph: text that wraps as a unit, in one tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Para {
    pub(crate) text: String,
    pub(crate) tone: Tone,
}

impl Para {
    pub(crate) fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    pub(crate) fn blank() -> Self {
        Self::new("", Tone::Text)
    }
}

/// One wrapped line of [`Para`]s.
pub(crate) type Wrapped = (String, Tone);

/// Wraps `paras` to `width` cells; an empty paragraph is a blank line.
pub(crate) fn wrap_paras(paras: &[Para], width: usize) -> Vec<Wrapped> {
    paras
        .iter()
        .flat_map(|para| {
            let clean = text::sanitize(&para.text);
            text::wrap(&clean, width)
                .into_iter()
                .map(move |line| (line, para.tone))
        })
        .collect()
}

/// A buffer region drawn in one appearance.
pub(crate) struct Canvas<'b> {
    buf: &'b mut Buffer,
    area: Rect,
    pub(crate) look: Appearance,
}

impl<'b> Canvas<'b> {
    pub(crate) fn new(buf: &'b mut Buffer, area: Rect, look: Appearance) -> Self {
        let area = area.intersection(buf.area);
        Self { buf, area, look }
    }

    pub(crate) const fn width(&self) -> u16 {
        self.area.width
    }

    pub(crate) const fn height(&self) -> u16 {
        self.area.height
    }

    pub(crate) const fn glyphs(&self) -> &'static GlyphSet {
        self.look.glyphs()
    }

    pub(crate) fn style(&self, tone: Tone) -> Style {
        self.look.style(tone)
    }

    /// Writes `content` at column `x` of row `y`, in at most `max` cells and never past the right edge. Returns
    /// the column after the last cell written.
    pub(crate) fn put(&mut self, x: u16, y: u16, content: &str, max: u16, style: Style) -> u16 {
        if x >= self.area.width || y >= self.area.height || max == 0 {
            return x;
        }
        let room = max.min(self.area.width - x);
        let clean = text::sanitize(content);
        let fitted = text::fit_end(&clean, usize::from(room), self.glyphs().ellipsis);
        let (end, _) = self.buf.set_stringn(
            self.area.x + x,
            self.area.y + y,
            &fitted,
            usize::from(room),
            style,
        );
        end - self.area.x
    }

    /// Like [`Canvas::put`] in a tone.
    pub(crate) fn say(&mut self, x: u16, y: u16, content: &str, max: u16, tone: Tone) -> u16 {
        let style = self.style(tone);
        self.put(x, y, content, max, style)
    }

    /// Writes `content` so that it ends at column `right` (exclusive), clipped at column `left`.
    pub(crate) fn put_right(&mut self, left: u16, right: u16, y: u16, content: &str, style: Style) {
        let room = right.saturating_sub(left);
        let clean = text::sanitize(content);
        let fitted = text::fit_end(&clean, usize::from(room), self.glyphs().ellipsis);
        let w = u16::try_from(text::width(&fitted)).unwrap_or(room);
        self.put(right.saturating_sub(w), y, &fitted, w, style);
    }

    /// Restyles `width` cells of row `y` from column `x`, keeping their symbols.
    pub(crate) fn restyle(&mut self, x: u16, y: u16, width: u16, style: Style) {
        if x >= self.area.width || y >= self.area.height {
            return;
        }
        let width = width.min(self.area.width - x);
        let area = Rect::new(self.area.x + x, self.area.y + y, width, 1);
        self.buf.set_style(area, style);
    }

    /// A horizontal rule across row `y` from column `x`, `width` cells long.
    pub(crate) fn rule(&mut self, x: u16, y: u16, width: u16) {
        let rule = self.glyphs().rule.repeat(usize::from(width));
        self.say(x, y, &rule, width, Tone::Muted);
    }

    /// Clears `area`, relative to the canvas, to blank default cells.
    pub(crate) fn clear(&mut self, area: Rect) {
        let area = Rect::new(
            self.area.x + area.x,
            self.area.y + area.y,
            area.width,
            area.height,
        )
        .intersection(self.area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = self.buf.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }
    }

    /// Draws a box border around `area`, relative to the canvas, with `title` on its top edge.
    pub(crate) fn frame(&mut self, area: Rect, title: &str, tone: Tone) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let glyphs = self.glyphs();
        let [tl, tr, bl, br] = glyphs.corners;
        let (left, top) = (area.x, area.y);
        let right = area.x + area.width - 1;
        let bottom = area.y + area.height - 1;
        self.clear(area);
        self.say(left, top, tl, 1, Tone::Muted);
        self.rule(left + 1, top, area.width - 2);
        self.say(right, top, tr, 1, Tone::Muted);
        for y in top + 1..bottom {
            self.say(left, y, glyphs.bar, 1, Tone::Muted);
            self.say(right, y, glyphs.bar, 1, Tone::Muted);
        }
        self.say(left, bottom, bl, 1, Tone::Muted);
        self.rule(left + 1, bottom, area.width - 2);
        self.say(right, bottom, br, 1, Tone::Muted);
        if area.width > 6 {
            let end = self.say(left + 2, top, " ", 1, Tone::Muted);
            let end = self.say(end, top, title, area.width - 6, tone);
            self.say(end, top, " ", 1, Tone::Muted);
        }
    }
}
