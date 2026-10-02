//! A CSS Color 4 color model and parser, sufficient for style expressions.

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

    /// Parse a CSS color string, following the CSS Color 4 subset that
    /// maplibre-style-spec's `parse_css_color.ts` implements: the `transparent`
    /// keyword, all 148 CSS named colors, every hex notation (`#rgb`, `#rgba`,
    /// `#rrggbb`, `#rrggbbaa`), and the `rgb()`/`rgba()`/`hsl()`/`hsla()`
    /// functions in both the comma-separated legacy syntax and the
    /// space-separated modern syntax with an optional `/`-separated alpha.
    ///
    /// Parsing is case-insensitive and ignores surrounding whitespace.
    /// Channels and alpha are clamped to their valid ranges rather than
    /// rejected; mixing commas with spaces, or percentages with plain numbers,
    /// is rejected. Angles only accept an optional `deg` suffix, and the `none`
    /// keyword is not supported.
    pub fn parse(input: &str) -> Option<Color> {
        let lowered = input.to_lowercase();
        let s = lowered.trim();

        if s == "transparent" {
            return Some(Color::new(0.0, 0.0, 0.0, 0.0));
        }

        if let Some((r, g, b)) = named(s) {
            return Some(Color::from_rgba8(r as f64, g as f64, b as f64, 1.0));
        }

        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex);
        }

        if s.starts_with("rgb") {
            // A `rgb`-prefixed string can never match the hsl grammar, so a
            // failure here is a failure outright.
            return parse_rgb(s);
        }

        parse_hsl(s)
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

// ---- CSS color parsing -----------------------------------------------
//
// A port of maplibre-style-spec's `parse_css_color.ts`. The upstream grammar is
// expressed as two regular expressions; this is a hand-rolled equivalent, with
// the same accept/reject decisions and the same clamping.

/// `/^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/` plus upstream's
/// `parseInt(hex.padEnd(2, hex), 16) / 255` expansion.
fn parse_hex(hex: &str) -> Option<Color> {
    let bytes = hex.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // `hex` is all-ASCII from here on, so byte indices are char boundaries.
    let expand = |c: u8| {
        let v = (c as char).to_digit(16).unwrap() as f64;
        v * 16.0 + v
    };
    let pair = |s: &str| u8::from_str_radix(s, 16).unwrap() as f64;
    match bytes.len() {
        3 => Some(Color::from_rgba8(
            expand(bytes[0]),
            expand(bytes[1]),
            expand(bytes[2]),
            1.0,
        )),
        4 => Some(Color::from_rgba8(
            expand(bytes[0]),
            expand(bytes[1]),
            expand(bytes[2]),
            expand(bytes[3]) / 255.0,
        )),
        6 => Some(Color::from_rgba8(
            pair(&hex[0..2]),
            pair(&hex[2..4]),
            pair(&hex[4..6]),
            1.0,
        )),
        8 => Some(Color::from_rgba8(
            pair(&hex[0..2]),
            pair(&hex[2..4]),
            pair(&hex[4..6]),
            pair(&hex[6..8]) / 255.0,
        )),
        _ => None,
    }
}

/// A cursor over the (already lowercased and trimmed) input.
struct Scanner<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Scanner<'a> {
    fn new(s: &'a str) -> Scanner<'a> {
        Scanner { s, i: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, prefix: &str) -> bool {
        if self.s[self.i..].starts_with(prefix) {
            self.i += prefix.len();
            true
        } else {
            false
        }
    }

    /// `\s*`; reports whether anything was consumed.
    fn skip_ws(&mut self) -> bool {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i += c.len_utf8();
            } else {
                break;
            }
        }
        self.i > start
    }

    /// `([\de.+-]+)`, converted the way JavaScript's unary `+` would: a token
    /// the character class admits but that is not a number becomes `NaN`, which
    /// upstream's `validateNumbers` then rejects. The class excludes the
    /// letters of `inf`/`nan`, so those never reach the conversion.
    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, 'e' | '.' | '+' | '-') {
                self.i += c.len_utf8();
            } else {
                break;
            }
        }
        if self.i == start {
            return None;
        }
        Some(self.s[start..self.i].parse::<f64>().unwrap_or(f64::NAN))
    }

    /// `(?:\s+|\s*(,)\s*)`: either whitespace or a comma, reported as `' '` or
    /// `','` so the caller can check that the separators are consistent.
    fn separator(&mut self) -> Option<char> {
        let ws = self.skip_ws();
        if self.eat(',') {
            self.skip_ws();
            Some(',')
        } else if ws {
            Some(' ')
        } else {
            None
        }
    }

    /// `(?:\s*([,\/])\s*([\de.+-]+)(%)?)?\s*\)$`: the optional alpha argument
    /// followed by the closing paren and end of input. Returns the separator
    /// that introduced the alpha (or `None`) and the clamped alpha value.
    fn alpha_tail(&mut self) -> Option<(Option<char>, Option<f64>)> {
        self.skip_ws();
        let mut sep = None;
        let mut alpha = None;
        if matches!(self.peek(), Some(',') | Some('/')) {
            sep = self.peek();
            self.i += 1;
            self.skip_ws();
            let a = self.number()?;
            let as_percentage = self.eat('%');
            alpha = Some(clamp(if as_percentage { a / 100.0 } else { a }, 0.0, 1.0));
            self.skip_ws();
        }
        if !self.eat(')') || self.i != self.s.len() {
            return None;
        }
        Some((sep, alpha))
    }
}

