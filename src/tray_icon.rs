//! Tray icon: the project's `lock.svg` (embedded at build time) rendered to
//! pixmaps — a solid lock when a tunnel is up, a dimmed one when not. Rendering
//! it ourselves (rather than relying on a theme icon name) guarantees the *same*
//! glyph on every desktop (KDE, GNOME/AppIndicator, Waybar), independent of the
//! installed icon theme.
//!
//! The SVG is a flat fill path; we parse the single `<path d="…">` (incl. arcs)
//! and fill it with Cairo. The glyph is already white; we only recolour it to a
//! dimmer shade for the disconnected state (StatusNotifierItem pixmaps aren't
//! recoloured by the panel). Set `VPNCBAR_TRAY_FG=dark` for a dark glyph on a
//! light panel.

use gtk::cairo::{Context, FillRule, Format, ImageSurface, LineCap, Operator};
use gtk::prelude::Cast;

const LOCK_SVG: &str = include_str!("../packaging/lock.svg");

/// Lightning-bolt outline (single fill path) for the VPN Manager connect button.
const BOLT_PATH: &str = "M13 2 L3 14 L12 14 L11 22 L21 10 L12 10 Z";

/// Sizes we render so the host can pick the best fit for its panel / scale.
/// Includes high-res entries so a HiDPI/scaled panel downscales a crisp source
/// instead of upscaling a tiny one (which looked blurry).
const SIZES: [i32; 6] = [24, 32, 48, 64, 128, 256];

/// The lock icon set (one entry per size) for the given connection state. There
/// is a single lock glyph (already white), so we distinguish state by brightness:
/// a solid lock when connected, a dimmed one when not. Both sets are rendered once
/// and cached (the tray refresh re-reads them every couple of seconds).
pub fn padlock_set(connected: bool) -> Vec<ksni::Icon> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<[Vec<ksni::Icon>; 2]> = OnceLock::new();
    let sets = CACHE.get_or_init(|| {
        let (r, g, b) = foreground();
        let dim = (r * 0.5, g * 0.5, b * 0.5);
        let render_set = |fg| SIZES.iter().filter_map(|&s| icon(LOCK_SVG, fg, s)).collect();
        [render_set(dim), render_set((r, g, b))] // [disconnected, connected]
    });
    sets[connected as usize].clone()
}

/// The closed-lock (application) icon as a texture, for in-app use (About logo,
/// window icon) so it shows even when running uninstalled. Drawn in `fg`.
pub fn closed_lock_texture(size: i32, fg: (f64, f64, f64)) -> Option<gtk::gdk::Texture> {
    let mut surface = render(LOCK_SVG, fg, size)?;
    let stride = surface.stride();
    let data = surface.data().ok()?;
    let bytes = gtk::glib::Bytes::from(&data[..]);
    Some(
        gtk::gdk::MemoryTexture::new(
            size,
            size,
            gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            stride as usize,
        )
        .upcast(),
    )
}

