//! Terminal einer SSH-Sitzung: Emulation (`alacritty_terminal`), Darstellung in einen
//! Framebuffer und Übersetzung von Tastatur, Maus und Einfügen in Terminal-Eingaben.
//!
//! Läuft im Sitzungs-Thread; die UI bekommt fertige Bilder wie bei RDP.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb};

use super::font::GlyphCache;
use crate::rdp::input::{KeyMods, MouseButton, TermKey};
use crate::rdp::renderer::{Framebuffer, Rect};

/// Zeilen im Verlauf (Scrollback).
const SCROLLBACK: usize = 10_000;
/// Zeilen je Mausrad-Raste.
const WHEEL_LINES: i32 = 3;
const MULTI_CLICK: Duration = Duration::from_millis(400);

const DEFAULT_FG: Rgb = Rgb {
    r: 0xdc,
    g: 0xdf,
    b: 0xe4,
};
const DEFAULT_BG: Rgb = Rgb {
    r: 0x12,
    g: 0x14,
    b: 0x1a,
};
const SELECTION_BG: Rgb = Rgb {
    r: 0x3a,
    g: 0x4a,
    b: 0x6b,
};

/// Die 16 Grundfarben (dunkles Schema, passend zur Oberfläche).
const PALETTE16: [Rgb; 16] = [
    Rgb {
        r: 0x28,
        g: 0x2c,
        b: 0x34,
    },
    Rgb {
        r: 0xe0,
        g: 0x6c,
        b: 0x75,
    },
    Rgb {
        r: 0x98,
        g: 0xc3,
        b: 0x79,
    },
    Rgb {
        r: 0xe5,
        g: 0xc0,
        b: 0x7b,
    },
    Rgb {
        r: 0x61,
        g: 0xaf,
        b: 0xef,
    },
    Rgb {
        r: 0xc6,
        g: 0x78,
        b: 0xdd,
    },
    Rgb {
        r: 0x56,
        g: 0xb6,
        b: 0xc2,
    },
    Rgb {
        r: 0xdc,
        g: 0xdf,
        b: 0xe4,
    },
    Rgb {
        r: 0x5a,
        g: 0x63,
        b: 0x74,
    },
    Rgb {
        r: 0xff,
        g: 0x7a,
        b: 0x85,
    },
    Rgb {
        r: 0xb5,
        g: 0xe8,
        b: 0x90,
    },
    Rgb {
        r: 0xff,
        g: 0xd6,
        b: 0x8a,
    },
    Rgb {
        r: 0x8c,
        g: 0xc8,
        b: 0xff,
    },
    Rgb {
        r: 0xde,
        g: 0x9c,
        b: 0xf0,
    },
    Rgb {
        r: 0x7f,
        g: 0xd4,
        b: 0xdf,
    },
    Rgb {
        r: 0xff,
        g: 0xff,
        b: 0xff,
    },
];

/// xterm-256-Farbtabelle: Grundfarben, 6×6×6-Würfel, Graustufen.
fn palette(index: usize) -> Rgb {
    match index {
        0..=15 => PALETTE16[index],
        16..=231 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ => DEFAULT_FG,
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb {
        r: (u16::from(c.r) * 2 / 3) as u8,
        g: (u16::from(c.g) * 2 / 3) as u8,
        b: (u16::from(c.b) * 2 / 3) as u8,
    }
}

fn pack(c: Rgb) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

/// Standardwert einer benannten Farbe (Index nach `NamedColor`), falls das Programm sie nicht
/// per OSC geändert hat.
fn named_default(index: usize) -> Rgb {
    const FOREGROUND: usize = NamedColor::Foreground as usize;
    const BACKGROUND: usize = NamedColor::Background as usize;
    const CURSOR: usize = NamedColor::Cursor as usize;
    const DIM_BLACK: usize = NamedColor::DimBlack as usize;
    const DIM_WHITE: usize = NamedColor::DimWhite as usize;
    const BRIGHT_FG: usize = NamedColor::BrightForeground as usize;
    const DIM_FG: usize = NamedColor::DimForeground as usize;
    match index {
        0..=255 => palette(index),
        FOREGROUND | CURSOR => DEFAULT_FG,
        BACKGROUND => DEFAULT_BG,
        DIM_BLACK..=DIM_WHITE => dim(palette(index - DIM_BLACK)),
        BRIGHT_FG => PALETTE16[15],
        DIM_FG => dim(DEFAULT_FG),
        _ => DEFAULT_FG,
    }
}

fn lookup(colors: &Colors, index: usize) -> Rgb {
    colors[index].unwrap_or_else(|| named_default(index))
}

fn resolve(colors: &Colors, color: Color, flags: Flags, foreground: bool) -> Rgb {
    let bright = foreground && flags.contains(Flags::BOLD);
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => {
            let i = usize::from(i);
            lookup(colors, if bright && i < 8 { i + 8 } else { i })
        }
        Color::Named(n) => {
            let i = n as usize;
            lookup(colors, if bright && i < 8 { i + 8 } else { i })
        }
    };
    if foreground && flags.contains(Flags::DIM) {
        dim(rgb)
    } else {
        rgb
    }
}