/// Upstream's `argFormat` check: `[f1 || ' ', f2 || ' ', f3].join('')` must be
/// one of `'  '`, `'  /'`, `',,'` or `',,,'` — commas and spaces never mix.
fn arg_format_ok(f1: char, f2: char, f3: Option<char>) -> bool {
    matches!(
        (f1, f2, f3),
        (' ', ' ', None) | (' ', ' ', Some('/')) | (',', ',', None) | (',', ',', Some(','))
    )
}

fn clamp(n: f64, min: f64, max: f64) -> f64 {
    // `Math.min(Math.max(min, n), max)`. JavaScript's `Math.min`/`Math.max`
    // propagate NaN while Rust's `f64::min`/`f64::max` discard it, so NaN is
    // kept explicitly here; `validateNumbers` upstream relies on it surviving.
    if n.is_nan() {
        f64::NAN
    } else {
        n.clamp(min, max)
    }
}

/// `/^rgba?\(\s*([\de.+-]+)(%)?(?:\s+|\s*(,)\s*)([\de.+-]+)(%)?(?:\s+|\s*(,)\s*)([\de.+-]+)(%)?(?:\s*([,\/])\s*([\de.+-]+)(%)?)?\s*\)$/`
fn parse_rgb(s: &str) -> Option<Color> {
    let mut sc = Scanner::new(s);
    if !sc.eat_str("rgb") {
        return None;
    }
    sc.eat('a');
    if !sc.eat('(') {
        return None;
    }
    sc.skip_ws();

    let r = sc.number()?;
    let rp = sc.eat('%');
    let f1 = sc.separator()?;
    let g = sc.number()?;
    let gp = sc.eat('%');
    let f2 = sc.separator()?;
    let b = sc.number()?;
    let bp = sc.eat('%');
    let (f3, alpha) = sc.alpha_tail()?;

    if !arg_format_ok(f1, f2, f3) {
        return None;
    }
    // `valFormat`: all three percentages or none; a mix is rejected.
    let max_value = match (rp, gp, bp) {
        (true, true, true) => 100.0,
        (false, false, false) => 255.0,
        _ => return None,
    };
    let rgba = [
        clamp(r / max_value, 0.0, 1.0),
        clamp(g / max_value, 0.0, 1.0),
        clamp(b / max_value, 0.0, 1.0),
        alpha.unwrap_or(1.0),
    ];
    if rgba.iter().any(|v| v.is_nan()) {
        return None;
    }
    Some(Color::new(rgba[0], rgba[1], rgba[2], rgba[3]))
}

/// `/^hsla?\(\s*([\de.+-]+)(?:deg)?(?:\s+|\s*(,)\s*)([\de.+-]+)%(?:\s+|\s*(,)\s*)([\de.+-]+)%(?:\s*([,\/])\s*([\de.+-]+)(%)?)?\s*\)$/`
fn parse_hsl(s: &str) -> Option<Color> {
    let mut sc = Scanner::new(s);
    if !sc.eat_str("hsl") {
        return None;
    }
    sc.eat('a');
    if !sc.eat('(') {
        return None;
    }
    sc.skip_ws();

    let h = sc.number()?;
    sc.eat_str("deg");
    let f1 = sc.separator()?;
    let s_val = sc.number()?;
    if !sc.eat('%') {
        return None;
    }
    let f2 = sc.separator()?;
    let l_val = sc.number()?;
    if !sc.eat('%') {
        return None;
    }
    let (f3, alpha) = sc.alpha_tail()?;

    if !arg_format_ok(f1, f2, f3) {
        return None;
    }
    let hsla = [
        h,
        clamp(s_val, 0.0, 100.0),
        clamp(l_val, 0.0, 100.0),
        alpha.unwrap_or(1.0),
    ];
    if hsla.iter().any(|v| v.is_nan()) {
        return None;
    }
    let [r, g, b, a] = hsl_to_rgb(hsla);
    Some(Color::new(r, g, b, a))
}

