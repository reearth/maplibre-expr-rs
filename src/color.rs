//! A minimal CSS color model and parser, sufficient for style expressions.

use std::fmt;

/// An RGBA color with channels stored as floats in the `0.0..=1.0` range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub fn new(r: f64, g: f64, b: f64, a: f64) -> Color {
        Color { r, g, b, a }
    }

    /// From 8-bit RGB channels plus a `0.0..=1.0` alpha.
    pub fn from_rgba8(r: f64, g: f64, b: f64, a: f64) -> Color {
        Color {
            r: r / 255.0,
            g: g / 255.0,
            b: b / 255.0,
            a,
        }
    }

    /// The premultiplied-alpha `[r, g, b, a]` representation used when a color
    /// value is serialized as a spec-fixture output. MapLibre stores colors
    /// premultiplied internally, so `["interpolate", ...]` results and other
    /// color outputs compare against `[r*a, g*a, b*a, a]`.
    pub fn to_rgba_unit(self) -> [f64; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }

    /// The `to-rgba` operator representation: straight (non-premultiplied)
    /// `[r, g, b, a]` with r/g/b in `0..=255` and alpha in `0.0..=1.0`.
    pub fn to_rgba255(self) -> [f64; 4] {
        [self.r * 255.0, self.g * 255.0, self.b * 255.0, self.a]
    }

    /// Convert to CIE L\*a\*b\* as `[l, a, b, alpha]` (from straight rgb).
    pub fn to_lab(self) -> [f64; 4] {
        rgb_to_lab([self.r, self.g, self.b, self.a])
    }

    /// Build a color from CIE L\*a\*b\* `[l, a, b, alpha]`.
    pub fn from_lab(lab: [f64; 4]) -> Color {
        let [r, g, b, a] = lab_to_rgb(lab);
        Color::new(r, g, b, a)
    }

    /// Convert to HCL as `[h, c, l, alpha]`; hue is `NaN` for achromatic colors.
    pub fn to_hcl(self) -> [f64; 4] {
        rgb_to_hcl([self.r, self.g, self.b, self.a])
    }

    /// Build a color from HCL `[h, c, l, alpha]`.
    pub fn from_hcl(hcl: [f64; 4]) -> Color {
        let [r, g, b, a] = hcl_to_rgb(hcl);
        Color::new(r, g, b, a)
    }

    /// Parse a CSS color string. Supports `#rgb`/`#rrggbb`/`#rrggbbaa`,
    /// `rgb()`/`rgba()`, `hsl()`/`hsla()`, and a table of named colors.
    pub fn parse(input: &str) -> Option<Color> {
        let s = input.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex);
        }
        if let Some(c) = parse_functional(s) {
            return Some(c);
        }
        named(s)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rgba({},{},{},{})",
            (self.r * 255.0).round() as u8,
            (self.g * 255.0).round() as u8,
            (self.b * 255.0).round() as u8,
            self.a
        )
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    let bytes = hex.as_bytes();
    let expand = |c: u8| {
        let v = (c as char).to_digit(16)? as f64;
        Some(v * 16.0 + v)
    };
    match hex.len() {
        3 => Some(Color::from_rgba8(
            expand(bytes[0])?,
            expand(bytes[1])?,
            expand(bytes[2])?,
            1.0,
        )),
        4 => Some(Color::from_rgba8(
            expand(bytes[0])?,
            expand(bytes[1])?,
            expand(bytes[2])?,
            expand(bytes[3])? / 255.0,
        )),
        6 => Some(Color::from_rgba8(
            hexpair(&hex[0..2])?,
            hexpair(&hex[2..4])?,
            hexpair(&hex[4..6])?,
            1.0,
        )),
        8 => Some(Color::from_rgba8(
            hexpair(&hex[0..2])?,
            hexpair(&hex[2..4])?,
            hexpair(&hex[4..6])?,
            hexpair(&hex[6..8])? / 255.0,
        )),
        _ => None,
    }
}

fn hexpair(s: &str) -> Option<f64> {
    u8::from_str_radix(s, 16).ok().map(|v| v as f64)
}

fn parse_functional(s: &str) -> Option<Color> {
    let open = s.find('(')?;
    let name = s[..open].trim().to_ascii_lowercase();
    let inner = s[open + 1..].strip_suffix(')')?;

    // Accept both legacy comma syntax (`rgb(0, 0, 255)`) and CSS Color 4
    // whitespace syntax with a `/`-separated alpha (`rgb(0 0 255 / 0.5)`).
    let (body, alpha_tok) = match inner.split_once('/') {
        Some((body, alpha)) => (body, Some(alpha.trim())),
        None => (inner, None),
    };
    let parts: Vec<&str> = body
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    let alpha_tok = alpha_tok.or_else(|| parts.get(3).copied());
    let a = match alpha_tok {
        Some(t) => alpha(t)?,
        None => 1.0,
    };

    match name.as_str() {
        "rgb" | "rgba" => Some(Color::from_rgba8(
            channel(parts[0])?,
            channel(parts[1])?,
            channel(parts[2])?,
            a,
        )),
        "hsl" | "hsla" => {
            let h = parts[0].trim_end_matches("deg").parse::<f64>().ok()?;
            let (r, g, b) = hsl_to_rgb(h, percent(parts[1])?, percent(parts[2])?);
            Some(Color::new(r, g, b, a))
        }
        _ => None,
    }
}