/// Sammelt Ereignisse des Emulators (Antworten an den Server, Titel …).
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
    }
}

/// Ergebnis eines Mausklicks.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MouseOutcome {
    /// An den Server zu senden (Mausberichte für Programme wie htop oder mc).
    pub bytes: Vec<u8>,
    /// In die Zwischenablage zu kopieren.
    pub copy: Option<String>,
    /// Inhalt der Zwischenablage soll eingefügt werden.
    pub paste: bool,
}

#[derive(Default)]
struct MouseState {
    x: u16,
    y: u16,
    left: bool,
    middle: bool,
    right: bool,
    /// Zelle des letzten Mausberichts (Bewegungen nur bei Zellwechsel melden).
    reported: Option<(usize, usize)>,
    /// Startpunkt einer Auswahl mit gedrückter linker Taste.
    anchor: Option<(Point, Side)>,
    last_click: Option<(Instant, Point)>,
    clicks: u8,
    wheel: i32,
}

pub struct TermView {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    glyphs: GlyphCache,
    fb: Framebuffer,
    cols: usize,
    rows: usize,
    dirty: bool,
    mouse: MouseState,
    title: Option<String>,
}

impl TermView {
    /// `None`, wenn keine Terminalschrift verfügbar ist.
    pub fn new(width: u16, height: u16, font_px: f32) -> Option<Self> {
        let glyphs = GlyphCache::new(font_px)?;
        let (cols, rows) = grid_size(width, height, &glyphs);
        let events = Listener::default();
        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        let term = Term::new(config, &TermSize::new(cols, rows), events.clone());
        Some(Self {
            term,
            parser: Processor::new(),
            events,
            glyphs,
            fb: Framebuffer::new(width, height),
            cols,
            rows,
            dirty: true,
            mouse: MouseState::default(),
            title: None,
        })
    }

    pub fn grid(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    pub fn size_px(&self) -> (u16, u16) {
        (self.fb.width(), self.fb.height())
    }

    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Ausgabe des Servers verarbeiten. Liefert Antworten, die an den Server gehen müssen
    /// (z. B. auf Abfragen der Cursorposition).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        self.dirty = true;
        self.take_responses()
    }

