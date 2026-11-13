//! Schriftrasterung für das Terminal.
//!
//! Verwendet die Schriften von Windows (Cascadia Mono bzw. Consolas, Symbole aus Segoe UI
//! Symbol); es werden keine Schriftdateien mitgeliefert. Die Schriften werden einmal pro
//! Prozess geladen, gerasterte Zeichen je Sitzung und Schriftgröße zwischengespeichert.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use fontdue::{Font, FontSettings};

/// Erste vorhandene Datei je Rolle: Hauptschrift, fette Variante, Ersatzschriften.
const PRIMARY: &[&str] = &["CascadiaMono.ttf", "consola.ttf", "cour.ttf"];
const FALLBACK: &[&str] = &["consola.ttf", "seguisym.ttf", "segoeui.ttf", "arial.ttf"];

struct Fonts {
    /// `[0]` ist die Hauptschrift; weitere nur für Zeichen, die ihr fehlen.
    chain: Vec<Font>,
}

fn fonts_dir() -> PathBuf {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(windir).join("Fonts")
}

fn load(name: &str) -> Option<Font> {
    let bytes = std::fs::read(fonts_dir().join(name)).ok()?;
    match Font::from_bytes(bytes, FontSettings::default()) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!(font = name, error = e, "Schrift konnte nicht geladen werden");
            None
        }
    }
}

fn fonts() -> Option<&'static Fonts> {
    static FONTS: OnceLock<Option<Fonts>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let (primary_name, primary) = PRIMARY.iter().find_map(|n| load(n).map(|f| (*n, f)))?;
            let mut chain = vec![primary];
            for name in FALLBACK.iter().filter(|n| **n != primary_name) {
                if let Some(f) = load(name) {
                    chain.push(f);
                }
            }
            tracing::info!(
                font = primary_name,
                fallbacks = chain.len() - 1,
                "Terminalschrift geladen"
            );
            Some(Fonts { chain })
        })
        .as_ref()
}

/// Ein gerastertes Zeichen, Position relativ zur linken oberen Ecke der Zelle.
pub struct Glyph {
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
    /// Deckung 0–255, zeilenweise.
    pub coverage: Vec<u8>,
}

/// Schrift in einer festen Pixelgröße mit Zellmaßen und Zeichen-Cache.
pub struct GlyphCache {
    px: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    baseline: i32,
    cache: HashMap<(char, bool), Option<Glyph>>,
}

impl GlyphCache {
    /// `None`, wenn keine der Systemschriften verfügbar ist.
    pub fn new(px: f32) -> Option<Self> {
        let fonts = fonts()?;
        let primary = &fonts.chain[0];
        let px = px.clamp(6.0, 96.0);
        let (ascent, descent) = match primary.horizontal_line_metrics(px) {
            Some(m) => (m.ascent, m.descent),
            None => (px * 0.8, -px * 0.2),
        };
        // Ohne Zeilenabstand, damit Rahmenzeichen lückenlos aneinanderstoßen.
        let cell_height = (ascent - descent).ceil().max(1.0) as u32;
        let cell_width = primary.metrics('M', px).advance_width.round().max(1.0) as u32;
        Some(Self {
            px,
            cell_width,
            cell_height,
            baseline: ascent.round() as i32,
            cache: HashMap::new(),
        })
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    /// Gerastertes Zeichen; `None` für Leerzeichen und nicht darstellbare Zeichen.
    pub fn glyph(&mut self, ch: char, bold: bool) -> Option<&Glyph> {
        let (px, baseline) = (self.px, self.baseline);
        self.cache
            .entry((ch, bold))
            .or_insert_with(|| rasterize(ch, bold, px, baseline))
            .as_ref()
    }
}

fn rasterize(ch: char, bold: bool, px: f32, baseline: i32) -> Option<Glyph> {
    if ch == ' ' || ch.is_control() {
        return None;
    }
    let fonts = fonts()?;
    let font = fonts
        .chain
        .iter()
        .find(|f| f.lookup_glyph_index(ch) != 0)
        .unwrap_or(&fonts.chain[0]);
    let (m, mut coverage) = font.rasterize(ch, px);
    if m.width == 0 || m.height == 0 {
        return None;
    }
    let mut width = m.width;
    if bold {
        // Fettdruck durch um ein Pixel versetztes Überlagern
        let mut wide = vec![0u8; (width + 1) * m.height];
        for y in 0..m.height {
            for x in 0..width {
                let v = coverage[y * width + x];
                let row = y * (width + 1);
                wide[row + x] = wide[row + x].max(v);
                wide[row + x + 1] = wide[row + x + 1].max(v);
            }
        }
        coverage = wide;
        width += 1;
    }
    Some(Glyph {
        left: m.xmin,
        top: baseline - (m.height as i32 + m.ymin),
        width,
        height: m.height,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn system_font_gives_monospace_cells() {
        let mut g = GlyphCache::new(16.0).expect("Windows bringt Consolas/Cascadia mit");
        assert!(g.cell_width >= 6 && g.cell_width <= 16, "Zellbreite {}", g.cell_width);
        assert!(g.cell_height >= 14 && g.cell_height <= 26, "Zellhöhe {}", g.cell_height);
        let h = g.cell_height as i32;
        let a = g.glyph('A', false).expect("A hat Pixel");
        assert!(a.top >= 0 && a.top + a.height as i32 <= h + 1);
        let normal = a.width;
        assert_eq!(g.glyph('A', true).unwrap().width, normal + 1);
        assert!(g.glyph(' ', false).is_none());
        // Rahmenzeichen kommen aus der Haupt- oder Ersatzschrift
        assert!(g.glyph('─', false).is_some());
    }
}