/// A lightning-bolt texture for the VPN Manager connect button, drawn in `fg`.
/// `slash` adds a diagonal "disabled" stroke (top-left → bottom-right) with a
/// transparent keyline cut through the bolt so it reads as crossed-out — the
/// connected state, where the button disconnects. Self-rendered (like the tray
/// glyph) so it needs no installed theme icon and works uninstalled.
pub fn bolt_texture(size: i32, fg: (f64, f64, f64), slash: bool) -> Option<gtk::gdk::Texture> {
    let toks = tokenize(BOLT_PATH);
    let mut surface = ImageSurface::create(Format::ARgb32, size, size).ok()?;
    {
        let cr = Context::new(&surface).ok()?;
        cr.set_antialias(gtk::cairo::Antialias::Best);
        cr.set_fill_rule(FillRule::EvenOdd);

        // Fit the bolt into the icon (with margin so the slash can extend past it).
        run_path(&cr, &toks);
        let (x1, y1, x2, y2) = cr.fill_extents().unwrap_or((0.0, 0.0, 1.0, 1.0));
        cr.new_path();
        let (bw, bh) = (x2 - x1, y2 - y1);
        if bw > 0.0 && bh > 0.0 {
            let margin = size as f64 * 0.06;
            let avail = size as f64 - 2.0 * margin;
            let scale = (avail / bw).min(avail / bh);
            let tx = margin + (avail - bw * scale) / 2.0 - x1 * scale;
            let ty = margin + (avail - bh * scale) / 2.0 - y1 * scale;
            cr.translate(tx, ty);
            cr.scale(scale, scale);
            cr.set_source_rgba(fg.0, fg.1, fg.2, 1.0);
            run_path(&cr, &toks);
            let _ = cr.fill();
            cr.identity_matrix(); // back to device coords for the slash
        }

        if slash {
            let m = size as f64;
            cr.set_line_cap(LineCap::Round);
            // Clear a thin keyline first so the slash reads as separate from the
            // bolt without erasing it.
            cr.set_operator(Operator::Clear);
            cr.set_line_width(m * 0.14);
            cr.move_to(m * 0.12, m * 0.12);
            cr.line_to(m * 0.88, m * 0.88);
            let _ = cr.stroke();
            // The visible diagonal slash (thin).
            cr.set_operator(Operator::Over);
            cr.set_source_rgba(fg.0, fg.1, fg.2, 1.0);
            cr.set_line_width(m * 0.07);
            cr.move_to(m * 0.12, m * 0.12);
            cr.line_to(m * 0.88, m * 0.88);
            let _ = cr.stroke();
        }
    }
    surface.flush();
    let stride = surface.stride();
    let data = surface.data().ok()?;
    let bytes = gtk::glib::Bytes::from(&data[..]);
    Some(
        gtk::gdk::MemoryTexture::new(
            size,
            size,
            gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &bytes,
            stride as usize,
        )
        .upcast(),
    )
}

/// Foreground RGB, from `VPNCBAR_TRAY_FG` (light by default).
fn foreground() -> (f64, f64, f64) {
    match std::env::var("VPNCBAR_TRAY_FG").as_deref() {
        Ok("dark") | Ok("black") => (0.12, 0.12, 0.12),
        _ => (0.92, 0.92, 0.92),
    }
}

/// Render an SVG (its first `<path>`) into an `size`×`size` SNI ARGB32 icon.
fn icon(svg: &str, fg: (f64, f64, f64), size: i32) -> Option<ksni::Icon> {
    let mut surface = render(svg, fg, size)?;
    let stride = surface.stride() as usize;
    let data = surface.data().ok()?;
    let (w, h) = (size as usize, size as usize);
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
            // Cairo ARgb32 is native-endian premultiplied: bytes are B,G,R,A.
            let (b, g, r, a) = (data[i], data[i + 1], data[i + 2], data[i + 3]);
            // Un-premultiply, then store as SNI ARGB32 (network byte order).
            let un = |c: u8| if a == 0 { 0 } else { (c as u32 * 255 / a as u32) as u8 };
            let o = (y * w + x) * 4;
            out[o] = a;
            out[o + 1] = un(r);
            out[o + 2] = un(g);
            out[o + 3] = un(b);
        }
    }
    Some(ksni::Icon { width: size, height: size, data: out })
}

