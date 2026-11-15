//! The Wi-Fi-style icons, drawn in code so the tray can color them by state.
use crate::monitor::Status;

type Rgb = [u8; 3];

const GOOD: Rgb = [0x4c, 0xd9, 0x8a];
const WARN: Rgb = [0xff, 0xb4, 0x54];
const BAD: Rgb = [0xff, 0x5c, 0x6c];
const GREY: Rgb = [0x9a, 0x93, 0xad];
const VIOLET: Rgb = [0x7c, 0x4d, 0xff];
const WHITE: Rgb = [0xff, 0xff, 0xff];

/// A shape in a 0..1 square: returns whether (x, y) is inside.
type Shape = Box<dyn Fn(f32, f32) -> bool>;

/// The three arcs of a Wi-Fi symbol around (cx, cy), opening upward.
fn arcs(cx: f32, cy: f32, scale: f32) -> Shape {
    Box::new(move |x, y| {
        let (dx, dy) = ((x - cx) / scale, (y - cy) / scale);
        let r = (dx * dx + dy * dy).sqrt();
        // Within 45° either side of straight up.
        let up = -dy >= dx.abs();
        up && [0.27f32, 0.5, 0.73].iter().any(|ring| (r - ring).abs() < 0.075)
    })
}

fn dot(cx: f32, cy: f32, r: f32) -> Shape {
    Box::new(move |x, y| (x - cx).powi(2) + (y - cy).powi(2) <= r * r)
}

/// A thick line from top left to bottom right.
fn slash(width: f32) -> Shape {
    Box::new(move |x, y| (x - y).abs() / std::f32::consts::SQRT_2 < width / 2.0 && x > 0.1 && x < 0.9)
}

fn rounded_square(r: f32) -> Shape {
    Box::new(move |x, y| {
        let cx = x.clamp(r, 1.0 - r);
        let cy = y.clamp(r, 1.0 - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    })
}

/// Paint `layers` (later ones on top, each with its color and opacity) at
/// `size`x`size`, 4x4 supersampled for smooth edges.
fn render(size: u32, layers: &[(Shape, Rgb, f32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) / size as f32;
                    // Composite this sample's layers back to front.
                    let mut c = [0f32; 4];
                    for (shape, rgb, alpha) in layers {
                        if shape(x, y) {
                            for i in 0..3 {
                                c[i] = rgb[i] as f32 * alpha + c[i] * (1.0 - alpha);
                            }
                            c[3] = alpha + c[3] * (1.0 - alpha);
                        }
                    }
                    // Premultiplied sum.
                    for i in 0..4 {
                        acc[i] += c[i];
                    }
                }
            }
            let a = acc[3] / 16.0;
            let un = |v: f32| if a > 0.0 { (v / 16.0 / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
            out.extend_from_slice(&[un(acc[0]), un(acc[1]), un(acc[2]), (a * 255.0).round() as u8]);
        }
    }
    out
}

/// The tray icon for `status`, RGBA.
pub fn status_rgba(size: u32, status: Status) -> Vec<u8> {
    let (cx, cy, scale) = (0.5, 0.86, 1.05);
    match status {
        Status::Down => render(size, &[(arcs(cx, cy, scale), BAD, 0.45), (dot(cx, cy - 0.02, 0.09), BAD, 0.45), (slash(0.13), BAD, 1.0)]),
        s => {
            let c = match s {
                Status::Up => GOOD,
                Status::Shaky => WARN,
                _ => GREY,
            };
            render(size, &[(arcs(cx, cy, scale), c, 1.0), (dot(cx, cy - 0.02, 0.09), c, 1.0)])
        }
    }
}

/// The app's own icon: white arcs and a green dot on violet, RGBA.
pub fn app_rgba(size: u32) -> Vec<u8> {
    render(size, &[(rounded_square(0.22), VIOLET, 1.0), (arcs(0.5, 0.74, 0.62), WHITE, 1.0), (dot(0.5, 0.72, 0.07), GOOD, 1.0)])
}

/// `net-watch --write-icon icon.png`: save the app icon as a PNG, for
/// packaging. Uncompressed (stored) deflate keeps this dependency-free.
pub fn write_png(path: &str, size: u32) -> std::io::Result<()> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let c = crc(&out[start..]);
        out.extend_from_slice(&c.to_be_bytes());
    }
    let rgba = app_rgba(size);
    let mut raw = Vec::new();
    for row in rgba.chunks_exact(size as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&size.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png)
}