    fn take_responses(&mut self) -> Vec<u8> {
        let events = std::mem::take(&mut *self.events.0.lock().unwrap_or_else(|e| e.into_inner()));
        let mut out = Vec::new();
        for event in events {
            match event {
                Event::PtyWrite(text) => out.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => {
                    out.extend_from_slice(format(lookup(self.term.colors(), index)).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = WindowSize {
                        num_lines: self.rows as u16,
                        num_cols: self.cols as u16,
                        cell_width: self.glyphs.cell_width as u16,
                        cell_height: self.glyphs.cell_height as u16,
                    };
                    out.extend_from_slice(format(size).as_bytes());
                }
                Event::Title(t) => self.title = Some(t),
                Event::ResetTitle => self.title = None,
                // Zwischenablage nicht ohne Zutun des Benutzers aus der Sitzung lesen oder
                // überschreiben lassen (OSC 52).
                Event::ClipboardStore(..) | Event::ClipboardLoad(..) => {
                    tracing::debug!("Zugriff auf die Zwischenablage aus der Sitzung ignoriert");
                }
                _ => {}
            }
        }
        out
    }

    /// Neue Flächengröße in Pixeln bzw. Schriftgröße. Liefert `(Spalten, Zeilen)`, wenn sich das
    /// Raster geändert hat (dann muss der Server informiert werden).
    pub fn resize(&mut self, width: u16, height: u16, font_px: f32) -> Option<(usize, usize)> {
        if (font_px - self.glyphs.px()).abs() > 0.01
            && let Some(g) = GlyphCache::new(font_px)
        {
            self.glyphs = g;
        }
        self.fb.ensure_size(width, height);
        self.dirty = true;
        let (cols, rows) = grid_size(width, height, &self.glyphs);
        if (cols, rows) == (self.cols, self.rows) {
            return None;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(TermSize::new(cols, rows));
        Some((cols, rows))
    }

    /// Zeichnet neu, falls sich etwas geändert hat. Liefert den geänderten Bereich.
    pub fn render(&mut self) -> Option<Rect> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let (w, h) = (self.fb.width(), self.fb.height());
        if w == 0 || h == 0 {
            return None;
        }
        let Self { term, glyphs, fb, .. } = self;
        let content = term.renderable_content();
        let colors = content.colors;
        let mode = content.mode;
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let cursor = content.cursor;
        let show_cursor = mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
        let (cw, ch) = (glyphs.cell_width as usize, glyphs.cell_height as usize);
        let stride = usize::from(w);
        let height = usize::from(h);
        let pixels = fb.pixels_mut();
        pixels.fill(pack(lookup(colors, NamedColor::Background as usize)));

        for indexed in content.display_iter {
            let cell = indexed.cell;
            let point = indexed.point;
            let row = point.line.0 + offset;
            if row < 0 {
                continue;
            }
            let (row, col) = (row as usize, point.column.0);
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let mut fg = resolve(colors, cell.fg, cell.flags, true);
            let mut bg = resolve(colors, cell.bg, cell.flags, false);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if selection.is_some_and(|s| s.contains(point)) {
                bg = SELECTION_BG;
            }
            let is_cursor = show_cursor && point == cursor.point;
            if is_cursor && cursor.shape == CursorShape::Block {
                let cursor_color = lookup(colors, NamedColor::Cursor as usize);
                fg = bg;
                bg = cursor_color;
            }
            let span = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let x0 = col * cw;
            let y0 = row * ch;
            if x0 >= stride || y0 >= height {
                continue;
            }
            let x1 = (x0 + cw * span).min(stride);
            let y1 = (y0 + ch).min(height);
            fill(pixels, stride, x0, y0, x1, y1, pack(bg));

            if cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN) {
                let bold = cell.flags.contains(Flags::BOLD);
                if let Some(glyph) = glyphs.glyph(cell.c, bold) {
                    blend_glyph(pixels, stride, (x0, y0, x1, y1), glyph, fg);
                }
            }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                let y = (y0 + ch).saturating_sub(2).max(y0);
                fill(pixels, stride, x0, y, x1, (y + 1).min(y1), pack(fg));
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                let y = y0 + ch / 2;
                fill(pixels, stride, x0, y, x1, (y + 1).min(y1), pack(fg));
            }
            if is_cursor {
                let c = pack(lookup(colors, NamedColor::Cursor as usize));
                let thick = (ch / 10).max(2);
                match cursor.shape {
                    CursorShape::Underline => fill(pixels, stride, x0, y1.saturating_sub(thick), x1, y1, c),
                    CursorShape::Beam => fill(pixels, stride, x0, y0, (x0 + thick).min(x1), y1, c),
                    CursorShape::HollowBlock => {
                        fill(pixels, stride, x0, y0, x1, y0 + 1, c);
                        fill(pixels, stride, x0, y1 - 1, x1, y1, c);
                        fill(pixels, stride, x0, y0, x0 + 1, y1, c);
                        fill(pixels, stride, x1 - 1, y0, x1, y1, c);
                    }
                    CursorShape::Block | CursorShape::Hidden => {}
                }
            }
        }
        Some(Rect::full(w, h))
    }

    // ------------------------------------------------------------ Tastatur

    /// Zeichen vom Benutzer. Springt ans Ende des Verlaufs.
    pub fn input_char(&mut self, ch: char, alt: bool) -> Vec<u8> {
        self.scroll_to_bottom();
        encode_char(ch, alt)
    }

    pub fn input_key(&mut self, key: TermKey, mods: KeyMods) -> Vec<u8> {
        // Umschalt+Bild↑/↓ blättert lokal im Verlauf (außer in Vollbildprogrammen)
        if mods.shift && !mods.ctrl && !mods.alt && !self.term.mode().contains(TermMode::ALT_SCREEN) {
            match key {
                TermKey::PageUp => {
                    self.term.scroll_display(Scroll::PageUp);
                    self.dirty = true;
                    return Vec::new();
                }
                TermKey::PageDown => {
                    self.term.scroll_display(Scroll::PageDown);
                    self.dirty = true;
                    return Vec::new();
                }
                _ => {}
            }
        }
        self.scroll_to_bottom();
        encode_key(key, mods, self.term.mode().contains(TermMode::APP_CURSOR))
    }

    pub fn paste(&mut self, text: &str) -> Vec<u8> {
        self.scroll_to_bottom();
        encode_paste(text, self.term.mode().contains(TermMode::BRACKETED_PASTE))
    }

    fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
            self.dirty = true;
        }
    }

    /// Markierten Text kopieren (Auswahl bleibt bestehen).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    // ------------------------------------------------------------ Maus

    /// Zelle unter einer Pixelposition: (Bildschirmzeile, Spalte, Seite innerhalb der Zelle).
    fn cell_at(&self, x: u16, y: u16) -> (usize, usize, Side) {
        let (cw, ch) = (self.glyphs.cell_width.max(1), self.glyphs.cell_height.max(1));
        let col = (u32::from(x) / cw).min(self.cols.saturating_sub(1) as u32) as usize;
        let row = (u32::from(y) / ch).min(self.rows.saturating_sub(1) as u32) as usize;
        let side = if u32::from(x) % cw < cw / 2 {
            Side::Left
        } else {
            Side::Right
        };
        (row, col, side)
    }

    fn grid_point(&self, row: usize, col: usize) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        Point::new(Line(row as i32 - offset), Column(col))
    }

    fn reporting(&self, mods: KeyMods) -> bool {
        // Umschalt erzwingt die lokale Auswahl, wie in anderen Terminals üblich.
        self.term.mode().intersects(TermMode::MOUSE_MODE) && !mods.shift
    }

    fn report(&self, code: u8, pressed: bool, row: usize, col: usize, mods: KeyMods) -> Vec<u8> {
        let code =
            code + if mods.shift { 4 } else { 0 } + if mods.alt { 8 } else { 0 } + if mods.ctrl { 16 } else { 0 };
        if self.term.mode().contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            return format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes();
        }
        // Klassisches Format: Loslassen ohne Tastenangabe, Koordinaten bis 223
        let code = if pressed { code } else { (code & !0b11) | 3 };
        if col + 1 > 223 || row + 1 > 223 {
            return Vec::new();
        }
        vec![0x1b, b'[', b'M', 32 + code, 32 + (col + 1) as u8, 32 + (row + 1) as u8]
    }

    pub fn mouse_move(&mut self, x: u16, y: u16, mods: KeyMods) -> Vec<u8> {
        self.mouse.x = x;
        self.mouse.y = y;
        let (row, col, side) = self.cell_at(x, y);
        let any = self.mouse.left || self.mouse.middle || self.mouse.right;
        if self.reporting(mods) {
            let mode = *self.term.mode();
            let wants = mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && any);
            if !wants || self.mouse.reported == Some((row, col)) {
                return Vec::new();
            }
            self.mouse.reported = Some((row, col));
            let button = if self.mouse.left {
                0
            } else if self.mouse.middle {
                1
            } else if self.mouse.right {
                2
            } else {
                3
            };
            return self.report(32 + button, true, row, col, mods);
        }
        if self.mouse.left {
            let point = self.grid_point(row, col);
            match self.term.selection.as_mut() {
                Some(sel) => sel.update(point, side),
                None => {
                    if let Some((start, start_side)) = self.mouse.anchor
                        && (start != point || start_side != side)
                    {
                        let mut sel = Selection::new(SelectionType::Simple, start, start_side);
                        sel.update(point, side);
                        self.term.selection = Some(sel);
                    }
                }
            }
            self.dirty = true;
        }
        Vec::new()
    }

    pub fn mouse_button(&mut self, button: MouseButton, pressed: bool, mods: KeyMods) -> MouseOutcome {
        let (row, col, side) = self.cell_at(self.mouse.x, self.mouse.y);
        let held = match button {
            MouseButton::Left => &mut self.mouse.left,
            MouseButton::Middle => &mut self.mouse.middle,
            MouseButton::Right => &mut self.mouse.right,
            MouseButton::X1 | MouseButton::X2 => return MouseOutcome::default(),
        };
        let was = *held;
        *held = pressed;
        if !pressed && !was {
            return MouseOutcome::default();
        }

        if self.reporting(mods) {
            let code = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                _ => 2,
            };
            self.mouse.reported = Some((row, col));
            return MouseOutcome {
                bytes: self.report(code, pressed, row, col, mods),
                ..Default::default()
            };
        }

        let point = self.grid_point(row, col);
        match (button, pressed) {
            (MouseButton::Left, true) => {
                let now = Instant::now();
                self.mouse.clicks = match self.mouse.last_click {
                    Some((at, p)) if p == point && now.duration_since(at) < MULTI_CLICK => self.mouse.clicks % 3 + 1,
                    _ => 1,
                };
                self.mouse.last_click = Some((now, point));
                self.mouse.anchor = Some((point, side));
                self.term.selection = match self.mouse.clicks {
                    2 => Some(Selection::new(SelectionType::Semantic, point, side)),
                    3 => Some(Selection::new(SelectionType::Lines, point, side)),
                    _ => None,
                };
                self.dirty = true;
                MouseOutcome::default()
            }
            (MouseButton::Left, false) => {
                self.mouse.anchor = None;
                MouseOutcome::default()
            }
            // Rechtsklick: Auswahl kopieren, sonst einfügen (wie Windows Terminal)
            (MouseButton::Right, true) => match self.selection_text() {
                Some(text) => {
                    self.term.selection = None;
                    self.dirty = true;
                    MouseOutcome {
                        copy: Some(text),
                        ..Default::default()
                    }
                }
                None => MouseOutcome {
                    paste: true,
                    ..Default::default()
                },
            },
            (MouseButton::Middle, true) => MouseOutcome {
                paste: true,
                ..Default::default()
            },
            _ => MouseOutcome::default(),
        }
    }

    /// Mausrad in 1/120 Rasten (positiv = nach oben).
    pub fn wheel(&mut self, units: i16, mods: KeyMods) -> Vec<u8> {
        self.mouse.wheel += i32::from(units);
        let notches = self.mouse.wheel / 120;
        if notches == 0 {
            return Vec::new();
        }
        self.mouse.wheel -= notches * 120;
        let mode = *self.term.mode();
        if self.reporting(mods) {
            let (row, col, _) = self.cell_at(self.mouse.x, self.mouse.y);
            let code = if notches > 0 { 64 } else { 65 };
            return (0..notches.abs())
                .flat_map(|_| self.report(code, true, row, col, mods))
                .collect();
        }
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let key = if notches > 0 { TermKey::Up } else { TermKey::Down };
            let one = encode_key(key, KeyMods::default(), mode.contains(TermMode::APP_CURSOR));
            return (0..notches.abs() * WHEEL_LINES).flat_map(|_| one.clone()).collect();
        }
        self.term.scroll_display(Scroll::Delta(notches * WHEEL_LINES));
        self.dirty = true;
        Vec::new()
    }
}