/// Draw the SVG's fill path into a fresh ARGB surface, recoloured to `fg`. The
/// glyph's *actual* bounding box (not the viewBox, which has internal padding) is
/// scaled to fill the icon, so the lock looks as large as possible.
fn render(svg: &str, fg: (f64, f64, f64), size: i32) -> Option<ImageSurface> {
    let toks = tokenize(extract_path(svg)?);
    let surface = ImageSurface::create(Format::ARgb32, size, size).ok()?;
    {
        let cr = Context::new(&surface).ok()?;
        cr.set_antialias(gtk::cairo::Antialias::Best);
        cr.set_fill_rule(FillRule::EvenOdd);

        // Measure the glyph's real extent, then clear and redraw it fitted.
        run_path(&cr, &toks);
        let (x1, y1, x2, y2) = cr.fill_extents().unwrap_or((0.0, 0.0, 1.0, 1.0));
        cr.new_path();
        let (bw, bh) = (x2 - x1, y2 - y1);
        if bw > 0.0 && bh > 0.0 {
            let margin = size as f64 * 0.02; // fill nearly the whole icon
            let avail = size as f64 - 2.0 * margin;
            let scale = (avail / bw).min(avail / bh);
            let tx = margin + (avail - bw * scale) / 2.0 - x1 * scale;
            let ty = margin + (avail - bh * scale) / 2.0 - y1 * scale;
            cr.translate(tx, ty);
            cr.scale(scale, scale);
            cr.set_source_rgba(fg.0, fg.1, fg.2, 1.0);
            run_path(&cr, &toks);
            let _ = cr.fill();
        }
    }
    surface.flush();
    Some(surface)
}


/// The `d` attribute of the first `<path>` in an SVG document.
fn extract_path(svg: &str) -> Option<&str> {
    let at = svg.find(" d=\"").or_else(|| svg.find(" d='"))?;
    let q = svg.as_bytes()[at + 3] as char; // the opening quote
    let start = at + 4;
    let end = svg[start..].find(q)? + start;
    Some(&svg[start..end])
}

enum Tok {
    Cmd(char),
    Num(f64),
}

/// Tokenise SVG path data into commands and numbers.
fn tokenize(d: &str) -> Vec<Tok> {
    let b = d.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c));
            i += 1;
        } else if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < b.len() {
                let cc = b[i] as char;
                if cc.is_ascii_digit() || cc == '.' {
                    i += 1;
                } else if cc == 'e' || cc == 'E' {
                    i += 1;
                    if i < b.len() && (b[i] as char == '-' || b[i] as char == '+') {
                        i += 1;
                    }
                } else {
                    break; // sign or separator → next number
                }
            }
            if let Ok(n) = d[start..i].parse::<f64>() {
                out.push(Tok::Num(n));
            }
        } else {
            i += 1; // whitespace / comma
        }
    }
    out
}

