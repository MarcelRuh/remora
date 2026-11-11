//! Protokollunabhängige Eingabeereignisse und Koordinaten-Abbildung.

use crate::database::ScaleMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

/// Eingabe für eine Sitzung. Koordinaten sind bereits Remote-Pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// PC/AT-Set-1-Scancode; `extended` entspricht dem E0-Präfix.
    Key {
        scancode: u8,
        extended: bool,
        pressed: bool,
    },
    /// Unicode-Zeichen (für Texteingaben ohne Scancode, z. B. "Zwischenablage eintippen").
    Unicode {
        ch: char,
        pressed: bool,
    },
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Mausrad in Einheiten von 1/120 Raste (positiv = nach oben bzw. rechts).
    Wheel {
        vertical: bool,
        units: i16,
    },
    /// Alle gedrückten Tasten und Maustasten loslassen (bei Fokusverlust).
    ReleaseAll,
    /// Zustand der Feststelltasten synchronisieren.
    SyncLocks {
        caps: bool,
        num: bool,
        scroll: bool,
    },
    /// Strg+Alt+Entf an die Remote-Sitzung (lokal nicht abfangbar).
    CtrlAltDel,
}

/// Rechteck des Remote-Bildes innerhalb der Anzeigefläche (logische Pixel).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Berechnet, wo das Remote-Bild in der Anzeigefläche liegt.
///
/// `device_scale` ist der Skalierungsfaktor des Fensters (physische / logische Pixel) und
/// wird für den 1:1-Modus benötigt.
pub fn view_rect(
    view_w: f32,
    view_h: f32,
    remote_w: u16,
    remote_h: u16,
    mode: ScaleMode,
    device_scale: f32,
) -> ViewRect {
    let rw = f32::from(remote_w.max(1));
    let rh = f32::from(remote_h.max(1));
    match mode {
        ScaleMode::Stretch => ViewRect {
            x: 0.0,
            y: 0.0,
            width: view_w,
            height: view_h,
        },
        ScaleMode::Original => {
            let s = device_scale.max(0.1);
            ViewRect {
                x: 0.0,
                y: 0.0,
                width: rw / s,
                height: rh / s,
            }
        }
        ScaleMode::Fit => {
            let scale = (view_w / rw).min(view_h / rh);
            let w = rw * scale;
            let h = rh * scale;
            ViewRect {
                x: (view_w - w) / 2.0,
                y: (view_h - h) / 2.0,
                width: w,
                height: h,
            }
        }
    }
}

/// Bildet eine lokale Mausposition (logische Pixel relativ zur Anzeigefläche) auf
/// Remote-Koordinaten ab. Positionen außerhalb des Bildes werden an den Rand geklemmt.
pub fn map_pointer(local_x: f32, local_y: f32, rect: ViewRect, remote_w: u16, remote_h: u16) -> (u16, u16) {
    if rect.width <= 0.0 || rect.height <= 0.0 || remote_w == 0 || remote_h == 0 {
        return (0, 0);
    }
    let fx = (local_x - rect.x) / rect.width * f32::from(remote_w);
    let fy = (local_y - rect.y) / rect.height * f32::from(remote_h);
    let x = fx.floor().clamp(0.0, f32::from(remote_w - 1));
    let y = fy.floor().clamp(0.0, f32::from(remote_h - 1));
    (x as u16, y as u16)
}

/// Wandelt Scroll-Deltas (logische Pixel, wie von Slint geliefert) in Rad-Einheiten um.
/// Bruchteile werden akkumuliert, damit Touchpads präzise scrollen.
#[derive(Debug, Default, Clone, Copy)]
pub struct WheelAccumulator {
    vertical: f32,
    horizontal: f32,
}

impl WheelAccumulator {
    /// Slint liefert für eine Mausrad-Raste typischerweise 60 logische Pixel.
    const PIXELS_PER_NOTCH: f32 = 60.0;

    pub fn feed(&mut self, delta_x: f32, delta_y: f32) -> Vec<InputEvent> {
        let mut out = Vec::new();
        self.vertical += delta_y / Self::PIXELS_PER_NOTCH * 120.0;
        self.horizontal += delta_x / Self::PIXELS_PER_NOTCH * 120.0;
        for (acc, vertical) in [(&mut self.vertical, true), (&mut self.horizontal, false)] {
            // RDP erlaubt pro Ereignis maximal ±255 Einheiten.
            while acc.abs() >= 1.0 {
                let chunk = acc.clamp(-255.0, 255.0).trunc();
                *acc -= chunk;
                // Horizontal: positive Werte bedeuten in RDP "nach rechts".
                let units = if vertical { chunk } else { -chunk };
                out.push(InputEvent::Wheel {
                    vertical,
                    units: units as i16,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_and_maps() {
        // 1000x500-Fläche, 1920x1080-Desktop → Höhe begrenzt
        let r = view_rect(1000.0, 500.0, 1920, 1080, ScaleMode::Fit, 1.0);
        assert!((r.height - 500.0).abs() < 0.01);
        assert!((r.width - 888.89).abs() < 0.1);
        assert!(r.x > 0.0 && r.y.abs() < 0.01);
        // Mitte bleibt Mitte
        let (x, y) = map_pointer(500.0, 250.0, r, 1920, 1080);
        assert_eq!((x, y), (960, 540));
        // Außerhalb wird geklemmt
        assert_eq!(map_pointer(-50.0, 9999.0, r, 1920, 1080), (0, 1079));
    }

    #[test]
    fn original_respects_device_scale() {
        let r = view_rect(3000.0, 3000.0, 1920, 1080, ScaleMode::Original, 1.5);
        assert!((r.width - 1280.0).abs() < 0.01);
        let (x, y) = map_pointer(640.0, 360.0, r, 1920, 1080);
        assert_eq!((x, y), (960, 540));
    }

    #[test]
    fn stretch_fills_view() {
        let r = view_rect(800.0, 600.0, 1920, 1080, ScaleMode::Stretch, 1.0);
        assert_eq!(map_pointer(799.9, 599.9, r, 1920, 1080), (1919, 1079));
    }

    #[test]
    fn wheel_accumulates_fractions() {
        let mut acc = WheelAccumulator::default();
        assert!(acc.feed(0.0, 0.2).is_empty());
        let ev = acc.feed(0.0, 60.0);
        assert_eq!(
            ev,
            vec![InputEvent::Wheel {
                vertical: true,
                units: 120
            }]
        );
        let big = acc.feed(0.0, 600.0);
        assert!(
            big.iter()
                .all(|e| matches!(e, InputEvent::Wheel { units, .. } if units.abs() <= 255))
        );
        let total: i32 = big
            .iter()
            .map(|e| match e {
                InputEvent::Wheel { units, .. } => i32::from(*units),
                _ => 0,
            })
            .sum();
        assert_eq!(total, 1200);
    }
}
