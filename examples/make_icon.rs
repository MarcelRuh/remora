//! Erzeugt aus der Markenvorlage `assets/brand/remora-logo.webp` alle Bilder der Anwendung:
//!
//! * `assets/brand/remora-mark.png` – Emblem (Sechseck) ohne Schriftzug, 512 × 512, für die
//!   Oberfläche (Sidebar, „Über“, Fenstersymbol),
//! * `assets/brand/remora-mark-32.png`, `remora-mark-64.png` – vorskaliert für Titelleiste und Sidebar,
//! * `assets/brand/remora-logo.png` – vollständiges Logo mit Schriftzug, 512 × 512,
//! * `assets/remora.ico` – Programmsymbol in allen Größen (16–256 px), aus dem Emblem.
//! * `installer/images/wizard-*.bmp` – Bilder des Setup-Assistenten (Logo bzw. Emblem).
//!
//! PNG-Vorschauen aller ICO-Größen landen in `target/icon-preview/`.
//!
//! ```text
//! cargo run --example make_icon
//! ```

use std::io::Write as _;
use std::path::Path;

use image::imageops::FilterType;
use image::{GenericImageView as _, RgbaImage};

const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
/// Ausschnitt des Emblems in der Vorlage, relativ zur Kantenlänge (x, y, Seite).
const MARK_CROP: (f32, f32, f32) = (0.19, 0.05, 0.61);

/// Quadratisches Emblem mit abgerundeten Ecken (Windows zeigt Symbole ohne eigenen Rahmen).
fn rounded(mut img: RgbaImage) -> RgbaImage {
    let size = img.width() as f32;
    let r = size * 0.16;
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = (r - fx).max(fx - (size - r)).max(0.0);
        let dy = (r - fy).max(fy - (size - r)).max(0.0);
        let dist = (dx * dx + dy * dy).sqrt();
        let coverage = (r - dist + 0.5).clamp(0.0, 1.0);
        px[3] = (px[3] as f32 * coverage).round() as u8;
    }
    img
}

/// Legt `img` mit Alpha auf eine Hintergrundfarbe (24-Bit, für BMP).
fn on_background(img: &RgbaImage, w: u32, h: u32, bg: [u8; 3]) -> image::RgbImage {
    let mut canvas = image::RgbImage::from_pixel(w, h, image::Rgb(bg));
    let (ox, oy) = ((w - img.width()) / 2, (h - img.height()) / 2);
    for (x, y, px) in img.enumerate_pixels() {
        let a = px[3] as u32;
        let dst = canvas.get_pixel_mut(ox + x, oy + y);
        for c in 0..3 {
            dst[c] = ((px[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
        }
    }
    canvas
}

/// Bilder des Inno-Setup-Assistenten (`installer/images/`) in mehreren DPI-Stufen:
/// großes Bild links auf Willkommens-/Abschlussseite (Logo mit Schriftzug) und kleines Bild
/// oben rechts (Emblem). Inno Setup wählt die passende Größe selbst.
fn write_installer_images(root: &Path, logo: &RgbaImage, mark: &RgbaImage) {
    // Hintergrundfarbe der Vorlage: Mittel über die obere Randzeile, damit das Bild nahtlos
    // in die Füllfläche übergeht.
    let mut sum = [0u32; 3];
    for x in 0..logo.width() {
        for (c, s) in sum.iter_mut().enumerate() {
            *s += logo.get_pixel(x, 0)[c] as u32;
        }
    }
    let bg = sum.map(|s| (s / logo.width()) as u8);
    let dir = root.join("installer/images");
    std::fs::create_dir_all(&dir).expect("installer/images");
    for (w, h) in [(164, 314), (246, 459), (328, 628)] {
        let scaled = image::imageops::resize(logo, w, w, FilterType::Lanczos3);
        on_background(&scaled, w, h, bg)
            .save(dir.join(format!("wizard-{w}x{h}.bmp")))
            .expect("Wizard-Bild");
    }
    for s in [55, 83, 110] {
        let side = s * 9 / 10;
        let scaled = image::imageops::resize(mark, side, side, FilterType::Lanczos3);
        on_background(&scaled, s, s, bg)
            .save(dir.join(format!("wizard-small-{s}.bmp")))
            .expect("kleines Wizard-Bild");
    }
}

fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("PNG kodieren");
    out.into_inner()
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let brand = root.join("assets/brand");

    let source = image::open(brand.join("remora-logo.webp")).expect("assets/brand/remora-logo.webp");
    let (w, h) = source.dimensions();
    let edge = w.min(h) as f32;
    let (cx, cy, cs) = MARK_CROP;
    let side = (edge * cs).round() as u32;
    let mark = source
        .crop_imm((edge * cx).round() as u32, (edge * cy).round() as u32, side, side)
        .to_rgba8();
    let mark = rounded(image::imageops::resize(&mark, 512, 512, FilterType::Lanczos3));
    mark.save(brand.join("remora-mark.png")).expect("remora-mark.png");
    // Vorskalierte Fassungen für kleine Darstellungen (Slint skaliert große Bilder ohne
    // Tiefpassfilter herunter, das ergibt bei 16–34 px sichtbares Flimmern).
    for size in [32, 64] {
        let small = image::imageops::resize(&mark, size, size, FilterType::Lanczos3);
        small
            .save(brand.join(format!("remora-mark-{size}.png")))
            .expect("remora-mark-<size>.png");
    }
    let logo = image::imageops::resize(&source.to_rgba8(), 512, 512, FilterType::Lanczos3);
    logo.save(brand.join("remora-logo.png")).expect("remora-logo.png");

    write_installer_images(root, &logo, &mark);

    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&s| {
            (
                s,
                png_bytes(&image::imageops::resize(&mark, s, s, FilterType::Lanczos3)),
            )
        })
        .collect();

    let preview = root.join("target/icon-preview");
    std::fs::create_dir_all(&preview).expect("Vorschauordner");
    for (size, data) in &images {
        std::fs::write(preview.join(format!("remora-{size}.png")), data).expect("Vorschau schreiben");
    }

    // ICO-Container mit PNG-Einträgen (ab Windows Vista unterstützt)
    let mut ico = Vec::new();
    ico.write_all(&[0, 0, 1, 0]).unwrap();
    ico.write_all(&(images.len() as u16).to_le_bytes()).unwrap();
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        ico.write_all(&[dim, dim, 0, 0]).unwrap();
        ico.write_all(&1u16.to_le_bytes()).unwrap(); // Farbebenen
        ico.write_all(&32u16.to_le_bytes()).unwrap(); // Bit pro Pixel
        ico.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
        ico.write_all(&offset.to_le_bytes()).unwrap();
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        ico.write_all(data).unwrap();
    }
    let path = root.join("assets/remora.ico");
    std::fs::write(&path, &ico).expect("ICO schreiben");
    println!("{} ({} Bytes, Größen {:?})", path.display(), ico.len(), SIZES);
}