/// <https://drafts.csswg.org/css-color-4/#hsl-to-rgb>, as in `color_spaces.ts`.
/// Hue is in degrees, saturation and lightness in `0..=100`.
fn hsl_to_rgb([h, s, l, alpha]: [f64; 4]) -> [f64; 4] {
    let h = constrain_angle(h);
    let s = s / 100.0;
    let l = l / 100.0;
    let f = |n: f64| {
        let k = (n + h / 30.0) % 12.0;
        let a = s * l.min(1.0 - l);
        // `Math.max(-1, Math.min(k - 3, 9 - k, 1))`; `k` is finite here because
        // a NaN hue is rejected before this point.
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    [f(0.0), f(8.0), f(4.0), alpha]
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
        libm::pow((x + 0.055) / 1.055, 2.4)
    }
}

fn xyz2lab(t: f64) -> f64 {
    if t > T3 {
        libm::cbrt(t)
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
        1.055 * libm::pow(x, 1.0 / 2.4) - 0.055
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
        constrain_angle(libm::atan2(b, a).to_degrees())
    } else {
        f64::NAN
    };
    [h, c, l, alpha]
}

fn hcl_to_rgb([h, c, l, alpha]: [f64; 4]) -> [f64; 4] {
    let h = if h.is_nan() { 0.0 } else { h.to_radians() };
    let (sin, cos) = libm::sincos(h);
    lab_to_rgb([l, cos * c, sin * c, alpha])
}

