//! Rendering: Framebuffer einer Sitzung und effiziente Übergabe an die UI.
//!
//! IronRDP liefert Änderungen als dicht gepackte Rechtecke (`DesktopUpdate`) im Format
//! `0x00RRGGBB`. Der [`Framebuffer`] hält den vollständigen Desktop. Der [`Presenter`]
//! konvertiert nur geänderte Bereiche in RGBA-Puffer, die direkt als `slint::Image` angezeigt
//! werden können. Zwei Puffer wechseln sich ab (Double Buffering): Solange die UI den einen
//! Puffer anzeigt, wird der andere aktualisiert – ohne erneute Allokation.
//!
//! Frames werden höchstens einmal pro Tick (~60 Hz) und nur für sichtbare Sitzungen erzeugt.
//! Solange die UI einen Frame noch nicht übernommen hat, wird kein weiterer gesendet
//! (Backpressure statt Event-Stau).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Rechteck mit exklusiver rechter/unterer Kante.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

impl Rect {
    pub fn full(width: u16, height: u16) -> Self {
        Self {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    pub fn union(self, other: Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub fn clamp_to(self, width: u16, height: u16) -> Rect {
        Rect {
            left: self.left.min(width),
            top: self.top.min(height),
            right: self.right.min(width),
            bottom: self.bottom.min(height),
        }
    }
}

/// Vollständiger Remote-Desktop im Format `0x00RRGGBB`.
pub struct Framebuffer {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; usize::from(width) * usize::from(height)],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Direkter Zugriff für lokal gezeichnete Inhalte (Terminal).
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// Passt die Größe an. Gibt `true` zurück, wenn sie sich geändert hat.
    pub fn ensure_size(&mut self, width: u16, height: u16) -> bool {
        if self.width == width && self.height == height {
            return false;
        }
        *self = Self::new(width, height);
        true
    }

    /// Übernimmt ein Teilrechteck (`region` mit *inklusiven* Kanten wie bei IronRDP).
    /// Gibt das betroffene Rechteck (exklusiv) zurück.
    pub fn apply_region(
        &mut self,
        buffer: &[u32],
        left: u16,
        top: u16,
        right_incl: u16,
        bottom_incl: u16,
    ) -> Option<Rect> {
        if right_incl < left || bottom_incl < top || right_incl >= self.width || bottom_incl >= self.height {
            return None;
        }
        let rw = usize::from(right_incl - left) + 1;
        let rh = usize::from(bottom_incl - top) + 1;
        if buffer.len() != rw * rh {
            return None;
        }
        let stride = usize::from(self.width);
        for (row, src) in buffer.chunks_exact(rw).enumerate() {
            let start = (usize::from(top) + row) * stride + usize::from(left);
            self.pixels[start..start + rw].copy_from_slice(src);
        }
        Some(Rect {
            left,
            top,
            right: right_incl + 1,
            bottom: bottom_incl + 1,
        })
    }

    /// Ersetzt den gesamten Inhalt.
    pub fn replace(&mut self, buffer: Vec<u32>, width: u16, height: u16) -> bool {
        if buffer.len() != usize::from(width) * usize::from(height) {
            return false;
        }
        self.width = width;
        self.height = height;
        self.pixels = buffer;
        true
    }
}

/// Konvertiert eine Zeile `0x00RRGGBB` → RGBA (Alpha = 255).
#[inline]
fn convert_row(src: &[u32], dst: &mut [u8]) {
    for (px, out) in src.iter().zip(dst.as_chunks_mut::<4>().0.iter_mut()) {
        let v = *px;
        out[0] = (v >> 16) as u8;
        out[1] = (v >> 8) as u8;
        out[2] = v as u8;
        out[3] = 0xFF;
    }
}

/// Bestätigt, dass die UI einen Frame übernommen hat.
#[derive(Debug, Clone)]
pub struct FrameAck(Arc<AtomicBool>);

impl FrameAck {
    pub fn done(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Ein fertiger Frame für die Anzeige.
pub struct FrameUpdate {
    pub width: u16,
    pub height: u16,
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub ack: FrameAck,
}

impl std::fmt::Debug for FrameUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FrameUpdate({}x{})", self.width, self.height)
    }
}

/// Remote-Mauszeiger als RGBA-Bitmap (nicht vormultipliziert).
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
}

impl std::fmt::Debug for CursorImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CursorImage({}x{})", self.width, self.height)
    }
}

impl CursorImage {
    pub fn from_rgba(width: u16, height: u16, hotspot_x: u16, hotspot_y: u16, rgba: &[u8]) -> Option<Self> {
        if width == 0 || height == 0 || rgba.len() != usize::from(width) * usize::from(height) * 4 {
            return None;
        }
        Some(Self {
            width,
            height,
            hotspot_x,
            hotspot_y,
            pixels: SharedPixelBuffer::clone_from_slice(rgba, u32::from(width), u32::from(height)),
        })
    }
}

/// Erzeugt Frames aus dem Framebuffer mit Double Buffering und Dirty-Tracking.
pub struct Presenter {
    staging: [Option<SharedPixelBuffer<Rgba8Pixel>>; 2],
    /// Seit der letzten Aktualisierung des jeweiligen Puffers geänderter Bereich.
    pending: [Option<Rect>; 2],
    next: usize,
    in_flight: Arc<AtomicBool>,
    size: (u16, u16),
}

