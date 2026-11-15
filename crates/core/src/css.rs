//! Shortest spelling of a declaration block: value minification and shorthand folding.
//!
//! Folding is only ever a re-spelling. A shorthand is used when it sets every longhand it
//! covers to that longhand's target value (components may be left out only when the target
//! equals the CSS initial value the shorthand would reset to) and is shorter than what it
//! replaces. The final document is read back from the browser, so a wrong fold is caught.

/// Shorten a computed value without changing its meaning.
pub fn minify_value(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = String::with_capacity(v.len());
    let mut i = 0;
    let mut depth = 0usize;
    while i < b.len() {
        let c = b[i];
        // Strings are copied untouched.
        if c == b'"' || c == b'\'' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&v[start..i]);
            continue;
        }
        if c.is_ascii_alphabetic() && (i == 0 || !is_ident(b[i - 1])) {
            let rest = &v[i..];
            if starts_ci(rest, "url(") {
                let end = close_paren(rest).unwrap_or(rest.len());
                out.push_str(&rest[..end]);
                i += end;
                continue;
            }
            if starts_ci(rest, "rgb(") || starts_ci(rest, "rgba(") {
                if let Some((len, hex)) = parse_rgb(rest) {
                    out.push_str(&hex);
                    i += len;
                    continue;
                }
            }
        }
        match c {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(',');
                i += 1;
                while i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                continue;
            }
            b'/' if depth == 0 => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push('/');
                i += 1;
                while i < b.len() && b[i] == b' ' {
                    i += 1;
                }
                continue;
            }
            b'0' => {
                let prev = if i == 0 { b' ' } else { b[i - 1] };
                let boundary = !(prev.is_ascii_alphanumeric() || prev == b'.' || prev == b'#' || prev == b'_' || prev == b'%');
                // 0.5 -> .5
                if boundary_num(prev) && i + 2 < b.len() && b[i + 1] == b'.' && b[i + 2].is_ascii_digit() {
                    i += 1;
                    continue;
                }
                // a bare zero length needs no unit
                if depth == 0 && boundary && v[i..].starts_with("0px") {
                    let next = b.get(i + 3).copied().unwrap_or(b' ');
                    if (prev == b' ' || prev == b',' || prev == b'/' || i == 0) && (next == b' ' || next == b',' || next == b'/') {
                        out.push('0');
                        i += 3;
                        continue;
                    }
                }
            }
            _ => {}
        }
        // Copy one character (UTF-8 aware).
        let ch_len = utf8_len(c);
        out.push_str(&v[i..(i + ch_len).min(v.len())]);
        i += ch_len;
    }
    out
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}

fn boundary_num(prev: u8) -> bool {
    !(prev.is_ascii_digit() || prev == b'.' || prev.is_ascii_alphabetic() || prev == b'#' || prev == b'_')
}