fn grid_size(width: u16, height: u16, glyphs: &GlyphCache) -> (usize, usize) {
    let cols = (u32::from(width) / glyphs.cell_width.max(1)).max(2) as usize;
    let rows = (u32::from(height) / glyphs.cell_height.max(1)).max(1) as usize;
    (cols, rows)
}

fn fill(pixels: &mut [u32], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize, color: u32) {
    for y in y0..y1 {
        let row = y * stride;
        if x0 < x1 && row + x1 <= pixels.len() {
            pixels[row + x0..row + x1].fill(color);
        }
    }
}

/// Zeichnet ein Zeichen mit Kantenglättung, beschnitten auf `clip` (x0, y0, x1, y1).
fn blend_glyph(
    pixels: &mut [u32],
    stride: usize,
    clip: (usize, usize, usize, usize),
    glyph: &super::font::Glyph,
    fg: Rgb,
) {
    let (cx0, cy0, cx1, cy1) = clip;
    for gy in 0..glyph.height {
        let y = cy0 as i32 + glyph.top + gy as i32;
        if y < cy0 as i32 || y >= cy1 as i32 {
            continue;
        }
        for gx in 0..glyph.width {
            let a = u32::from(glyph.coverage[gy * glyph.width + gx]);
            if a == 0 {
                continue;
            }
            let x = cx0 as i32 + glyph.left + gx as i32;
            if x < cx0 as i32 || x >= cx1 as i32 {
                continue;
            }
            let idx = y as usize * stride + x as usize;
            let Some(px) = pixels.get_mut(idx) else { continue };
            let bg = *px;
            let mix = |f: u8, b: u32| (u32::from(f) * a + b * (255 - a)) / 255;
            let r = mix(fg.r, (bg >> 16) & 0xFF);
            let g = mix(fg.g, (bg >> 8) & 0xFF);
            let b = mix(fg.b, bg & 0xFF);
            *px = (r << 16) | (g << 8) | b;
        }
    }
}

