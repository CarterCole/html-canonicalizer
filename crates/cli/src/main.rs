//! canon: recreate a block of HTML with the fewest elements and the least CSS, and prove the
//! result renders the same.

mod cdp;
mod chrome;
mod fonts;
mod pixel;

use anyhow::{anyhow, bail, Context, Result};
use canon_core::geom;
use canon_core::{CompareError, Doc, Engine, Measure, Options, Plan, Renderer, StyleMode};
use cdp::{Cdp, Page};
use clap::{Parser, ValueEnum};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const AGENT: &str = include_str!("../../../agent/agent.js");
/// Chrome cannot rasterise a surface taller or wider than this in one piece.
const MAX_SHOT: f64 = 16000.0;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum StyleArg {
    /// A class where a declaration block repeats, an inline style where it does not
    Auto,
    /// Inline styles only (nothing but the markup to paste)
    Inline,
    /// Every element styled through a class
    Classes,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq)]
enum Scheme {
    Light,
    Dark,
    /// Whatever this machine is set to
    System,
}

#[derive(Parser, Debug)]
#[command(
    name = "canon",
    version,
    about = "Recreate a block of HTML with the fewest elements and the least CSS that render the same.",
    after_help = "Examples:\n  canon page.html -o min.html\n  canon https://example.com -s main --screenshot before.png\n  echo '<div class=a><div><p>hi</p></div></div>' | canon - --style inline --fragment\n\nExit status: 0 when the result is verified identical, 2 when differences remain, 1 on error."
)]
struct Args {
    /// An HTML file, a URL, literal markup (starting with `<`), or `-` for stdin
    input: String,

    /// CSS selector of the element to recreate [default: the page's single top-level element, else <body>]
    #[arg(short, long)]
    selector: Option<String>,

    /// Write the result here instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Viewport the page is laid out in
    #[arg(long, default_value = "1280x800", value_name = "WxH")]
    viewport: String,

    /// Device pixel ratio used for rendering and screenshots
    #[arg(long, default_value_t = 1.0)]
    dpr: f64,

    /// How declarations are attached
    #[arg(long, value_enum, default_value_t = StyleArg::Auto)]
    style: StyleArg,

    /// Emit only `<style>` and the element, not a whole document
    #[arg(long)]
    fragment: bool,

    /// Leave the element's width to its container instead of fixing the captured width
    #[arg(long)]
    fluid: bool,

    /// Keep every element; only reduce the CSS
    #[arg(long)]
    no_reduce: bool,

    /// Keep declarations that cannot change the picture (a border colour with no border, ...)
    #[arg(long)]
    no_simplify: bool,

    /// Skip the screenshot comparison (geometry is still checked)
    #[arg(long)]
    no_verify: bool,

    /// Compare screenshots after every accepted reduction step, not just at the end
    #[arg(long)]
    strict: bool,

    /// Reference web fonts by URL instead of inlining them
    #[arg(long)]
    no_embed_fonts: bool,

    /// Save a screenshot of the original element
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,

    /// Save a screenshot of the recreated element (as rendered for the final check)
    #[arg(long, value_name = "PNG")]
    screenshot_result: Option<PathBuf>,

    /// Save a picture of the differing pixels, if any
    #[arg(long, value_name = "PNG")]
    diff: Option<PathBuf>,

    /// Save a machine-readable report
    #[arg(long, value_name = "JSON")]
    report: Option<PathBuf>,

    /// Extra time to let the page settle after it loads, in milliseconds
    #[arg(long, default_value_t = 0, value_name = "MS")]
    wait: u64,

    /// How long to wait for the page and the selector, in seconds
    #[arg(long, default_value_t = 30, value_name = "S")]
    timeout: u64,

    /// Per-channel difference a pixel may have and still count as equal
    #[arg(long, default_value_t = 0)]
    tolerance: u8,

    /// Stop reducing after this many seconds and keep what has been verified so far
    #[arg(long, default_value_t = 120, value_name = "S")]
    time_limit: u64,

    /// Upper bound on layout trials
    #[arg(long, default_value_t = 20_000)]
    max_trials: usize,

    /// Prefix for generated class names
    #[arg(long, default_value = "")]
    class_prefix: String,

    /// `prefers-color-scheme` the page sees
    #[arg(long, value_enum, default_value_t = Scheme::Light)]
    color_scheme: Scheme,

    /// Chrome or Chromium binary [default: found automatically, or $CANON_CHROME]
    #[arg(long, value_name = "PATH")]
    chrome: Option<PathBuf>,

    /// Run Chrome without its sandbox (needed as root or in some containers)
    #[arg(long)]
    no_sandbox: bool,

    /// Print nothing but errors
    #[arg(short, long)]
    quiet: bool,

    /// Print what is happening
    #[arg(short, long)]
    verbose: bool,
}

struct Log {
    verbose: bool,
    t0: Instant,
}