/// Parse an alpha token: a plain `0.0..=1.0` number or a percentage.
fn alpha(s: &str) -> Option<f64> {
    if let Some(p) = s.strip_suffix('%') {
        Some(p.trim().parse::<f64>().ok()? / 100.0)
    } else {
        s.parse::<f64>().ok()
    }
}

fn channel(s: &str) -> Option<f64> {
    if let Some(p) = s.strip_suffix('%') {
        Some(p.trim().parse::<f64>().ok()? / 100.0 * 255.0)
    } else {
        s.parse::<f64>().ok()
    }
}

fn percent(s: &str) -> Option<f64> {
    s.strip_suffix('%')?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v / 100.0)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    (
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    )
}

fn hue_to_rgb(p: f64, q: f64, t: f64) -> f64 {
    let t = if t < 0.0 {
        t + 1.0
    } else if t > 1.0 {
        t - 1.0
    } else {
        t
    };
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

// ---- CIE L*a*b* / HCL conversions ------------------------------------
//
// Ported from maplibre-style-spec's `color_spaces.ts` (D50 reference white),
// so that `interpolate-lab` / `interpolate-hcl` match the reference exactly.
// See https://observablehq.com/@mbostock/lab-and-rgb

const XN: f64 = 0.96422;
const YN: f64 = 1.0;
const ZN: f64 = 0.82521;
const T0: f64 = 4.0 / 29.0;
const T1: f64 = 6.0 / 29.0;
const T2: f64 = 3.0 * T1 * T1;
const T3: f64 = T1 * T1 * T1;

fn rgb_to_lab([r, g, b, alpha]: [f64; 4]) -> [f64; 4] {
    let r = rgb2xyz(r);
    let g = rgb2xyz(g);
    let b = rgb2xyz(b);
    let y = xyz2lab((0.2225045 * r + 0.7168786 * g + 0.0606169 * b) / YN);
    let (x, z) = if r == g && g == b {
        (y, y)
    } else {
        (
            xyz2lab((0.4360747 * r + 0.3850649 * g + 0.1430804 * b) / XN),
            xyz2lab((0.0139322 * r + 0.0971045 * g + 0.7141733 * b) / ZN),
        )
    };
    let l = 116.0 * y - 16.0;
    [
        if l < 0.0 { 0.0 } else { l },
        500.0 * (x - y),
        200.0 * (y - z),
        alpha,
    ]
}

fn lab_to_rgb([l, a, b, alpha]: [f64; 4]) -> [f64; 4] {
    let y = (l + 16.0) / 116.0;
    let x = if a.is_nan() { y } else { y + a / 500.0 };
    let z = if b.is_nan() { y } else { y - b / 200.0 };
    let y = YN * lab2xyz(y);
    let x = XN * lab2xyz(x);
    let z = ZN * lab2xyz(z);
    [
        xyz2rgb(3.1338561 * x - 1.6168667 * y - 0.4906146 * z),
        xyz2rgb(-0.9787684 * x + 1.9161415 * y + 0.033454 * z),
        xyz2rgb(0.0719453 * x - 0.2289914 * y + 1.4052427 * z),
        alpha,
    ]
}

fn rgb2xyz(x: f64) -> f64 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

fn xyz2lab(t: f64) -> f64 {
    if t > T3 {
        t.cbrt()
    } else {
        t / T2 + T0
    }
}

fn lab2xyz(t: f64) -> f64 {
    if t > T1 {
        t * t * t
    } else {
        T2 * (t - T0)
    }
}

fn xyz2rgb(x: f64) -> f64 {
    let x = if x <= 0.00304 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    x.clamp(0.0, 1.0)
}

fn constrain_angle(angle: f64) -> f64 {
    let a = angle % 360.0;
    if a < 0.0 {
        a + 360.0
    } else {
        a
    }
}

fn rgb_to_hcl(rgb: [f64; 4]) -> [f64; 4] {
    let [l, a, b, alpha] = rgb_to_lab(rgb);
    let c = (a * a + b * b).sqrt();
    let h = if (c * 10000.0).round() != 0.0 {
        constrain_angle(b.atan2(a).to_degrees())
    } else {
        f64::NAN
    };
    [h, c, l, alpha]
}

fn hcl_to_rgb([h, c, l, alpha]: [f64; 4]) -> [f64; 4] {
    let h = if h.is_nan() { 0.0 } else { h.to_radians() };
    lab_to_rgb([l, h.cos() * c, h.sin() * c, alpha])
}

/// A small subset of the CSS named colors that appear in the test fixtures.
fn named(s: &str) -> Option<Color> {
    let rgb = match s.to_ascii_lowercase().as_str() {
        "transparent" => return Some(Color::new(0.0, 0.0, 0.0, 0.0)),
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "lime" => (0, 255, 0),
        "blue" => (0, 0, 255),
        "yellow" => (255, 255, 0),
        "cyan" | "aqua" => (0, 255, 255),
        "magenta" | "fuchsia" => (255, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "maroon" => (128, 0, 0),
        "olive" => (128, 128, 0),
        "navy" => (0, 0, 128),
        "purple" => (128, 0, 128),
        "teal" => (0, 128, 128),
        "orange" => (255, 165, 0),
        _ => return None,
    };
    Some(Color::from_rgba8(
        rgb.0 as f64,
        rgb.1 as f64,
        rgb.2 as f64,
        1.0,
    ))
}