/// The CSS Color 4 named colors, copied verbatim from the `namedColors` table
/// in maplibre-style-spec's `parse_css_color.ts`. `transparent` is handled by
/// the caller, as it is upstream.
fn named(s: &str) -> Option<(u8, u8, u8)> {
    let rgb = match s {
        "aliceblue" => (240, 248, 255),
        "antiquewhite" => (250, 235, 215),
        "aqua" => (0, 255, 255),
        "aquamarine" => (127, 255, 212),
        "azure" => (240, 255, 255),
        "beige" => (245, 245, 220),
        "bisque" => (255, 228, 196),
        "black" => (0, 0, 0),
        "blanchedalmond" => (255, 235, 205),
        "blue" => (0, 0, 255),
        "blueviolet" => (138, 43, 226),
        "brown" => (165, 42, 42),
        "burlywood" => (222, 184, 135),
        "cadetblue" => (95, 158, 160),
        "chartreuse" => (127, 255, 0),
        "chocolate" => (210, 105, 30),
        "coral" => (255, 127, 80),
        "cornflowerblue" => (100, 149, 237),
        "cornsilk" => (255, 248, 220),
        "crimson" => (220, 20, 60),
        "cyan" => (0, 255, 255),
        "darkblue" => (0, 0, 139),
        "darkcyan" => (0, 139, 139),
        "darkgoldenrod" => (184, 134, 11),
        "darkgray" => (169, 169, 169),
        "darkgreen" => (0, 100, 0),
        "darkgrey" => (169, 169, 169),
        "darkkhaki" => (189, 183, 107),
        "darkmagenta" => (139, 0, 139),
        "darkolivegreen" => (85, 107, 47),
        "darkorange" => (255, 140, 0),
        "darkorchid" => (153, 50, 204),
        "darkred" => (139, 0, 0),
        "darksalmon" => (233, 150, 122),
        "darkseagreen" => (143, 188, 143),
        "darkslateblue" => (72, 61, 139),
        "darkslategray" => (47, 79, 79),
        "darkslategrey" => (47, 79, 79),
        "darkturquoise" => (0, 206, 209),
        "darkviolet" => (148, 0, 211),
        "deeppink" => (255, 20, 147),
        "deepskyblue" => (0, 191, 255),
        "dimgray" => (105, 105, 105),
        "dimgrey" => (105, 105, 105),
        "dodgerblue" => (30, 144, 255),
        "firebrick" => (178, 34, 34),
        "floralwhite" => (255, 250, 240),
        "forestgreen" => (34, 139, 34),
        "fuchsia" => (255, 0, 255),
        "gainsboro" => (220, 220, 220),
        "ghostwhite" => (248, 248, 255),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "gray" => (128, 128, 128),
        "green" => (0, 128, 0),
        "greenyellow" => (173, 255, 47),
        "grey" => (128, 128, 128),
        "honeydew" => (240, 255, 240),
        "hotpink" => (255, 105, 180),
        "indianred" => (205, 92, 92),
        "indigo" => (75, 0, 130),
        "ivory" => (255, 255, 240),
        "khaki" => (240, 230, 140),
        "lavender" => (230, 230, 250),
        "lavenderblush" => (255, 240, 245),
        "lawngreen" => (124, 252, 0),
        "lemonchiffon" => (255, 250, 205),
        "lightblue" => (173, 216, 230),
        "lightcoral" => (240, 128, 128),
        "lightcyan" => (224, 255, 255),
        "lightgoldenrodyellow" => (250, 250, 210),
        "lightgray" => (211, 211, 211),
        "lightgreen" => (144, 238, 144),
        "lightgrey" => (211, 211, 211),
        "lightpink" => (255, 182, 193),
        "lightsalmon" => (255, 160, 122),
        "lightseagreen" => (32, 178, 170),
        "lightskyblue" => (135, 206, 250),
        "lightslategray" => (119, 136, 153),
        "lightslategrey" => (119, 136, 153),
        "lightsteelblue" => (176, 196, 222),
        "lightyellow" => (255, 255, 224),
        "lime" => (0, 255, 0),
        "limegreen" => (50, 205, 50),
        "linen" => (250, 240, 230),
        "magenta" => (255, 0, 255),
        "maroon" => (128, 0, 0),
        "mediumaquamarine" => (102, 205, 170),
        "mediumblue" => (0, 0, 205),
        "mediumorchid" => (186, 85, 211),
        "mediumpurple" => (147, 112, 219),
        "mediumseagreen" => (60, 179, 113),
        "mediumslateblue" => (123, 104, 238),
        "mediumspringgreen" => (0, 250, 154),
        "mediumturquoise" => (72, 209, 204),
        "mediumvioletred" => (199, 21, 133),
        "midnightblue" => (25, 25, 112),
        "mintcream" => (245, 255, 250),
        "mistyrose" => (255, 228, 225),
        "moccasin" => (255, 228, 181),
        "navajowhite" => (255, 222, 173),
        "navy" => (0, 0, 128),
        "oldlace" => (253, 245, 230),
        "olive" => (128, 128, 0),
        "olivedrab" => (107, 142, 35),
        "orange" => (255, 165, 0),
        "orangered" => (255, 69, 0),
        "orchid" => (218, 112, 214),
        "palegoldenrod" => (238, 232, 170),
        "palegreen" => (152, 251, 152),
        "paleturquoise" => (175, 238, 238),
        "palevioletred" => (219, 112, 147),
        "papayawhip" => (255, 239, 213),
        "peachpuff" => (255, 218, 185),
        "peru" => (205, 133, 63),
        "pink" => (255, 192, 203),
        "plum" => (221, 160, 221),
        "powderblue" => (176, 224, 230),
        "purple" => (128, 0, 128),
        "rebeccapurple" => (102, 51, 153),
        "red" => (255, 0, 0),
        "rosybrown" => (188, 143, 143),
        "royalblue" => (65, 105, 225),
        "saddlebrown" => (139, 69, 19),
        "salmon" => (250, 128, 114),
        "sandybrown" => (244, 164, 96),
        "seagreen" => (46, 139, 87),
        "seashell" => (255, 245, 238),
        "sienna" => (160, 82, 45),
        "silver" => (192, 192, 192),
        "skyblue" => (135, 206, 235),
        "slateblue" => (106, 90, 205),
        "slategray" => (112, 128, 144),
        "slategrey" => (112, 128, 144),
        "snow" => (255, 250, 250),
        "springgreen" => (0, 255, 127),
        "steelblue" => (70, 130, 180),
        "tan" => (210, 180, 140),
        "teal" => (0, 128, 128),
        "thistle" => (216, 191, 216),
        "tomato" => (255, 99, 71),
        "turquoise" => (64, 224, 208),
        "violet" => (238, 130, 238),
        "wheat" => (245, 222, 179),
        "white" => (255, 255, 255),
        "whitesmoke" => (245, 245, 245),
        "yellow" => (255, 255, 0),
        "yellowgreen" => (154, 205, 50),
        _ => return None,
    };
    Some(rgb)
}

