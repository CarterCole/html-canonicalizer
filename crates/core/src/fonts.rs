//! Web fonts. The copy is a separate document, so the `@font-face` rules the original
//! relied on have to travel with it - only the faces that actually loaded, and by default
//! with the font data inlined (fonts are subject to CORS; a copy on another origin could
//! not fetch them).

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// Descriptors in source order, names lower-cased.
    pub descriptors: Vec<(String, String)>,
    /// URL the rule's relative URLs resolve against.
    pub base: String,
}

impl Face {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.descriptors.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Loaded {
    pub family: String,
    pub style: String,
    pub weight: String,
    #[serde(default)]
    pub range: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Src {
    pub local: bool,
    pub url: Option<String>,
    pub format: Option<String>,
    pub raw: String,
}

fn skip_string(b: &[u8], mut i: usize) -> usize {
    let q = b[i];
    i += 1;
    while i < b.len() && b[i] != q {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

fn skip_comment(b: &[u8], i: usize) -> usize {
    let mut j = i + 2;
    while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
        j += 1;
    }
    (j + 2).min(b.len())
}

/// Every `@font-face` block in a stylesheet, wherever it is nested.
pub fn extract(css: &str, base: &str) -> Vec<Face> {
    let b = css.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = skip_comment(b, i);
                continue;
            }
            b'"' | b'\'' => {
                i = skip_string(b, i);
                continue;
            }
            b'@' if b.len() - i >= 10 && b[i..i + 10].eq_ignore_ascii_case(b"@font-face") => {
                let mut j = i + 10;
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if b.get(j) == Some(&b'{') {
                    let start = j + 1;
                    let mut k = start;
                    while k < b.len() && b[k] != b'}' {
                        match b[k] {
                            b'"' | b'\'' => k = skip_string(b, k),
                            b'/' if b.get(k + 1) == Some(&b'*') => k = skip_comment(b, k),
                            _ => k += 1,
                        }
                    }
                    let end = k.min(b.len());
                    out.push(Face { descriptors: parse_block(&css[start..end]), base: base.to_string() });
                    i = end + 1;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Split at `sep` outside strings and parentheses.
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0i32, 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                i = skip_string(b, i);
                continue;
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            c if c == sep && depth <= 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start.min(s.len())..]);
    out
}

fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_string(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&s[last..i]);
                i = skip_comment(b, i);
                last = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&s[last.min(s.len())..]);
    out
}