fn utf8_len(c: u8) -> usize {
    match c {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn starts_ci(s: &str, pre: &str) -> bool {
    s.len() >= pre.len() && s.as_bytes()[..pre.len()].eq_ignore_ascii_case(pre.as_bytes())
}

/// Byte length up to and including the parenthesis that closes the first `(`.
fn close_paren(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `rgb(255, 0, 0)` -> `#f00`, `rgba(0, 0, 0, 0.5)` -> `#00000080`. Returns the bytes consumed.
fn parse_rgb(s: &str) -> Option<(usize, String)> {
    let open = s.find('(')?;
    let end = close_paren(s)?;
    let args: Vec<&str> = s[open + 1..end - 1].split(',').map(str::trim).collect();
    if args.len() != 3 && args.len() != 4 {
        return None;
    }
    let mut ch = [0u8; 3];
    for (k, a) in args[..3].iter().enumerate() {
        if a.is_empty() || !a.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        ch[k] = a.parse::<u16>().ok().filter(|&x| x <= 255)? as u8;
    }
    let alpha: f64 = if args.len() == 4 { args[3].parse().ok()? } else { 1.0 };
    if !(0.0..=1.0).contains(&alpha) {
        return None;
    }
    let hex = |v: &[u8]| -> String {
        let short = v.iter().all(|x| x >> 4 == x & 15);
        let mut s = String::from("#");
        for x in v {
            if short {
                s.push_str(&format!("{:x}", x & 15));
            } else {
                s.push_str(&format!("{:02x}", x));
            }
        }
        s
    };
    if alpha == 1.0 {
        return Some((end, hex(&ch)));
    }
    // Legacy colours carry 8-bit alpha, serialised with the fewest decimals that round-trip.
    let byte = (alpha * 255.0).round();
    let canon = {
        let two = (byte / 255.0 * 100.0).round() / 100.0;
        if (two * 255.0).round() == byte {
            two
        } else {
            (byte / 255.0 * 1000.0).round() / 1000.0
        }
    };
    if (canon - alpha).abs() < 1e-9 {
        return Some((end, hex(&[ch[0], ch[1], ch[2], byte as u8])));
    }
    let a = args[3].strip_prefix('0').filter(|r| r.starts_with('.')).unwrap_or(args[3]);
    Some((end, format!("rgba({},{},{},{})", ch[0], ch[1], ch[2], a)))
}

/// `top right bottom left` (or the four corners) with the usual elision.
fn four(v: &[String; 4]) -> String {
    if v[0] == v[1] && v[1] == v[2] && v[2] == v[3] {
        v[0].clone()
    } else if v[0] == v[2] && v[1] == v[3] {
        format!("{} {}", v[0], v[1])
    } else if v[1] == v[3] {
        format!("{} {} {}", v[0], v[1], v[2])
    } else {
        format!("{} {} {} {}", v[0], v[1], v[2], v[3])
    }
}

type Decls = Vec<(String, String)>;

fn find(d: &Decls, name: &str) -> Option<usize> {
    d.iter().position(|(n, _)| n == name)
}

/// Replace the declared longhands among `covers` with `short: value` when that is shorter.
fn replace(d: &mut Decls, covers: &[&str], min_declared: usize, short: &str, value: String) -> bool {
    let idx: Vec<usize> = covers.iter().filter_map(|c| find(d, c)).collect();
    if idx.len() < min_declared.max(1) {
        return false;
    }
    let old: usize = idx.iter().map(|&i| d[i].0.len() + 1 + d[i].1.len()).sum::<usize>() + idx.len() - 1;
    let new = short.len() + 1 + value.len();
    if new >= old {
        return false;
    }
    let first = *idx.iter().min().unwrap();
    let mut sorted = idx.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    for i in sorted {
        if i != first {
            d.remove(i);
        }
    }
    d[first] = (short.to_string(), value);
    true
}

/// Fold longhands into shorthands. `target` gives the (minified) target value of any
/// longhand of the element, declared or not.
pub fn fold(d: &mut Decls, target: &dyn Fn(&str) -> Option<String>) {
    let get4 = |names: [&str; 4]| -> Option<[String; 4]> { Some([target(names[0])?, target(names[1])?, target(names[2])?, target(names[3])?]) };
    let tv = |name: &str| target(name).unwrap_or_default();

    // --- border ---------------------------------------------------------------------------
    let sides = ["top", "right", "bottom", "left"];
    let bw: Vec<String> = sides.iter().map(|s| format!("border-{s}-width")).collect();
    let bs: Vec<String> = sides.iter().map(|s| format!("border-{s}-style")).collect();
    let bc: Vec<String> = sides.iter().map(|s| format!("border-{s}-color")).collect();
    let border_part = |w: &str, s: &str, c: &str| -> String {
        let mut parts: Vec<&str> = Vec::new();
        if w != "3px" {
            parts.push(w);
        }
        if s != "none" {
            parts.push(s);
        }
        if c != "currentcolor" {
            parts.push(c);
        }
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(" ")
        }
    };
    let image_initial = tv("border-image-source") == "none"
        && matches!(tv("border-image-outset").as_str(), "0" | "0px")
        && tv("border-image-repeat") == "stretch"
        && tv("border-image-slice") == "100%"
        && tv("border-image-width") == "1";
    let all: Vec<&str> = bw.iter().chain(bs.iter()).chain(bc.iter()).map(String::as_str).collect();
    let uniform = |names: &[String]| -> Option<String> {
        let v: Vec<String> = names.iter().map(|n| tv(n)).collect();
        (!v[0].is_empty() && v.iter().all(|x| *x == v[0])).then(|| v[0].clone())
    };
    // Three ways to spell borders; keep whichever comes out shortest.
    let size = |d: &Decls| d.iter().map(|(n, v)| n.len() + v.len() + 2).sum::<usize>();
    let mut whole = d.clone();
    if image_initial {
        if let (Some(w), Some(s), Some(c)) = (uniform(&bw), uniform(&bs), uniform(&bc)) {
            replace(&mut whole, &all, 2, "border", border_part(&w, &s, &c));
        }
    }
    let by_group = |d: &mut Decls| {
        for (group, short) in [(&bw, "border-width"), (&bs, "border-style"), (&bc, "border-color")] {
            if let Some(v) = get4([&group[0], &group[1], &group[2], &group[3]]) {
                let names: Vec<&str> = group.iter().map(String::as_str).collect();
                replace(d, &names, 2, short, four(&v));
            }
        }
    };
    let mut groups = d.clone();
    by_group(&mut groups);
    let mut by_side = d.clone();
    for i in 0..4 {
        let (w, s, c) = (tv(&bw[i]), tv(&bs[i]), tv(&bc[i]));
        if w.is_empty() || s.is_empty() || c.is_empty() {
            continue;
        }
        replace(&mut by_side, &[&bw[i], &bs[i], &bc[i]], 2, &format!("border-{}", sides[i]), border_part(&w, &s, &c));
    }
    by_group(&mut by_side);
    let mut best = whole;
    for alt in [groups, by_side] {
        if size(&alt) < size(&best) {
            best = alt;
        }
    }
    *d = best;

    // --- four-sided groups ----------------------------------------------------------------
    for (names, short) in [
        (["margin-top", "margin-right", "margin-bottom", "margin-left"], "margin"),
        (["padding-top", "padding-right", "padding-bottom", "padding-left"], "padding"),
        (["top", "right", "bottom", "left"], "inset"),
    ] {
        if let Some(v) = get4(names) {
            replace(d, &names, 2, short, four(&v));
        }
    }
    let radius = ["border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius", "border-bottom-left-radius"];
    if let Some(v) = get4(radius) {
        if v.iter().all(|x| !x.contains(' ')) {
            replace(d, &radius, 2, "border-radius", four(&v));
        }
    }

    // --- pairs ----------------------------------------------------------------------------
    let pair = |d: &mut Decls, a: &str, b: &str, short: &str, sep: &str, min: usize| {
        if let (Some(x), Some(y)) = (target(a), target(b)) {
            let v = if x == y && sep == " " { x } else { format!("{x}{sep}{y}") };
            replace(d, &[a, b], min, short, v);
        }
    };
    pair(d, "overflow-x", "overflow-y", "overflow", " ", 2);
    pair(d, "row-gap", "column-gap", "gap", " ", 2);
    pair(d, "flex-direction", "flex-wrap", "flex-flow", " ", 2);
    pair(d, "-webkit-border-horizontal-spacing", "-webkit-border-vertical-spacing", "border-spacing", " ", 2);
    for axis in ["row", "column"] {
        let (a, b) = (format!("grid-{axis}-start"), format!("grid-{axis}-end"));
        if let (Some(x), Some(y)) = (target(&a), target(&b)) {
            // `grid-row: x` leaves the end `auto` only when x is not a custom ident.
            let plain = x == "auto" || x.starts_with("span") || x.bytes().all(|c| c.is_ascii_digit() || c == b'-');
            let v = if y == "auto" && plain { x } else { format!("{x}/{y}") };
            replace(d, &[&a, &b], 1, &format!("grid-{axis}"), v);
        }
    }

    // --- flex -----------------------------------------------------------------------------
    if let (Some(g), Some(s), Some(b)) = (target("flex-grow"), target("flex-shrink"), target("flex-basis")) {
        let v = if s == "1" && b == "0%" {
            g
        } else if g == "1" && s == "1" && b == "auto" {
            "auto".into()
        } else if g == "0" && s == "0" && b == "auto" {
            "none".into()
        } else {
            format!("{g} {s} {b}")
        };
        replace(d, &["flex-grow", "flex-shrink", "flex-basis"], 2, "flex", v);
    }

    // --- white-space ----------------------------------------------------------------------
    if let (Some(c), Some(w)) = (target("white-space-collapse"), target("text-wrap-mode")) {
        let v = match (c.as_str(), w.as_str()) {
            ("collapse", "wrap") => Some("normal"),
            ("collapse", "nowrap") => Some("nowrap"),
            ("preserve", "nowrap") => Some("pre"),
            ("preserve", "wrap") => Some("pre-wrap"),
            ("preserve-breaks", "wrap") => Some("pre-line"),
            ("break-spaces", "wrap") => Some("break-spaces"),
            _ => None,
        };
        if let Some(v) = v {
            replace(d, &["white-space-collapse", "text-wrap-mode"], 1, "white-space", v.into());
        }
    }

    // --- outline / text-decoration --------------------------------------------------------
    if let (Some(w), Some(s), Some(c)) = (target("outline-width"), target("outline-style"), target("outline-color")) {
        replace(d, &["outline-width", "outline-style", "outline-color"], 2, "outline", border_part(&w, &s, &c));
    }
    if let (Some(l), Some(s), Some(c), Some(t)) =
        (target("text-decoration-line"), target("text-decoration-style"), target("text-decoration-color"), target("text-decoration-thickness"))
    {
        let mut parts = vec![l];
        if s != "solid" {
            parts.push(s);
        }
        if c != "currentcolor" {
            parts.push(c);
        }
        if t != "auto" {
            parts.push(t);
        }
        replace(
            d,
            &["text-decoration-line", "text-decoration-style", "text-decoration-color", "text-decoration-thickness"],
            1,
            "text-decoration",
            parts.join(" "),
        );
    }

    // --- background -----------------------------------------------------------------------
    let bg = [
        "background-color",
        "background-image",
        "background-repeat",
        "background-attachment",
        "background-position",
        "background-size",
        "background-origin",
        "background-clip",
    ];
    let bv: Vec<String> = bg.iter().map(|n| tv(n)).collect();
    if bv.iter().all(|v| !v.is_empty()) {
        // One entry per layer; a shorter list repeats (a single value holds for every layer).
        let lists: Vec<Vec<String>> = bv[1..].iter().map(|v| split_layers(v)).collect();
        let n = lists[0].len();
        let boxes_ok = ["border-box", "padding-box", "content-box"];
        let usable = lists.iter().all(|l| l.len() == n || l.len() == 1) && lists[5].iter().chain(&lists[6]).all(|b| boxes_ok.contains(&b.as_str()));
        if usable {
            let at = |k: usize, layer: usize| -> &str { if lists[k].len() == 1 { &lists[k][0] } else { &lists[k][layer] } };
            let mut layers: Vec<String> = Vec::with_capacity(n);
            for layer in 0..n {
                let (image, repeat, attachment, position, size, origin, clip) =
                    (at(0, layer), at(1, layer), at(2, layer), at(3, layer), at(4, layer), at(5, layer), at(6, layer));
                let mut parts: Vec<String> = Vec::new();
                // The colour belongs to the last layer.
                if layer + 1 == n && !matches!(bv[0].as_str(), "#0000" | "#00000000" | "transparent") {
                    parts.push(bv[0].clone());
                }
                if image != "none" {
                    parts.push(image.to_string());
                }
                if repeat != "repeat" {
                    parts.push(repeat.to_string());
                }
                if attachment != "scroll" {
                    parts.push(attachment.to_string());
                }
                if size != "auto" {
                    parts.push(format!("{position}/{size}"));
                } else if position != "0% 0%" {
                    parts.push(position.to_string());
                }
                if origin != "padding-box" || clip != "border-box" {
                    parts.push(format!("{origin} {clip}"));
                }
                layers.push(if parts.is_empty() { "none".to_string() } else { parts.join(" ") });
            }
            replace(d, &bg, 1, "background", layers.join(","));
        }
    }

    // --- font -----------------------------------------------------------------------------
    fold_font(d, target);
}

/// Split a (minified) comma-separated list at its top-level commas.
fn split_layers(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, 0u8);
    for (i, &c) in v.as_bytes().iter().enumerate() {
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => quote = c,
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(v[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(v[start..].trim().to_string());
    out
}

/// `font` resets every font sub-property to its initial value, so it is only used when all
/// the ones it does not spell out are meant to be initial anyway.
fn fold_font(d: &mut Decls, target: &dyn Fn(&str) -> Option<String>) {
    if find(d, "font-size").is_none() || find(d, "font-family").is_none() {
        return;
    }
    const INITIAL: &[(&str, &str)] = &[
        ("font-variant-caps", "normal"),
        ("font-variant-ligatures", "normal"),
        ("font-variant-numeric", "normal"),
        ("font-variant-east-asian", "normal"),
        ("font-variant-alternates", "normal"),
        ("font-variant-position", "normal"),
        ("font-variant-emoji", "normal"),
        ("font-kerning", "auto"),
        ("font-optical-sizing", "auto"),
        ("font-feature-settings", "normal"),
        ("font-variation-settings", "normal"),
        ("font-language-override", "normal"),
        ("font-size-adjust", "none"),
        ("font-palette", "normal"),
        ("font-synthesis-weight", "auto"),
        ("font-synthesis-style", "auto"),
        ("font-synthesis-small-caps", "auto"),
    ];
    for (name, init) in INITIAL {
        match target(name) {
            Some(v) if v == *init => {}
            Some(_) => return,
            None => {}
        }
    }
    let (Some(style), Some(weight), Some(stretch), Some(size), Some(lh), Some(family)) = (
        target("font-style"),
        target("font-weight"),
        target("font-stretch"),
        target("font-size"),
        target("line-height"),
        target("font-family"),
    ) else {
        return;
    };
    let stretch_kw = match stretch.as_str() {
        "100%" => "",
        "50%" => "ultra-condensed",
        "62.5%" => "extra-condensed",
        "75%" => "condensed",
        "87.5%" => "semi-condensed",
        "112.5%" => "semi-expanded",
        "125%" => "expanded",
        "150%" => "extra-expanded",
        "200%" => "ultra-expanded",
        _ => return,
    };
    if !(style == "normal" || style == "italic" || style.starts_with("oblique")) {
        return;
    }
    let mut v = String::new();
    if style != "normal" {
        v.push_str(&style);
        v.push(' ');
    }
    if weight != "400" {
        v.push_str(&weight);
        v.push(' ');
    }
    if !stretch_kw.is_empty() {
        v.push_str(stretch_kw);
        v.push(' ');
    }
    v.push_str(&size);
    if lh != "normal" {
        v.push('/');
        v.push_str(&lh);
    }
    v.push(' ');
    v.push_str(&family);
    let mut covers = vec!["font-style", "font-weight", "font-stretch", "font-size", "line-height", "font-family"];
    covers.extend(INITIAL.iter().map(|(n, _)| *n));
    replace(d, &covers, 2, "font", v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(decls: &[(&str, &str)], extra: &[(&str, &str)]) -> String {
        let mut d: Decls = decls.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let mut all: HashMap<String, String> = extra.iter().chain(decls.iter()).map(|(a, b)| (a.to_string(), b.to_string())).collect();
        for s in ["top", "right", "bottom", "left"] {
            all.entry(format!("border-{s}-width")).or_insert("3px".into());
            all.entry(format!("border-{s}-style")).or_insert("none".into());
            all.entry(format!("border-{s}-color")).or_insert("currentcolor".into());
            all.entry(format!("margin-{s}")).or_insert("0".into());
            all.entry(format!("padding-{s}")).or_insert("0".into());
        }
        for (k, v) in [
            ("border-image-source", "none"),
            ("border-image-outset", "0"),
            ("border-image-repeat", "stretch"),
            ("border-image-slice", "100%"),
            ("border-image-width", "1"),
            ("background-color", "#0000"),
            ("background-image", "none"),
            ("background-repeat", "repeat"),
            ("background-attachment", "scroll"),
            ("background-position", "0% 0%"),
            ("background-size", "auto"),
            ("background-origin", "padding-box"),
            ("background-clip", "border-box"),
        ] {
            all.entry(k.into()).or_insert(v.into());
        }
        fold(&mut d, &|n| all.get(n).cloned());
        d.iter().map(|(a, b)| format!("{a}:{b}")).collect::<Vec<_>>().join(";")
    }

    #[test]
    fn minifies_colors_and_zeros() {
        assert_eq!(minify_value("rgb(255, 0, 0)"), "#f00");
        assert_eq!(minify_value("rgb(18, 52, 86)"), "#123456");
        assert_eq!(minify_value("rgba(0, 0, 0, 0)"), "#0000");
        assert_eq!(minify_value("rgba(0, 0, 0, 0.5)"), "#00000080");
        assert_eq!(minify_value("rgba(0, 0, 0, 0.2)"), "#0003");
        assert_eq!(minify_value("rgb(0, 0, 0) 0px 1px 2px 0px"), "#000 0 1px 2px 0");
        assert_eq!(minify_value("0px"), "0");
        assert_eq!(minify_value("0px 10px"), "0 10px");
        assert_eq!(minify_value("-0.5px"), "-.5px");
        assert_eq!(minify_value("1.05"), "1.05");
        assert_eq!(minify_value("10px"), "10px");
        assert_eq!(minify_value("calc(100% - 0px)"), "calc(100% - 0px)");
        assert_eq!(minify_value("0.5"), ".5");
        assert_eq!(minify_value("10.5px"), "10.5px");
        assert_eq!(minify_value("\"Helvetica Neue\", Arial, sans-serif"), "\"Helvetica Neue\",Arial,sans-serif");
        assert_eq!(minify_value("linear-gradient(rgb(255, 0, 0), rgb(0, 0, 255))"), "linear-gradient(#f00,#00f)");
        assert_eq!(minify_value("url(\"a, b.png\")"), "url(\"a, b.png\")");
        assert_eq!(minify_value("auto 100 / 50"), "auto 100/50");
        assert_eq!(minify_value("translate(-50%, 0px) rotate(3deg)"), "translate(-50%,0px) rotate(3deg)");
    }

    #[test]
    fn folds_sides() {
        assert_eq!(run(&[("margin-top", "10px"), ("margin-bottom", "10px")], &[]), "margin:10px 0");
        assert_eq!(run(&[("padding-top", "4px"), ("padding-right", "4px"), ("padding-bottom", "4px"), ("padding-left", "4px")], &[]), "padding:4px");
        assert_eq!(run(&[("margin-left", "8px")], &[]), "margin-left:8px");
    }

    #[test]
    fn folds_borders() {
        let all: Vec<(String, String)> = ["top", "right", "bottom", "left"]
            .iter()
            .flat_map(|s| {
                vec![
                    (format!("border-{s}-width"), "1px".to_string()),
                    (format!("border-{s}-style"), "solid".to_string()),
                    (format!("border-{s}-color"), "#ccd".to_string()),
                ]
            })
            .collect();
        let refs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(run(&refs, &[]), "border:1px solid #ccd");
        assert_eq!(
            run(&[("border-bottom-width", "1px"), ("border-bottom-style", "solid"), ("border-bottom-color", "#eee")], &[]),
            "border-bottom:1px solid #eee"
        );
        // currentcolor is the default and may be left out
        assert_eq!(run(&[("border-top-width", "2px"), ("border-top-style", "dashed")], &[]), "border-top:2px dashed");
    }

    #[test]
    fn folds_background_and_flex() {
        assert_eq!(run(&[("background-color", "#fff")], &[]), "background:#fff");
        assert_eq!(
            run(&[("background-image", "url(\"x.png\")"), ("background-repeat", "no-repeat"), ("background-size", "cover"), ("background-position", "50% 50%")], &[]),
            "background:url(\"x.png\") no-repeat 50% 50%/cover"
        );
        // two layers: the gradient carries the colour-less second layer
        assert_eq!(
            run(&[("background-image", "url(\"a.svg\"),linear-gradient(#000,#fff)"), ("background-repeat", "no-repeat,repeat"), ("background-size", "10px,auto")], &[]),
            "background:url(\"a.svg\") no-repeat 0% 0%/10px,linear-gradient(#000,#fff)"
        );
        assert_eq!(split_layers("url(\"a,b.png\"),linear-gradient(red,blue)").len(), 2);
        assert_eq!(run(&[("flex-grow", "1"), ("flex-basis", "0%")], &[("flex-shrink", "1")]), "flex:1");
        assert_eq!(run(&[("flex-grow", "0"), ("flex-shrink", "0")], &[("flex-basis", "auto")]), "flex:none");
    }

    #[test]
    fn folds_font_only_when_safe() {
        let base = [("font-style", "normal"), ("font-stretch", "100%")];
        assert_eq!(
            run(&[("font-family", "Arial"), ("font-size", "14px"), ("line-height", "1.4"), ("font-weight", "700")], &base),
            "font:700 14px/1.4 Arial"
        );
        // a non-initial sub-property the shorthand would reset: keep the longhands
        let kept = run(&[("font-family", "Arial"), ("font-size", "14px")], &[("font-style", "normal"), ("font-stretch", "100%"), ("font-weight", "400"), ("line-height", "normal"), ("font-kerning", "none")]);
        assert_eq!(kept, "font-family:Arial;font-size:14px");
    }

    #[test]
    fn folds_white_space() {
        assert_eq!(run(&[("text-wrap-mode", "nowrap")], &[("white-space-collapse", "collapse")]), "white-space:nowrap");
    }
}