#[cfg(test)]
mod tests {
    use super::Color;

    fn rgba(s: &str) -> Option<[f64; 4]> {
        Color::parse(s).map(|c| [c.r, c.g, c.b, c.a])
    }

    fn close(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() < 1e-12)
    }

    #[test]
    fn multibyte_input_does_not_panic() {
        // `hex.len()` is a byte length; slicing it at 2/4/6 used to split a
        // multi-byte char. Upstream's hex regexp simply rejects these.
        for s in [
            "#€abc",
            "#€abcde",
            "#€",
            "#ab€",
            "#abcde€",
            "#日本語",
            "rgb(日)",
            "€",
        ] {
            assert_eq!(Color::parse(s), None, "{s}");
        }
    }

    #[test]
    fn hex_forms() {
        assert_eq!(rgba("#f0c"), Some([1.0, 0.0, 204.0 / 255.0, 1.0]));
        assert_eq!(rgba("#f0cf"), Some([1.0, 0.0, 204.0 / 255.0, 1.0]));
        assert_eq!(rgba("#ff00cc"), Some([1.0, 0.0, 204.0 / 255.0, 1.0]));
        assert_eq!(rgba("#ff00ccff"), Some([1.0, 0.0, 204.0 / 255.0, 1.0]));
        assert_eq!(
            rgba("#ff00cc80"),
            Some([1.0, 0.0, 204.0 / 255.0, 128.0 / 255.0])
        );
        assert_eq!(rgba("  #FF00CC  "), Some([1.0, 0.0, 204.0 / 255.0, 1.0]));
        for bad in [
            "#",
            "#f",
            "#ff",
            "#fffff",
            "#fffffff",
            "#fffffffff",
            "#gggggg",
        ] {
            assert_eq!(Color::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn keywords_and_named_colors() {
        assert_eq!(rgba("transparent"), Some([0.0, 0.0, 0.0, 0.0]));
        assert_eq!(rgba("  TRANSPARENT "), Some([0.0, 0.0, 0.0, 0.0]));
        assert_eq!(rgba("red"), Some([1.0, 0.0, 0.0, 1.0]));
        let smoke = 245.0 / 255.0;
        assert_eq!(rgba("WhiteSmoke"), Some([smoke, smoke, smoke, 1.0]));
        // Names the 18-entry table used to miss.
        assert_eq!(rgba("pink"), Some([1.0, 192.0 / 255.0, 203.0 / 255.0, 1.0]));
        assert_eq!(rgba("gold"), Some([1.0, 215.0 / 255.0, 0.0, 1.0]));
        assert_eq!(
            rgba("darkgray"),
            Some([169.0 / 255.0, 169.0 / 255.0, 169.0 / 255.0, 1.0])
        );
        assert_eq!(rgba("darkgrey"), rgba("darkgray"));
        assert_eq!(
            rgba("rebeccapurple"),
            Some([102.0 / 255.0, 51.0 / 255.0, 153.0 / 255.0, 1.0])
        );
        assert_eq!(Color::parse("notacolor"), None);
    }

    #[test]
    fn rgb_channels_are_clamped() {
        assert_eq!(rgba("rgb(300, 0, 0)"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(rgba("rgb(-20, 0, 0)"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(rgba("rgb(150%, 0%, 0%)"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(rgba("rgba(0,0,0,5)"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(rgba("rgba(0,0,0,-1)"), Some([0.0, 0.0, 0.0, 0.0]));
        assert_eq!(rgba("rgba(0,0,0,500%)"), Some([0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn rgb_rejects_mixed_and_malformed_arguments() {
        for bad in [
            "rgb(50%, 0, 0)",       // % mixed with numbers
            "rgb(0%, 0, 0%)",       // ditto
            "rgb(0, 0 255)",        // comma mixed with space
            "rgb(0 0, 255)",        // ditto
            "rgb(0 0 255, 0.5)",    // space args then a comma alpha
            "rgb(0, 0, 255 / 0.5)", // comma args then a slash alpha
            "rgb(inf, 0, 0)",
            "rgb(NaN, 0, 0)",
            // Tokens the `[\de.+-]+` class admits but that are not numbers;
            // they must stay NaN through the clamp and be rejected.
            "rgb(1-1, 0, 0)",
            "rgb(1e, 0, 0)",
            "rgb(., 0, 0)",
            "rgb(+, 0, 0)",
            "rgb(0,0,255,0.5,9)", // extra argument
            "rgb(0,0)",
            "rgb(0,0,255",
            "rgb(0,0,255))",
            "rgb 0 0 255",
        ] {
            assert_eq!(Color::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn rgb_accepted_forms() {
        assert_eq!(rgba("rgb(0, 0, 255)"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(rgba("RGB(0,0,255)"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(rgba("rgb(0 0 255)"), Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(rgba("rgba(0, 0, 255, 0.5)"), Some([0.0, 0.0, 1.0, 0.5]));
        assert_eq!(rgba("rgb(0 0 255 / 0.5)"), Some([0.0, 0.0, 1.0, 0.5]));
        assert_eq!(rgba("rgb(0% 0% 100% /.6)"), Some([0.0, 0.0, 1.0, 0.6]));
        assert_eq!(rgba("rgb(255 0 255 / 60%)"), Some([1.0, 0.0, 1.0, 0.6]));
        assert_eq!(rgba("rgb(50%, 0%, 0%)"), Some([0.5, 0.0, 0.0, 1.0]));
        assert_eq!(rgba("rgb(1e2, 0, 0)"), Some([100.0 / 255.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn hsl_forms_and_clamping() {
        assert!(close(
            rgba("hsl(120, 50%, 50%)").unwrap(),
            [0.25, 0.75, 0.25, 1.0]
        ));
        assert!(close(
            rgba("hsl(120deg 50% 50%)").unwrap(),
            [0.25, 0.75, 0.25, 1.0]
        ));
        assert!(close(
            rgba("hsla(120,50%,50%,0.5)").unwrap(),
            [0.25, 0.75, 0.25, 0.5]
        ));
        assert!(close(
            rgba("hsl(12e1 50% 50% / 90%)").unwrap(),
            [0.25, 0.75, 0.25, 0.9]
        ));
        // s clamped to 100, so this is pure green rather than out-of-range rgb.
        assert!(close(
            rgba("hsl(120, 150%, 50%)").unwrap(),
            [0.0, 1.0, 0.0, 1.0]
        ));
        assert!(close(
            rgba("hsl(120, -50%, 50%)").unwrap(),
            [0.5, 0.5, 0.5, 1.0]
        ));
        assert!(close(
            rgba("hsl(120, 50%, 150%)").unwrap(),
            [1.0, 1.0, 1.0, 1.0]
        ));
        for bad in [
            "hsl(120, 50, 50%)", // percentages are required
            "hsl(120, 50%, 50)",
            "hsl(120 50%, 50%)", // mixed separators
            "hsl(120, 50% 50%)",
            "hsl(120rad, 50%, 50%)", // only `deg` is supported
            "hsl(NaN, 50%, 50%)",
            "hsl(120, 50%, 50%, 0.5, 9)",
        ] {
            assert_eq!(Color::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn parsed_channels_stay_in_range() {
        for s in [
            "rgb(300, -20, 999)",
            "rgba(0,0,0,5)",
            "hsl(120, 150%, 50%)",
            "rgb(1e3% 0% 0%)",
        ] {
            let c = Color::parse(s).unwrap();
            for v in c.to_rgba_unit() {
                assert!((0.0..=1.0).contains(&v), "{s} -> {v}");
            }
        }
    }

    #[test]
    fn lab_roundtrip_is_unchanged() {
        assert_eq!(
            Color::new(1.0, 1.0, 1.0, 1.0).to_lab(),
            [100.0, 0.0, 0.0, 1.0]
        );
        for c in [
            Color::new(0.2, 0.4, 0.6, 1.0),
            Color::new(1.0, 0.0, 0.0, 0.5),
            Color::new(0.0, 0.0, 0.0, 1.0),
        ] {
            let back = Color::from_lab(c.to_lab());
            assert!((back.r - c.r).abs() < 1e-6 && (back.g - c.g).abs() < 1e-6);
            let back = Color::from_hcl(c.to_hcl());
            assert!((back.b - c.b).abs() < 1e-6);
        }
    }
}