fn parse_block(body: &str) -> Vec<(String, String)> {
    let body = strip_comments(body);
    split_top(&body, b';')
        .into_iter()
        .filter_map(|d| {
            let (name, value) = d.split_once(':')?;
            let value = value.trim().trim_end_matches("!important").trim();
            (!value.is_empty()).then(|| (name.trim().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 && (s.starts_with('"') && s.ends_with('"') || s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// The argument of the first `name(...)` in `s`.
fn func_arg<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let at = lower.find(&format!("{name}("))?;
    let open = at + name.len();
    let b = s.as_bytes();
    let mut i = open + 1;
    while i < b.len() && b[i] != b')' {
        match b[i] {
            b'"' | b'\'' => i = skip_string(b, i),
            _ => i += 1,
        }
    }
    Some(&s[open + 1..i.min(s.len())])
}

pub fn parse_src(value: &str, base: &str) -> Vec<Src> {
    split_top(value, b',')
        .into_iter()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|entry| {
            let local = entry.to_ascii_lowercase().starts_with("local(");
            let url = if local {
                None
            } else {
                func_arg(entry, "url").map(|u| {
                    let u = unquote(u);
                    if u.starts_with("data:") {
                        u.to_string()
                    } else {
                        url::Url::parse(base).and_then(|b| b.join(u)).map(|x| x.to_string()).unwrap_or_else(|_| u.to_string())
                    }
                })
            };
            let format = func_arg(entry, "format").map(|f| unquote(f).to_ascii_lowercase());
            Src { local, url, format, raw: entry.to_string() }
        })
        .collect()
}

fn usable(format: &Option<String>) -> bool {
    match format {
        None => true,
        Some(f) => f.contains("woff") || f.contains("truetype") || f.contains("opentype") || f == "collection",
    }
}

fn norm_family(s: &str) -> String {
    unquote(s).to_ascii_lowercase()
}

fn norm_weight(s: &str) -> String {
    s.split_whitespace()
        .map(|w| match w.to_ascii_lowercase().as_str() {
            "normal" => "400".to_string(),
            "bold" => "700".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `U+0-FF, U+131, U+4??` as sorted code point ranges.
fn norm_range(s: &str) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = s
        .split(',')
        .filter_map(|part| {
            let p = part.trim().to_ascii_uppercase();
            let p = p.strip_prefix("U+")?;
            if let Some((a, b)) = p.split_once('-') {
                Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b, 16).ok()?))
            } else if p.contains('?') {
                Some((u32::from_str_radix(&p.replace('?', "0"), 16).ok()?, u32::from_str_radix(&p.replace('?', "F"), 16).ok()?))
            } else {
                let v = u32::from_str_radix(p, 16).ok()?;
                Some((v, v))
            }
        })
        .collect();
    out.sort_unstable();
    if out.is_empty() {
        out.push((0, 0x10FFFF));
    }
    out
}

/// The faces the page actually used: those matching a font the browser reports as loaded.
pub fn needed(faces: &[Face], loaded: &[Loaded]) -> Vec<Face> {
    let mut out: Vec<Face> = Vec::new();
    let mut push = |f: &Face| {
        if !out.iter().any(|o| o.descriptors == f.descriptors) {
            out.push(f.clone());
        }
    };
    let mut families: Vec<String> = loaded.iter().map(|l| norm_family(&l.family)).collect();
    families.sort();
    families.dedup();
    for fam in families {
        let of_family: Vec<&Face> = faces.iter().filter(|f| f.get("font-family").is_some_and(|x| norm_family(x) == fam)).collect();
        let mut any = false;
        for f in &of_family {
            let weight = norm_weight(f.get("font-weight").unwrap_or("400"));
            let style = f.get("font-style").unwrap_or("normal").to_ascii_lowercase();
            let range = norm_range(f.get("unicode-range").unwrap_or(""));
            let hit = loaded.iter().any(|l| {
                norm_family(&l.family) == fam && norm_weight(&l.weight) == weight && l.style.to_ascii_lowercase() == style && norm_range(&l.range) == range
            });
            if hit {
                push(f);
                any = true;
            }
        }
        if !any {
            // Could not tell which face it was: take them all rather than lose the font.
            for f in &of_family {
                push(f);
            }
        }
    }
    out
}

fn mime(format: &Option<String>, url: &str) -> &'static str {
    let ext = url.split(['?', '#']).next().unwrap_or("").rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let f = format.as_deref().unwrap_or("");
    if f.contains("woff2") || ext == "woff2" {
        "font/woff2"
    } else if f.contains("woff") || ext == "woff" {
        "font/woff"
    } else if f.contains("opentype") || ext == "otf" {
        "font/otf"
    } else {
        "font/ttf"
    }
}

/// `@font-face` rule for one face, with its font inlined when `fetch` can get the bytes.
pub fn rule(face: &Face, embed: bool, fetch: &mut dyn FnMut(&str) -> Option<String>) -> Option<String> {
    let srcs = parse_src(face.get("src")?, &face.base);
    let mut parts: Vec<String> = Vec::new();
    for s in &srcs {
        if s.local {
            parts.push(s.raw.clone());
            continue;
        }
        let Some(url) = &s.url else { continue };
        if !usable(&s.format) {
            continue;
        }
        let fmt = s.format.as_ref().map(|f| format!(" format(\"{f}\")")).unwrap_or_default();
        let href = if embed && !url.starts_with("data:") {
            match fetch(url) {
                Some(b64) => format!("data:{};base64,{}", mime(&s.format, url), b64),
                None => url.clone(),
            }
        } else {
            url.clone()
        };
        parts.push(format!("url(\"{href}\"){fmt}"));
        break; // the first source the browser can use is the one it used
    }
    if !parts.iter().any(|p| p.starts_with("url(")) {
        return None;
    }
    let mut out = String::from("@font-face{");
    let mut wrote_src = false;
    let mut seen: Vec<&str> = Vec::new();
    for (name, value) in &face.descriptors {
        if name == "font-display" || seen.contains(&name.as_str()) {
            continue;
        }
        seen.push(name);
        if name == "src" {
            if !wrote_src {
                out.push_str(&format!("src:{};", parts.join(",")));
                wrote_src = true;
            }
            continue;
        }
        out.push_str(&format!("{name}:{};", face.get(name).unwrap_or(value)));
    }
    if out.ends_with(';') {
        out.pop();
    }
    out.push_str("}\n");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = r#"
      /* @font-face { font-family: Nope; src: url(nope.woff2) } */
      body { content: "@font-face {"; }
      @media screen {
        @font-face {
          font-family: "Inter";
          font-weight: 400;
          src: local("Inter"), url(fonts/inter.woff2?v=1) format("woff2"), url('fonts/inter.woff') format('woff');
          unicode-range: U+0000-00FF, U+0131;
          font-display: swap;
        }
      }
      @FONT-FACE{font-family:Inter;font-weight:bold;font-style:italic;src:url(/abs/inter-bi.ttf)}
    "#;

    #[test]
    fn finds_faces_and_ignores_comments_and_strings() {
        let faces = extract(CSS, "https://example.com/css/site.css");
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].get("font-family"), Some("\"Inter\""));
        assert_eq!(faces[1].get("font-weight"), Some("bold"));
    }

    #[test]
    fn resolves_and_orders_sources() {
        let faces = extract(CSS, "https://example.com/css/site.css");
        let srcs = parse_src(faces[0].get("src").unwrap(), &faces[0].base);
        assert_eq!(srcs.len(), 3);
        assert!(srcs[0].local);
        assert_eq!(srcs[1].url.as_deref(), Some("https://example.com/css/fonts/inter.woff2?v=1"));
        assert_eq!(srcs[1].format.as_deref(), Some("woff2"));
        let s2 = parse_src(faces[1].get("src").unwrap(), &faces[1].base);
        assert_eq!(s2[0].url.as_deref(), Some("https://example.com/abs/inter-bi.ttf"));
    }

    #[test]
    fn keeps_only_loaded_faces() {
        let faces = extract(CSS, "https://example.com/css/site.css");
        let loaded = vec![Loaded { family: "Inter".into(), style: "normal".into(), weight: "400".into(), range: "U+0-FF, U+131".into() }];
        let need = needed(&faces, &loaded);
        assert_eq!(need.len(), 1);
        assert_eq!(need[0].get("font-weight"), Some("400"));
        // bold italic, written as keywords in the sheet and as numbers by the browser
        let loaded = vec![Loaded { family: "\"Inter\"".into(), style: "italic".into(), weight: "700".into(), range: "U+0-10FFFF".into() }];
        assert_eq!(needed(&faces, &loaded)[0].get("font-style"), Some("italic"));
    }

    #[test]
    fn writes_a_rule_with_inlined_data() {
        let faces = extract(CSS, "https://example.com/css/site.css");
        let mut asked = Vec::new();
        let rule = rule(&faces[0], true, &mut |u| {
            asked.push(u.to_string());
            Some("QUJD".into())
        })
        .unwrap();
        assert_eq!(asked, vec!["https://example.com/css/fonts/inter.woff2?v=1"]);
        assert_eq!(
            rule,
            "@font-face{font-family:\"Inter\";font-weight:400;src:local(\"Inter\"),url(\"data:font/woff2;base64,QUJD\") format(\"woff2\");unicode-range:U+0000-00FF, U+0131}\n"
        );
        let linked = super::rule(&faces[1], false, &mut |_| None).unwrap();
        assert!(linked.contains("url(\"https://example.com/abs/inter-bi.ttf\")"));
    }

    #[test]
    fn unicode_ranges_normalise() {
        assert_eq!(norm_range("U+0000-00FF, U+0131"), norm_range("U+0-FF,U+131"));
        assert_eq!(norm_range("U+4??"), vec![(0x400, 0x4FF)]);
        assert_eq!(norm_range(""), vec![(0, 0x10FFFF)]);
    }
}