/// Execute path tokens into Cairo (absolute + relative M/L/H/V/C/Z).
fn run_path(cr: &Context, toks: &[Tok]) {
    let num = |k: &mut usize| -> f64 {
        match toks.get(*k) {
            Some(Tok::Num(v)) => {
                *k += 1;
                *v
            }
            _ => 0.0,
        }
    };
    let (mut cx, mut cy, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
    let mut cmd = 'M';
    let mut k = 0;
    while k < toks.len() {
        match toks[k] {
            Tok::Cmd(c) => {
                cmd = c;
                k += 1;
            }
            // A number after Z is invalid (Z takes no args); skip it rather than
            // spin. Any other number is an implicit repeat of the current command.
            Tok::Num(_) if cmd.eq_ignore_ascii_case(&'z') => {
                k += 1;
                continue;
            }
            Tok::Num(_) => {}
        }
        let abs = cmd.is_ascii_uppercase();
        match cmd.to_ascii_uppercase() {
            'M' => {
                let (mut x, mut y) = (num(&mut k), num(&mut k));
                if !abs {
                    x += cx;
                    y += cy;
                }
                cr.move_to(x, y);
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                cmd = if abs { 'L' } else { 'l' }; // extra pairs are line-tos
            }
            'L' => {
                let (mut x, mut y) = (num(&mut k), num(&mut k));
                if !abs {
                    x += cx;
                    y += cy;
                }
                cr.line_to(x, y);
                cx = x;
                cy = y;
            }
            'H' => {
                let mut x = num(&mut k);
                if !abs {
                    x += cx;
                }
                cr.line_to(x, cy);
                cx = x;
            }
            'V' => {
                let mut y = num(&mut k);
                if !abs {
                    y += cy;
                }
                cr.line_to(cx, y);
                cy = y;
            }
            'C' => {
                let (mut x1, mut y1) = (num(&mut k), num(&mut k));
                let (mut x2, mut y2) = (num(&mut k), num(&mut k));
                let (mut x, mut y) = (num(&mut k), num(&mut k));
                if !abs {
                    x1 += cx; y1 += cy; x2 += cx; y2 += cy; x += cx; y += cy;
                }
                cr.curve_to(x1, y1, x2, y2, x, y);
                cx = x;
                cy = y;
            }
            'A' => {
                let (rx, ry) = (num(&mut k), num(&mut k));
                let rot = num(&mut k);
                let large = num(&mut k) != 0.0;
                let sweep = num(&mut k) != 0.0;
                let (mut x, mut y) = (num(&mut k), num(&mut k));
                if !abs {
                    x += cx;
                    y += cy;
                }
                arc_to(cr, cx, cy, rx, ry, rot, large, sweep, x, y);
                cx = x;
                cy = y;
            }
            'Z' => {
                cr.close_path();
                cx = sx;
                cy = sy;
            }
            _ => {
                k += 1; // unknown command — skip it
            }
        }
    }
}

/// Approximate an SVG elliptical arc (endpoint parameterisation) from (x1,y1) to
/// (x2,y2) with cubic béziers (W3C implementation notes), emitting curve_to()s.
#[allow(clippy::too_many_arguments)]
fn arc_to(
    cr: &Context,
    x1: f64,
    y1: f64,
    mut rx: f64,
    mut ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    x2: f64,
    y2: f64,
) {
    if rx == 0.0 || ry == 0.0 {
        cr.line_to(x2, y2);
        return;
    }
    rx = rx.abs();
    ry = ry.abs();
    let phi = rot_deg.to_radians();
    let (cosp, sinp) = (phi.cos(), phi.sin());

    // Step 1: midpoint in the rotated frame.
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cosp * dx + sinp * dy;
    let y1p = -sinp * dx + cosp * dy;

    // Correct out-of-range radii.
    let lambda = x1p * x1p / (rx * rx) + y1p * y1p / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    // Step 2: centre in the rotated frame.
    let num = (rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p).max(0.0);
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den == 0.0 { 0.0 } else { (num / den).sqrt() };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;

    // Centre in user space.
    let cx = cosp * cxp - sinp * cyp + (x1 + x2) / 2.0;
    let cy = sinp * cxp + cosp * cyp + (y1 + y2) / 2.0;

    let angle = |ux: f64, uy: f64, vx: f64, vy: f64| -> f64 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut a = (dot / len).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let theta1 = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= 2.0 * std::f64::consts::PI;
    } else if sweep && dtheta < 0.0 {
        dtheta += 2.0 * std::f64::consts::PI;
    }

    // Split into ≤90° segments, each a cubic bézier.
    let segs = (dtheta.abs() / (std::f64::consts::PI / 2.0)).ceil().max(1.0) as i32;
    let delta = dtheta / segs as f64;
    let t = (delta / 2.0).tan();
    let alpha = delta.sin() * ((4.0 + 3.0 * t * t).sqrt() - 1.0) / 3.0;
    let point = |a: f64| -> (f64, f64) {
        let (ca, sa) = (a.cos(), a.sin());
        (cx + cosp * rx * ca - sinp * ry * sa, cy + sinp * rx * ca + cosp * ry * sa)
    };
    let deriv = |a: f64| -> (f64, f64) {
        let (ca, sa) = (a.cos(), a.sin());
        (-cosp * rx * sa - sinp * ry * ca, -sinp * rx * sa + cosp * ry * ca)
    };
    let mut th = theta1;
    for _ in 0..segs {
        let th2 = th + delta;
        let (p1x, p1y) = point(th);
        let (p2x, p2y) = point(th2);
        let (d1x, d1y) = deriv(th);
        let (d2x, d2y) = deriv(th2);
        cr.curve_to(
            p1x + alpha * d1x,
            p1y + alpha * d1y,
            p2x - alpha * d2x,
            p2y - alpha * d2y,
            p2x,
            p2y,
        );
        th = th2;
    }
}