/// Zeichen → Bytes. Rücktaste sendet DEL (wie xterm), Strg+Rücktaste ^H; Alt stellt ESC voran.
pub fn encode_char(ch: char, alt: bool) -> Vec<u8> {
    let ch = match ch {
        '\u{8}' => '\u{7f}',
        '\u{7f}' => '\u{8}',
        c => c,
    };
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    out
}

/// Taste → Escape-Folge im xterm-Format.
pub fn encode_key(key: TermKey, mods: KeyMods, app_cursor: bool) -> Vec<u8> {
    let m = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    let modified = m > 1;
    // Folgen der Form CSI 1;m X bzw. SS3 X
    let letter = |c: char| -> Vec<u8> {
        if modified {
            format!("\x1b[1;{m}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    // Folgen der Form CSI n ~ bzw. CSI n;m ~
    let tilde = |n: u8| -> Vec<u8> {
        if modified {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    match key {
        TermKey::Up => letter('A'),
        TermKey::Down => letter('B'),
        TermKey::Right => letter('C'),
        TermKey::Left => letter('D'),
        TermKey::Home => letter('H'),
        TermKey::End => letter('F'),
        TermKey::Insert => tilde(2),
        TermKey::Delete => tilde(3),
        TermKey::PageUp => tilde(5),
        TermKey::PageDown => tilde(6),
        TermKey::BackTab => b"\x1b[Z".to_vec(),
        TermKey::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if modified {
                format!("\x1b[1;{m}{c}").into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        TermKey::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        TermKey::F(_) => Vec::new(),
    }
}

/// Text zum Einfügen. Zeilenumbrüche werden zu CR (wie beim Tippen). Mit Bracketed Paste
/// erkennt die Shell den Block als Einfügung und führt ihn nicht Zeile für Zeile aus; ESC wird
/// entfernt, damit der Text den Block nicht vorzeitig beenden kann.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let inner: String = normalized.chars().filter(|c| *c != '\x1b').collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(inner.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        normalized.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(shift: bool, alt: bool, ctrl: bool) -> KeyMods {
        KeyMods { shift, alt, ctrl }
    }

    #[test]
    fn keys_follow_xterm() {
        let none = KeyMods::default();
        assert_eq!(encode_key(TermKey::Up, none, false), b"\x1b[A");
        assert_eq!(encode_key(TermKey::Up, none, true), b"\x1bOA");
        assert_eq!(encode_key(TermKey::Left, mods(false, false, true), true), b"\x1b[1;5D");
        assert_eq!(encode_key(TermKey::Right, mods(true, true, false), false), b"\x1b[1;4C");
        assert_eq!(encode_key(TermKey::Delete, none, false), b"\x1b[3~");
        assert_eq!(
            encode_key(TermKey::PageUp, mods(false, false, true), false),
            b"\x1b[5;5~"
        );
        assert_eq!(encode_key(TermKey::F(1), none, false), b"\x1bOP");
        assert_eq!(encode_key(TermKey::F(4), mods(true, false, false), false), b"\x1b[1;2S");
        assert_eq!(encode_key(TermKey::F(5), none, false), b"\x1b[15~");
        assert_eq!(encode_key(TermKey::F(12), none, false), b"\x1b[24~");
        assert_eq!(encode_key(TermKey::BackTab, none, false), b"\x1b[Z");
    }

    #[test]
    fn chars_and_paste() {
        assert_eq!(encode_char('a', false), b"a");
        assert_eq!(encode_char('ä', false), "ä".as_bytes());
        assert_eq!(encode_char('x', true), b"\x1bx");
        assert_eq!(encode_char('\u{8}', false), b"\x7f");
        assert_eq!(encode_char('\u{3}', false), b"\x03");
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(
            encode_paste("ls\x1b[201~rm", true),
            b"\x1b[200~ls[201~rm\x1b[201~".to_vec()
        );
    }

    #[cfg(windows)]
    fn view() -> TermView {
        TermView::new(800, 400, 16.0).expect("Systemschrift vorhanden")
    }

    #[test]
    #[cfg(windows)]
    fn renders_text_and_colors() {
        let mut t = view();
        let (cols, rows) = t.grid();
        assert!(cols > 40 && rows > 10);
        let replies = t.feed(b"\x1b[41mX\x1b[0m hallo");
        assert!(replies.is_empty());
        assert!(t.render().is_some());
        assert!(t.render().is_none(), "ohne Änderung kein neues Bild");
        let px = t.framebuffer().pixels();
        // Erste Zelle hat roten Hintergrund (Ecke ist nicht vom Zeichen bedeckt)
        assert_eq!(px[0], pack(PALETTE16[1]));
        // Irgendwo im Text gibt es helle Vordergrundpixel
        let w = usize::from(t.size_px().0);
        let ch = t.glyphs.cell_height as usize;
        let lit = (0..ch).any(|y| (0..w).any(|x| px[y * w + x] == pack(DEFAULT_FG)));
        assert!(lit);
    }

    #[test]
    #[cfg(windows)]
    fn answers_cursor_position_query() {
        let mut t = view();
        let reply = t.feed(b"ab\x1b[6n");
        assert_eq!(reply, b"\x1b[1;3R");
    }

    #[test]
    #[cfg(windows)]
    fn resize_changes_grid() {
        let mut t = view();
        let before = t.grid();
        assert_eq!(t.resize(800, 400, 16.0), None);
        let after = t.resize(400, 200, 16.0).expect("kleineres Raster");
        assert!(after.0 < before.0 && after.1 < before.1);
        assert_eq!(t.size_px(), (400, 200));
    }

    #[test]
    #[cfg(windows)]
    fn select_and_copy() {
        let mut t = view();
        t.feed(b"hello world");
        let cw = t.glyphs.cell_width as u16;
        let none = KeyMods::default();
        t.mouse_move(1, 2, none);
        t.mouse_button(MouseButton::Left, true, none);
        t.mouse_move(cw * 5 - 1, 2, none);
        t.mouse_button(MouseButton::Left, false, none);
        assert_eq!(t.selection_text().as_deref(), Some("hello"));
        // Rechtsklick kopiert und hebt die Auswahl auf, der nächste fügt ein
        let out = t.mouse_button(MouseButton::Right, true, none);
        assert_eq!(out.copy.as_deref(), Some("hello"));
        t.mouse_button(MouseButton::Right, false, none);
        assert!(t.mouse_button(MouseButton::Right, true, none).paste);
    }

    #[test]
    #[cfg(windows)]
    fn mouse_reports_when_requested() {
        let mut t = view();
        // htop & Co.: Klickberichte im SGR-Format
        t.feed(b"\x1b[?1000h\x1b[?1006h");
        let none = KeyMods::default();
        let (cw, ch) = (t.glyphs.cell_width as u16, t.glyphs.cell_height as u16);
        t.mouse_move(cw * 2 + 1, ch * 3 + 1, none);
        let down = t.mouse_button(MouseButton::Left, true, none);
        assert_eq!(down.bytes, b"\x1b[<0;3;4M");
        let up = t.mouse_button(MouseButton::Left, false, none);
        assert_eq!(up.bytes, b"\x1b[<0;3;4m");
        assert_eq!(t.wheel(120, none), b"\x1b[<64;3;4M");
        // Umschalt erzwingt lokale Auswahl
        assert!(
            t.mouse_button(MouseButton::Left, true, mods(true, false, false))
                .bytes
                .is_empty()
        );
    }

    #[test]
    #[cfg(windows)]
    fn wheel_scrolls_history() {
        let mut t = view();
        let rows = t.grid().1;
        for i in 0..rows * 3 {
            t.feed(format!("Zeile {i}\r\n").as_bytes());
        }
        assert!(t.wheel(120, KeyMods::default()).is_empty());
        assert_eq!(t.term.grid().display_offset(), WHEEL_LINES as usize);
        // Tippen springt zurück ans Ende
        assert_eq!(t.input_char('a', false), b"a");
        assert_eq!(t.term.grid().display_offset(), 0);
    }
}