impl Default for Presenter {
    fn default() -> Self {
        Self::new()
    }
}

impl Presenter {
    pub fn new() -> Self {
        Self {
            staging: [None, None],
            pending: [None, None],
            next: 0,
            in_flight: Arc::new(AtomicBool::new(false)),
            size: (0, 0),
        }
    }

    pub fn mark_dirty(&mut self, rect: Rect) {
        for slot in &mut self.pending {
            *slot = Some(match *slot {
                Some(r) => r.union(rect),
                None => rect,
            });
        }
    }

    pub fn has_pending(&self) -> bool {
        self.pending[self.next].is_some()
    }

    /// Die UI hat den letzten Frame noch nicht übernommen.
    pub fn is_busy(&self) -> bool {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Erzwingt beim nächsten Aufruf einen vollständigen Frame.
    pub fn invalidate(&mut self, width: u16, height: u16) {
        self.mark_dirty(Rect::full(width, height));
    }

    /// Erzeugt einen Frame, falls Änderungen vorliegen und die UI bereit ist.
    pub fn present(&mut self, fb: &Framebuffer) -> Option<FrameUpdate> {
        if self.is_busy() || fb.width() == 0 || fb.height() == 0 {
            return None;
        }
        let (w, h) = (fb.width(), fb.height());
        if self.size != (w, h) {
            self.size = (w, h);
            self.staging = [None, None];
            self.pending = [Some(Rect::full(w, h)); 2];
        }
        let idx = self.next;
        let dirty = self.pending[idx]?.clamp_to(w, h);
        self.pending[idx] = None;

        let buffer = self.staging[idx].get_or_insert_with(|| SharedPixelBuffer::new(u32::from(w), u32::from(h)));
        if !dirty.is_empty() {
            // Liegt der Puffer noch bei der UI, kopiert `make_mut_bytes` ihn zuerst (korrekt,
            // nur langsamer). Im Normalfall hat die UI ihn bereits freigegeben.
            let bytes = buffer.make_mut_bytes();
            let stride = usize::from(w);
            let src = fb.pixels();
            let (l, r) = (usize::from(dirty.left), usize::from(dirty.right));
            for y in usize::from(dirty.top)..usize::from(dirty.bottom) {
                let s = &src[y * stride + l..y * stride + r];
                let d = &mut bytes[(y * stride + l) * 4..(y * stride + r) * 4];
                convert_row(s, d);
            }
        }
        self.next = 1 - idx;
        self.in_flight.store(true, Ordering::Release);
        Some(FrameUpdate {
            width: w,
            height: h,
            pixels: buffer.clone(),
            ack: FrameAck(self.in_flight.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_region_and_present() {
        let mut fb = Framebuffer::new(4, 3);
        let rect = fb.apply_region(&[0x00FF0000, 0x0000FF00], 1, 1, 2, 1).unwrap();
        assert_eq!(
            rect,
            Rect {
                left: 1,
                top: 1,
                right: 3,
                bottom: 2
            }
        );
        assert_eq!(fb.pixels()[4 + 1], 0x00FF0000);
        // Ungültige Regionen werden verworfen
        assert!(fb.apply_region(&[0; 3], 0, 0, 1, 0).is_none());
        assert!(fb.apply_region(&[0; 1], 4, 0, 4, 0).is_none());

        let mut p = Presenter::new();
        let frame = p.present(&fb).expect("erster Frame ist immer vollständig");
        assert_eq!((frame.width, frame.height), (4, 3));
        let px = frame.pixels.as_slice()[4 + 1];
        assert_eq!((px.r, px.g, px.b, px.a), (255, 0, 0, 255));

        // Solange nicht bestätigt, kommt kein neuer Frame
        p.mark_dirty(rect);
        assert!(p.present(&fb).is_none());
        frame.ack.done();
        drop(frame);
        let f2 = p.present(&fb).unwrap();
        let px = f2.pixels.as_slice()[4 + 2];
        assert_eq!((px.r, px.g, px.b), (0, 255, 0));
    }

    #[test]
    fn double_buffer_catches_up() {
        let mut fb = Framebuffer::new(2, 1);
        let mut p = Presenter::new();
        let f = p.present(&fb).unwrap(); // Puffer 0
        f.ack.done();
        drop(f);
        // Puffer 1 wird erst jetzt erzeugt (vollständig)
        let r = fb.apply_region(&[0x00010203], 0, 0, 0, 0).unwrap();
        p.mark_dirty(r);
        let f = p.present(&fb).unwrap(); // Puffer 1
        f.ack.done();
        drop(f);
        let r = fb.apply_region(&[0x00040506], 1, 0, 1, 0).unwrap();
        p.mark_dirty(r);
        // Puffer 0 muss beide Änderungen enthalten
        let f = p.present(&fb).unwrap();
        let s = f.pixels.as_slice();
        assert_eq!((s[0].r, s[0].g, s[0].b), (1, 2, 3));
        assert_eq!((s[1].r, s[1].g, s[1].b), (4, 5, 6));
    }

    #[test]
    fn nothing_to_present_without_changes() {
        let fb = Framebuffer::new(2, 2);
        let mut p = Presenter::new();
        let f = p.present(&fb).unwrap();
        f.ack.done();
        // Puffer 1 hat noch nie einen Frame erhalten → vollständig
        let f = p.present(&fb).unwrap();
        f.ack.done();
        assert!(p.present(&fb).is_none());
    }
}