impl Log {
    fn step(&self, msg: &str) {
        if self.verbose {
            eprintln!("[{:>6.0} ms] {msg}", self.t0.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

/// The clean second tab: lays out trial documents and photographs them.
struct Replica {
    page: Page,
    reference: Option<pixel::Image>,
    /// Region of the original to compare, in source viewport px (whole pixels).
    clip: [f64; 4],
    /// Top-left of the original root, in source viewport px.
    src_origin: (f64, f64),
    viewport: (f64, f64),
    tolerance: u8,
    shots: usize,
    misplaced: bool,
    /// The last trial photographed, and the part of the original it covers (source viewport px).
    last: Option<(pixel::Image, [f64; 4])>,
    deadline: Option<Instant>,
    /// Device pixels per CSS px in the screenshots.
    scale: f64,
}

fn origin_px(m: &Measure) -> Option<(f64, f64)> {
    let b = geom::bounds(&m.nodes.first()?.1)?;
    Some((b[0] as f64 / geom::UNIT, b[1] as f64 / geom::UNIT))
}

impl Replica {
    fn measure(&self, expr: &str) -> Result<Measure> {
        Measure::parse(&self.page.eval_string(expr)?).map_err(|e| anyhow!(e))
    }

    /// Photograph the document now in the page. `stage` is the body offset it was laid out
    /// with, `target` where the root should be. The stage is nudged so the root lands a
    /// whole number of pixels from the original: same sub-pixel phase, same rasterisation.
    /// A region given in px from the root's top-left, as whole pixels of the original
    /// inside the compared area (`None`: the whole area).
    fn sub(&self, region: Option<[f64; 4]>) -> [f64; 4] {
        let c = self.clip;
        let Some(r) = region else { return c };
        let x0 = (self.src_origin.0 + r[0]).floor().clamp(c[0], c[0] + c[2] - 1.0);
        let y0 = (self.src_origin.1 + r[1]).floor().clamp(c[1], c[1] + c[3] - 1.0);
        let x1 = (self.src_origin.0 + r[0] + r[2]).ceil().clamp(x0 + 1.0, c[0] + c[2]);
        let y1 = (self.src_origin.1 + r[1] + r[3]).ceil().clamp(y0 + 1.0, c[1] + c[3]);
        [x0, y0, x1 - x0, y1 - y0]
    }

    /// Device-pixel rectangle of `part` inside a photograph that starts at `origin` (both in CSS px).
    fn device(&self, part: [f64; 4], origin: (f64, f64)) -> (u32, u32, u32, u32) {
        let d = |v: f64| (v * self.scale).round() as u32;
        (d(part[0] - origin.0), d(part[1] - origin.1), d(part[2]), d(part[3]))
    }

    fn reference_part(&self, sub: [f64; 4]) -> Option<pixel::Image> {
        let r = self.reference.as_ref()?;
        let (x, y, w, h) = self.device(sub, (self.clip[0], self.clip[1]));
        Some(pixel::crop(r, x, y, w, h))
    }

    fn shoot(&mut self, stage: (f64, f64), target: (f64, f64), m: &Measure) -> Result<pixel::Image> {
        self.shoot_part(stage, target, m, self.clip)
    }

    /// As `shoot`, for the part `sub` of the compared area (source viewport px).
    fn shoot_part(&mut self, stage: (f64, f64), target: (f64, f64), m: &Measure, sub: [f64; 4]) -> Result<pixel::Image> {
        let (ox, oy) = origin_px(m).ok_or_else(|| anyhow!("the copy's root has no box"))?;
        // The root sits at stage + c, where c is whatever its own content pushed it by.
        let (cx, cy) = (ox - stage.0, oy - stage.1);
        let (mut px, mut py) = (target.0 - cx, target.1 - cy);
        if px < 0.0 {
            px += (-px).ceil();
        }
        if py < 0.0 {
            py += (-py).ceil();
        }
        let (ax, ay) = if (px - stage.0).abs() > 0.004 || (py - stage.1).abs() > 0.004 {
            let m2 = self.measure(&format!("__canon.offset({px},{py})"))?;
            origin_px(&m2).ok_or_else(|| anyhow!("the copy's root has no box"))?
        } else {
            (ox, oy)
        };
        let (dx, dy) = (ax - self.src_origin.0, ay - self.src_origin.1);
        let (kx, ky) = (dx.round(), dy.round());
        if (dx - kx).abs() > 0.02 || (dy - ky).abs() > 0.02 {
            self.misplaced = true;
        }
        let clip = [(sub[0] + kx).max(0.0), (sub[1] + ky).max(0.0), sub[2], sub[3]];
        let beyond = clip[0] + clip[2] > self.viewport.0 || clip[1] + clip[3] > self.viewport.1;
        self.shots += 1;
        pixel::decode(&self.page.screenshot(clip, beyond)?)
    }
}

impl Renderer for Replica {
    fn render(&mut self, doc: &Doc) -> std::result::Result<Measure, String> {
        let arg = json!({ "html": doc.html, "css": doc.css, "x": doc.x, "y": doc.y, "w": doc.w, "bg": doc.bg });
        let s = self.page.eval_string(&format!("__canon.render({arg})")).map_err(|e| format!("{e:#}"))?;
        Measure::parse(&s)
    }

    fn accept(&mut self, doc: &Doc, m: &Measure) -> std::result::Result<Option<bool>, String> {
        if self.reference.is_none() {
            return Ok(None);
        }
        let sub = self.sub(doc.region);
        let img = self.shoot_part((doc.x, doc.y), (doc.x, doc.y), m, sub).map_err(|e| format!("{e:#}"))?;
        let same = pixel::diff(&self.reference_part(sub).unwrap(), &img, self.tolerance).exact();
        self.last = Some((img, sub));
        Ok(Some(same))
    }

    fn out_of_time(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn has_pixels(&self) -> bool {
        self.reference.is_some()
    }

    fn diff_points(&mut self, max: usize) -> Vec<(f64, f64)> {
        let Some((img, sub)) = &self.last else { return Vec::new() };
        let Some(want) = self.reference_part(*sub) else { return Vec::new() };
        let (w, h) = (img.w.min(want.w) as usize, img.h.min(want.h) as usize);
        let mut all = Vec::new();
        for y in 0..h {
            let (a, b) = (&img.rgba[y * img.w as usize * 4..][..w * 4], &want.rgba[y * want.w as usize * 4..][..w * 4]);
            if a == b {
                continue;
            }
            for x in 0..w {
                if (0..4).any(|i| a[x * 4 + i].abs_diff(b[x * 4 + i]) > self.tolerance) {
                    let (cx, cy) = ((x as f64 + 0.5) / self.scale, (y as f64 + 0.5) / self.scale);
                    all.push((sub[0] + cx - self.src_origin.0, sub[1] + cy - self.src_origin.1));
                }
            }
        }
        // An even sample over everything that differs.
        let step = all.len().div_ceil(max.max(1)).max(1);
        all.into_iter().step_by(step).collect()
    }

    fn differs(&mut self, regions: &[[f64; 4]]) -> Vec<bool> {
        let Some((img, sub)) = &self.last else { return vec![false; regions.len()] };
        regions
            .iter()
            .map(|r| {
                let part = self.sub(Some(*r));
                // intersect with what the last photograph covers
                let x0 = part[0].max(sub[0]);
                let y0 = part[1].max(sub[1]);
                let x1 = (part[0] + part[2]).min(sub[0] + sub[2]);
                let y1 = (part[1] + part[3]).min(sub[1] + sub[3]);
                if x1 <= x0 || y1 <= y0 {
                    return false;
                }
                let inter = [x0, y0, x1 - x0, y1 - y0];
                let Some(want) = self.reference_part(inter) else { return false };
                let (gx, gy, gw, gh) = self.device(inter, (sub[0], sub[1]));
                let got = pixel::crop(img, gx, gy, gw, gh);
                !pixel::diff(&want, &got, self.tolerance).exact()
            })
            .collect()
    }
}

fn parse_viewport(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X', ',']).ok_or_else(|| anyhow!("viewport must look like 1280x800"))?;
    let (w, h): (u32, u32) = (w.trim().parse().context("bad viewport width")?, h.trim().parse().context("bad viewport height")?);
    if w == 0 || h == 0 {
        bail!("viewport must not be empty");
    }
    Ok((w, h))
}

fn file_url(p: &Path) -> Result<String> {
    let abs = std::fs::canonicalize(p).with_context(|| format!("cannot open {}", p.display()))?;
    url::Url::from_file_path(&abs).map(|u| u.to_string()).map_err(|_| anyhow!("cannot turn {} into a URL", abs.display()))
}

/// The address to load, writing literal markup or stdin to a file first.
fn resolve_input(input: &str, tmp: &Path) -> Result<String> {
    let literal = |html: &str| -> Result<String> {
        let p = tmp.join("input.html");
        std::fs::write(&p, html)?;
        file_url(&p)
    };
    if input == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("cannot read stdin")?;
        return literal(&s);
    }
    if input.trim_start().starts_with('<') {
        return literal(input);
    }
    if let Ok(u) = url::Url::parse(input) {
        if matches!(u.scheme(), "http" | "https" | "file" | "data" | "about") {
            return Ok(input.to_string());
        }
    }
    let p = Path::new(input);
    if p.exists() {
        return file_url(p);
    }
    bail!("{input} is neither a file nor a URL");
}

struct Verdict {
    structure_ok: bool,
    geometry: Vec<canon_core::Mismatch>,
    styles: Vec<canon_core::StyleMismatch>,
    pixels: Option<pixel::Diff>,
    image: Option<pixel::Image>,
}

impl Verdict {
    fn ok(&self) -> bool {
        self.structure_ok && self.geometry.is_empty() && self.pixels.is_none_or(|d| d.exact())
    }
}

/// Load the finished document the way a browser would and check it end to end.
fn verify(eng: &mut Engine, rep: &mut Replica, plan: &Plan, document: &str, tmp: &Path, init: &str) -> Result<Verdict> {
    let path = tmp.join("final.html");
    std::fs::write(&path, document)?;
    rep.page.navigate(&file_url(&path)?, Duration::from_secs(30), false)?;
    rep.page.eval(AGENT)?;
    rep.page.eval(&format!("__canon.init({init})"))?;
    rep.page.eval("__canon.settle()")?;
    let m = rep.measure("__canon.measure()")?;
    let (structure_ok, geometry) = match eng.compare(plan, &m, 200) {
        Ok(()) => (true, Vec::new()),
        Err(CompareError::Structure { .. }) => (false, Vec::new()),
        Err(CompareError::Geometry(v)) => (true, v),
    };
    let styles = if structure_ok {
        eng.check_styles(plan, &rep.page.eval_string("__canon.styles()")?).map_err(|e| anyhow!(e))?
    } else {
        Vec::new()
    };
    let (mut pixels, mut image) = (None, None);
    if rep.reference.is_some() && structure_ok {
        let img = rep.shoot((0.0, 0.0), eng.target_origin(), &m)?;
        pixels = Some(pixel::diff(rep.reference.as_ref().unwrap(), &img, rep.tolerance));
        image = Some(img);
    }
    Ok(Verdict { structure_ok, geometry, styles, pixels, image })
}

/// For every element the agent marked as having generated content: the property names its
/// matched `::before` / `::after` rules declare.
fn pseudo_rules(page: &Page) -> Result<Vec<(usize, usize, std::collections::HashSet<String>)>> {
    // Chrome answers these one element at a time and slowly on big stylesheets: bounded.
    let until = Instant::now() + Duration::from_secs(3);
    page.call("DOM.enable", json!({}))?;
    page.call("CSS.enable", json!({}))?;
    let doc = page.call("DOM.getDocument", json!({ "depth": 0 }))?;
    let root = doc["root"]["nodeId"].as_i64().ok_or_else(|| anyhow!("no document"))?;
    let found = page.call("DOM.querySelectorAll", json!({ "nodeId": root, "selector": "[data-canon-q]" }))?;
    let mut out = Vec::new();
    for id in found["nodeIds"].as_array().cloned().unwrap_or_default() {
        if Instant::now() >= until {
            break;
        }
        let Ok(attrs) = page.call("DOM.getAttributes", json!({ "nodeId": id })) else { continue };
        let list = attrs["attributes"].as_array().cloned().unwrap_or_default();
        let Some(node) = list.chunks(2).find(|c| c[0] == "data-canon-q").and_then(|c| c.get(1)?.as_str()?.parse::<usize>().ok()) else { continue };
        let Ok(matched) = page.call("CSS.getMatchedStylesForNode", json!({ "nodeId": id })) else { continue };
        let mut sets = [std::collections::HashSet::new(), std::collections::HashSet::new()];
        let mut seen = [false, false];
        for pe in matched["pseudoElements"].as_array().cloned().unwrap_or_default() {
            let which = match pe["pseudoType"].as_str() {
                Some("before") => 0,
                Some("after") => 1,
                _ => continue,
            };
            seen[which] = true;
            for m in pe["matches"].as_array().cloned().unwrap_or_default() {
                for prop in m["rule"]["style"]["cssProperties"].as_array().cloned().unwrap_or_default() {
                    if prop["disabled"] == true || prop["parsedOk"] == false {
                        continue;
                    }
                    if let Some(name) = prop["name"].as_str() {
                        sets[which].insert(name.to_ascii_lowercase());
                    }
                }
            }
        }
        for (which, set) in sets.into_iter().enumerate() {
            if seen[which] {
                out.push((node, which, set));
            }
        }
    }
    let _ = page.call("CSS.disable", json!({}));
    let _ = page.call("DOM.disable", json!({}));
    Ok(out)
}

fn kb(n: usize) -> String {
    if n < 1000 {
        format!("{n} B")
    } else {
        format!("{:.1} kB", n as f64 / 1000.0)
    }
}

fn main() {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("canon: {e:#}");
            std::process::exit(1);
        }
    }
}

fn run(args: &Args) -> Result<i32> {
    let log = Log { verbose: args.verbose && !args.quiet, t0: Instant::now() };
    let tmp = tempfile::Builder::new().prefix("canon-").tempdir().context("cannot create a temporary directory")?;
    let url = resolve_input(&args.input, tmp.path())?;
    let (vw, vh) = parse_viewport(&args.viewport)?;
    if !(args.dpr > 0.0 && args.dpr <= 8.0) {
        bail!("--dpr must be between 0 and 8");
    }
    let timeout = Duration::from_secs(args.timeout.max(1));

    let chrome_path = chrome::find(args.chrome.as_deref())?;
    let browser = chrome::Chrome::launch(&chrome_path, !args.no_sandbox)?;
    let cdp = Rc::new(RefCell::new(Cdp::connect(&browser.ws_url)?));
    log.step("browser ready");

    // ------------------------------------------------------------ capture
    let scheme = |page: &Page| -> Result<()> {
        let value = match args.color_scheme {
            Scheme::Light => "light",
            Scheme::Dark => "dark",
            Scheme::System => return Ok(()),
        };
        page.call("Emulation.setEmulatedMedia", json!({ "features": [{ "name": "prefers-color-scheme", "value": value }] }))?;
        Ok(())
    };
    let src = Page::open(&cdp)?;
    src.set_viewport(vw, vh, args.dpr)?;
    scheme(&src)?;
    let mut warnings: Vec<String> = Vec::new();
    if !src.navigate(&url, timeout, true)? {
        warnings.push(format!("the page had not finished loading after {}s; captured as it stood", args.timeout));
    }
    if args.wait > 0 {
        std::thread::sleep(Duration::from_millis(args.wait));
    }
    src.eval(AGENT)?;
    let sel = serde_json::to_string(&args.selector)?;
    if args.selector.is_some() {
        let until = Instant::now() + timeout;
        while src.eval(&format!("!!document.querySelector({sel})"))? != Value::Bool(true) {
            if Instant::now() >= until {
                bail!("nothing matches the selector {sel}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let prep = src.eval(&format!("__canon.prepare({sel})"))?;
    if let Some(e) = prep.get("error").and_then(Value::as_str) {
        bail!("{e}");
    }
    let mut cfg = canon_core::agent_config();
    cfg["mark"] = json!(true);
    let snapshot = src.eval_string(&format!("__canon.capture({sel}, {cfg})"))?;
    log.step(&format!("captured ({} of snapshot)", kb(snapshot.len())));

    let opts = Options { simplify: !args.no_simplify, structural: !args.no_reduce, max_trials: args.max_trials, fluid: args.fluid, ..Options::default() };
    let mut eng = Engine::load(&snapshot, opts).map_err(|e| anyhow!(e))?;
    drop(snapshot);

    // The original is photographed now, in the same instant it was measured: a page that is
    // still changing should not get the chance to change in between.
    let b = eng.source_bounds();
    let mut clip = [b[0].floor(), b[1].floor(), (b[0] + b[2]).ceil() - b[0].floor(), (b[1] + b[3]).ceil() - b[1].floor()];
    clip[2] = clip[2].max(1.0);
    clip[3] = clip[3].max(1.0);
    if clip[2] > MAX_SHOT || clip[3] > MAX_SHOT {
        warnings.push(format!("the element is larger than {MAX_SHOT} px; only that much of it is compared pixel by pixel"));
        clip[2] = clip[2].min(MAX_SHOT);
        clip[3] = clip[3].min(MAX_SHOT);
    }
    let mut source_image: Option<pixel::Image> = None;
    if !args.no_verify {
        let bg = eng.backdrop_css();
        src.eval(&format!("__canon.isolate({})", Value::String(bg)))?;
        let scroll = eng.info.scroll;
        let doc_clip = [(clip[0] + scroll.0).max(0.0), (clip[1] + scroll.1).max(0.0), clip[2], clip[3]];
        let beyond = clip[0] < 0.0 || clip[1] < 0.0 || clip[0] + clip[2] > vw as f64 || clip[1] + clip[3] > vh as f64;
        let png = src.screenshot(doc_clip, beyond)?;
        if let Some(p) = &args.screenshot {
            std::fs::write(p, &png).with_context(|| format!("cannot write {}", p.display()))?;
        }
        source_image = Some(pixel::decode(&png)?);
        log.step("original photographed");
    }

    // Generated content whose rules the page agent could not read (a stylesheet from
    // another origin): ask the browser instead, as long as that stays cheap.
    if eng.info.pseudo_unknown > 0 && eng.info.pseudo_unknown <= 150 {
        match pseudo_rules(&src) {
            Ok(found) => {
                for (node, which, names) in found {
                    eng.set_pseudo_declared(node, which, names);
                }
            }
            Err(e) => warnings.push(format!("could not read the rules of generated content ({e:#}); its sizes are written out in pixels")),
        }
    }

    if eng.info.pseudo_unknown > 150 {
        warnings.push(format!("{} generated-content boxes are styled from stylesheets that cannot be read; their sizes are written out in pixels", eng.info.pseudo_unknown));
    }

    // ------------------------------------------------------------ fonts
    let loaded: Vec<fonts::Loaded> = serde_json::from_value(src.eval("__canon.loadedFonts()")?).unwrap_or_default();
    let mut font_bytes = 0;
    if !loaded.is_empty() {
        let mut faces = Vec::new();
        for (text, base) in fonts::stylesheets(&src, &eng.info.url)? {
            faces.extend(fonts::extract(&text, &base));
        }
        log.step(&format!("{} @font-face rule(s) found", faces.len()));
        let need = fonts::needed(&faces, &loaded);
        let mut css = String::new();
        let mut failed = 0;
        for f in &need {
            let mut fetch = |u: &str| {
                let r = fonts::fetch_b64(&src, u);
                if r.is_none() {
                    failed += 1;
                }
                r
            };
            if let Some(rule) = fonts::rule(f, !args.no_embed_fonts, &mut fetch) {
                css.push_str(&rule);
            }
        }
        if failed > 0 {
            warnings.push(format!("{failed} web font file(s) could not be inlined and are referenced by URL"));
        }
        if need.is_empty() {
            warnings.push("the page uses web fonts that have no @font-face rule (added from script); text may fall back".into());
        }
        font_bytes = css.len();
        eng.font_css = css;
        log.step(&format!("{} web font face(s) carried over ({})", need.len(), kb(font_bytes)));
    }

    // ------------------------------------------------------------ replica
    let harness = tmp.path().join("stage.html");
    let doctype = eng.doctype();
    std::fs::write(&harness, format!("{doctype}<html><head><meta charset=\"utf-8\"></head><body></body></html>"))?;
    let rep_page = Page::open(&cdp)?;
    rep_page.set_viewport(vw, vh, args.dpr)?;
    scheme(&rep_page)?;
    rep_page.navigate(&file_url(&harness)?, timeout, false)?;
    rep_page.eval(AGENT)?;
    let init = json!({ "names": eng.t.props, "opaque": cfg["opaque"] }).to_string();
    rep_page.eval(&format!("__canon.init({init})"))?;

    let keys = serde_json::to_string(eng.probe_keys())?;
    eng.set_probe(&rep_page.eval_string(&format!("__canon.probe({keys})"))?).map_err(|e| anyhow!(e))?;
    let tests = serde_json::to_string(&eng.inherit_tests())?;
    eng.set_inherit(&rep_page.eval_string(&format!("__canon.inherit({tests})"))?).map_err(|e| anyhow!(e))?;
    let (classes, _) = eng.follow_request();
    let classes = serde_json::to_string(classes)?;
    eng.set_follow(&rep_page.eval_string(&format!("__canon.follow({classes}, {tests})"))?).map_err(|e| anyhow!(e))?;
    eng.prepare().map_err(|e| anyhow!(e))?;
    log.step(&format!("defaults measured for {} kinds of element", eng.probe_keys().len()));

    let mut rep = Replica {
        page: rep_page,
        reference: None,
        clip,
        src_origin: (eng.info.rect[0], eng.info.rect[1]),
        viewport: (vw as f64, vh as f64),
        tolerance: args.tolerance,
        shots: 0,
        misplaced: false,
        last: None,
        deadline: None,
        scale: args.dpr,
    };

    // ------------------------------------------------------------ faithful copy
    let baseline = eng.calibrate(&mut rep).map_err(|e| anyhow!(e))?;
    log.step(&format!("faithful copy laid out ({} node(s) off)", baseline.len()));
    if args.verbose && !baseline.is_empty() {
        for m in baseline.iter().take(8) {
            eprintln!("           off: {} #{} expected {:?} got {:?}", if m.text { "text" } else { "box" }, m.index, m.expected, m.got);
        }
    }

    let mut fidelity: Option<pixel::Diff> = None;
    if let Some(src_img) = &source_image {
        let bg = eng.backdrop_css();
        let (_, html, css) = eng.trial_markup();
        let (x, y) = eng.target_origin();
        let doc = Doc { html: &html, css: &css, x, y, w: eng.stage_width(), bg: &bg, region: None, tree: None };
        let m = rep.render(&doc).map_err(|e| anyhow!(e))?;
        let copy_img = rep.shoot((x, y), (x, y), &m)?;
        let fid = pixel::diff(src_img, &copy_img, args.tolerance);
        log.step(&format!("faithful copy photographed: {} of {} px differ", fid.different, fid.total));
        // Reductions are judged against the original when the faithful copy matches it,
        // and against the faithful copy otherwise.
        rep.reference = Some(if fid.exact() { src_img.clone() } else { copy_img });
        fidelity = Some(fid);
    }

    // ------------------------------------------------------------ reduce
    let identity = eng.work.clone();
    let before = eng.source_elems;
    eng.opts.strict = args.strict;
    rep.deadline = Some(Instant::now() + Duration::from_secs(args.time_limit.max(1)));
    eng.reduce(&mut rep).map_err(|e| anyhow!(e))?;
    log.step(&format!("reduced: {} trials, {} screenshots", eng.stats.trials, rep.shots));
    if args.verbose {
        let mut tally: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for t in &eng.trace {
            *tally.entry(t.split(" #").next().unwrap_or(t).to_string()).or_insert(0) += 1;
        }
        let line: Vec<String> = tally.iter().map(|(k, v)| format!("{k} x{v}")).collect();
        eprintln!("           edits: {}", line.join(", "));
        if std::env::var_os("CANON_TRACE").is_some() {
            for t in &eng.trace {
                eprintln!("           edit: {t}");
            }
        }
    }

    // ------------------------------------------------------------ emit and verify
    let mode = match args.style {
        StyleArg::Auto => StyleMode::Auto,
        StyleArg::Inline => StyleMode::Inline,
        StyleArg::Classes => StyleMode::Classes,
    };
    // The CSS-level savings (declaration placement, shorthands) are each checked against the
    // browser; one that does not survive is dropped rather than trusted.
    let mut repairs = 0;
    let (fin, verdict) = loop {
        let w = eng.work.clone();
        let mut best: Option<(canon_core::Final, Verdict)> = None;
        for (hoist, fold) in [(true, true), (true, false), (false, true), (false, false)] {
            if hoist && !eng.opts.hoist {
                continue;
            }
            let keep = eng.opts.hoist;
            eng.opts.hoist = hoist;
            let plan = eng.plan_final(&w);
            eng.opts.hoist = keep;
            let fin = eng.render_final(&plan, mode, fold, &args.class_prefix);
            let v = verify(&mut eng, &mut rep, &plan, &fin.document, tmp.path(), &init)?;
            let clean = v.ok() && v.styles.is_empty();
            if !clean {
                log.step(&format!(
                    "placement={hoist} shorthands={fold}: {} box(es) off, {} computed value(s) off, pixels {}",
                    v.geometry.len(),
                    v.styles.len(),
                    v.pixels.map_or("not compared".to_string(), |d| format!("{} differ", d.different))
                ));
            }
            if !clean && args.verbose && std::env::var_os("CANON_TRACE").is_some() {
                for m in v.geometry.iter().take(6) {
                    eprintln!("           off: {} #{} node {:?} expected {:?} got {:?}", if m.text { "text" } else { "box" }, m.index, m.nodes, m.expected, m.got);
                }
                let mut seen = std::collections::BTreeMap::new();
                for sm in &v.styles {
                    *seen.entry(format!("<{}> {}: wanted `{}`, got `{}`", sm.tag, sm.property, sm.expected, sm.got)).or_insert(0usize) += 1;
                }
                for (k, n) in seen.iter().take(14) {
                    eprintln!("           style x{n}: {k}");
                }
            }
            let better = match &best {
                None => true,
                Some((_, b)) => (v.ok() && !b.ok()) || (v.ok() == b.ok() && v.styles.len() < b.styles.len()),
            };
            if better {
                best = Some((fin, v));
            }
            if clean {
                break;
            }
        }
        let (fin, v) = best.expect("at least one variant is tried");
        if v.ok() {
            break (fin, v);
        }
        // Before blaming the reduction: does the *unreduced* copy still look like the reference?
        // If not, the page does not render the same way twice (video, script-driven motion).
        let mut stable = true;
        if rep.reference.is_some() && v.structure_ok && v.geometry.is_empty() {
            let (_, html, css) = eng.markup_of(&identity);
            let (x, y) = eng.target_origin();
            let bg = eng.backdrop_css();
            let doc = Doc { html: &html, css: &css, x, y, w: eng.stage_width(), bg: &bg, region: None, tree: None };
            let m = rep.render(&doc).map_err(|e| anyhow!(e))?;
            stable = rep.accept(&doc, &m).map_err(|e| anyhow!(e))? != Some(false);
            if !stable {
                warnings.push("the page does not render the same way twice (video or script-driven motion); differences cannot be attributed".into());
            }
        }
        if stable && rep.reference.is_some() && eng.opts.structural && repairs < 3 {
            repairs += 1;
            rep.deadline = Some(Instant::now() + Duration::from_secs(args.time_limit.clamp(1, 60)));
            log.step("the reduced tree lays out the same but does not look the same; finding the edits responsible");
            match eng.repair(&identity, &mut rep) {
                Ok(n) if n > 0 => {
                    log.step(&format!("{n} edit(s) withdrawn"));
                    if args.verbose {
                        for t in eng.trace.iter().filter(|t| t.starts_with("withdrawn")) {
                            eprintln!("           {t}");
                        }
                    }
                    continue;
                }
                Ok(_) => {}
                Err(e) => warnings.push(e),
            }
        }
        break (fin, v);
    };
    log.step("verified");

    // ------------------------------------------------------------ output
    let out_text = if args.fragment {
        let mut s = String::new();
        if !eng.font_css.is_empty() || !fin.css.is_empty() {
            s.push_str("<style>\n");
            s.push_str(&eng.font_css);
            s.push_str(&fin.css);
            s.push_str("</style>\n");
        }
        s.push_str(&fin.html);
        s.push('\n');
        s
    } else {
        fin.document.clone()
    };
    match &args.output {
        Some(p) => std::fs::write(p, &out_text).with_context(|| format!("cannot write {}", p.display()))?,
        None => std::io::stdout().write_all(out_text.as_bytes())?,
    }

    if let (Some(dir), Some(a), Some(b)) = (std::env::var_os("CANON_DUMP"), &rep.reference, &verdict.image) {
        // Debugging aid: where the final document differs from the reference it was judged against.
        std::fs::write(Path::new(&dir).join("diff-vs-reference.png"), pixel::diff_png(a, b, args.tolerance)?)?;
        std::fs::write(Path::new(&dir).join("reference.png"), pixel::encode(a)?)?;
        std::fs::write(Path::new(&dir).join("final.png"), pixel::encode(b)?)?;
    }
    // The final picture is compared with the original itself whenever the two can be compared.
    let vs_source = match (&source_image, &verdict.image) {
        (Some(a), Some(b)) => Some(pixel::diff(a, b, args.tolerance)),
        _ => None,
    };
    if let Some(p) = &args.screenshot_result {
        match &verdict.image {
            Some(img) => std::fs::write(p, pixel::encode(img)?).with_context(|| format!("cannot write {}", p.display()))?,
            None => warnings.push("no screenshot of the result was taken (--no-verify, or the result did not parse as written)".into()),
        }
    }
    if let (Some(p), Some(a), Some(b)) = (&args.diff, &source_image, &verdict.image) {
        if !vs_source.is_some_and(|d| d.exact()) {
            std::fs::write(p, pixel::diff_png(a, b, args.tolerance)?).with_context(|| format!("cannot write {}", p.display()))?;
        }
    }
    warnings.extend(eng.warnings.iter().cloned());
    if rep.misplaced {
        warnings.push("the copy could not be placed at the original's sub-pixel offset; edge pixels may differ".into());
    }
    if !verdict.structure_ok {
        warnings.push("the HTML parser rebuilt the result differently from what was written".into());
    }
    if !verdict.styles.is_empty() {
        warnings.push(format!("{} computed value(s) read back differently from their target", verdict.styles.len()));
    }
    let geometry_exact = verdict.structure_ok && verdict.geometry.is_empty() && eng.stats.baseline_mismatches == 0;
    let exact = match vs_source {
        Some(d) => d.exact() && verdict.structure_ok && verdict.geometry.is_empty(),
        None => geometry_exact,
    };
    let status = match vs_source {
        Some(d) if d.exact() && verdict.geometry.is_empty() && args.tolerance == 0 => format!("pixel-identical ({} px compared)", d.total),
        Some(d) if d.exact() && verdict.geometry.is_empty() => {
            format!("identical within a per-channel tolerance of {} ({} px compared, largest difference {})", args.tolerance, d.total, d.max_delta)
        }
        Some(d) => {
            let from_copy = fidelity.is_some_and(|f| !f.exact());
            format!(
                "{} of {} px differ (max channel delta {}){}",
                d.different,
                d.total,
                d.max_delta,
                if from_copy && verdict.ok() { " - already present in the unreduced copy; the reduction itself changed nothing" } else { "" }
            )
        }
        None if geometry_exact => "layout identical (pixels not compared)".to_string(),
        None => format!("{} node(s) laid out differently (pixels not compared)", verdict.geometry.len() + eng.stats.baseline_mismatches),
    };

    if !args.quiet {
        let after = fin.elements;
        let pct = if before > 0 { 100.0 * (before as f64 - after as f64) / before as f64 } else { 0.0 };
        eprintln!(
            "canon: {before} -> {after} elements ({pct:.0}% fewer), {} declarations, {} of markup -> {} self-contained{}",
            fin.declarations,
            kb(eng.info.source_bytes),
            kb(out_text.len()),
            if font_bytes > 0 { format!(" ({} of it fonts)", kb(font_bytes)) } else { String::new() },
        );
        eprintln!("canon: {status}; {} layout trials in {:.1}s", eng.stats.trials, log.t0.elapsed().as_secs_f64());
        for w in &warnings {
            eprintln!("canon: note: {w}");
        }
        if args.verbose {
            for m in verdict.geometry.iter().take(8) {
                eprintln!("           off: {} #{} expected {:?} got {:?}", if m.text { "text" } else { "box" }, m.index, m.expected, m.got);
            }
            for s in verdict.styles.iter().take(12) {
                eprintln!("           style: <{}> #{} {}: wanted `{}`, got `{}`", s.tag, s.element, s.property, s.expected, s.got);
            }
        }
    }

    if let Some(p) = &args.report {
        let diff_json = |d: Option<pixel::Diff>| d.map(|d| json!({ "different": d.different, "total": d.total, "maxDelta": d.max_delta, "exact": d.exact() }));
        let report = json!({
            "input": args.input,
            "url": eng.info.url,
            "selector": args.selector,
            "rootTag": eng.info.root_tag,
            "viewport": [vw, vh],
            "dpr": args.dpr,
            "elements": { "dom": eng.info.dom_elements, "rendered": before, "output": fin.elements },
            "declarations": fin.declarations,
            "bytes": { "sourceMarkup": eng.info.source_bytes, "output": out_text.len(), "fonts": font_bytes },
            "edits": eng.stats,
            "screenshots": rep.shots,
            "verification": {
                "exact": exact,
                "structure": verdict.structure_ok,
                "geometryMismatches": verdict.geometry.len(),
                "baselineMismatches": eng.stats.baseline_mismatches,
                "styleMismatches": verdict.styles.len(),
                "pixelsVsOriginal": diff_json(vs_source),
                "unreducedCopyVsOriginal": diff_json(fidelity),
                "pixelsVsReference": diff_json(verdict.pixels),
            },
            "warnings": warnings,
            "seconds": log.t0.elapsed().as_secs_f64(),
        });
        std::fs::write(p, serde_json::to_string_pretty(&report)?).with_context(|| format!("cannot write {}", p.display()))?;
    }
    Ok(if exact { 0 } else { 2 })
}
