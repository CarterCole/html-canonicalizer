//! The small amount of CSS knowledge the reducer carries.
//!
//! Whether a property inherits, what a bare element's defaults are, and which properties
//! default to `currentcolor` are all *measured* in the browser (see `baseline`). What is
//! written down here is only: which properties cannot change a static picture, which tags
//! have behaviour CSS cannot reproduce, and which properties are pure layout (used to pick
//! candidates - every candidate is still checked against the real render).

use crate::style::Table;

/// Properties that cannot affect a static render. Never captured.
pub const SKIP_EXACT: &[&str] = &[
    "cursor",
    "pointer-events",
    "user-select",
    "-webkit-user-drag",
    "-webkit-user-modify",
    "touch-action",
    "scroll-behavior",
    "overflow-anchor",
    "caret-color",
    "caret-animation",
    "caret-shape",
    "-webkit-tap-highlight-color",
    "interactivity",
    "speak",
    "app-region",
    "window-drag",
    "timeline-scope",
    "trigger-scope",
    "container-name",
    "reading-flow",
    "reading-order",
    "scroll-marker-group",
    "scroll-initial-target",
    "scroll-target-group",
    "scroll-axis-lock",
    "overlay",
    "interpolate-size",
    "buffered-rendering",
    "forced-color-adjust",
    "print-color-adjust",
    // Reflects decorations propagated from ancestors; not settable.
    "-webkit-text-decorations-in-effect",
    // Shorthands and aliases Chrome enumerates next to the longhands they stand for.
    "text-decoration",
    "font-variant",
    "contain-intrinsic-size",
    "-webkit-border-image",
    "-webkit-mask-box-image",
    "-webkit-text-combine",
    "-webkit-text-orientation",
    "-webkit-writing-mode",
    "-webkit-ruby-position",
    "-webkit-line-break",
    "-webkit-box-decoration-break",
    "-webkit-mask-position-x",
    "-webkit-mask-position-y",
];

pub const SKIP_PREFIX: &[&str] = &[
    "animation-",
    "transition-",
    "scroll-snap-",
    "scroll-margin-",
    "scroll-padding-",
    "scroll-timeline-",
    "view-timeline-",
    "view-transition-",
    "overscroll-behavior-",
    "timeline-trigger-",
    "interest-delay-",
];

/// HTML tags that are kept as themselves because CSS on a `<div>` cannot stand in for them,
/// with the attributes that are carried over.
pub const KEEP_HTML: &[(&str, &[&str])] = &[
    ("img", &["src", "alt", "width", "height"]),
    ("svg", &[]),
    ("math", &[]),
    ("video", &["src", "poster", "width", "height", "controls"]),
    ("audio", &["src", "controls"]),
    ("canvas", &["width", "height"]),
    ("iframe", &["src", "srcdoc", "width", "height"]),
    ("object", &["data", "type", "width", "height"]),
    ("embed", &["src", "type", "width", "height"]),
    ("input", &["type", "value", "checked", "placeholder", "size", "disabled", "min", "max", "step", "multiple"]),
    ("textarea", &["rows", "cols", "placeholder", "disabled"]),
    ("select", &["multiple", "size", "disabled"]),
    ("button", &["disabled"]),
    ("progress", &["value", "max"]),
    ("meter", &["value", "min", "max", "low", "high", "optimum"]),
    ("br", &[]),
    ("wbr", &[]),
    ("table", &[]),
    ("caption", &[]),
    ("colgroup", &["span"]),
    ("col", &["span"]),
    ("thead", &[]),
    ("tbody", &[]),
    ("tfoot", &[]),
    ("tr", &[]),
    ("td", &["colspan", "rowspan"]),
    ("th", &["colspan", "rowspan"]),
    ("fieldset", &["disabled"]),
    ("legend", &[]),
];

/// Kept tags whose children are not walked: they render from attributes or state.
pub const OPAQUE_HTML: &[&str] = &[
    "img", "video", "audio", "canvas", "iframe", "object", "embed", "input", "textarea", "select", "progress", "meter", "br",
    "wbr", "col",
];

pub const VOID_HTML: &[&str] = &["img", "input", "br", "wbr", "col", "embed"];

/// Attributes that carry content, not style: left out of the default-style probe key.
pub const CONTENT_ATTRS: &[&str] = &["src", "alt", "value", "placeholder", "poster", "data", "srcdoc", "href", "xlink:href"];

pub fn is_keep(tag: &str) -> bool {
    KEEP_HTML.iter().any(|(t, _)| *t == tag)
}

pub fn is_opaque(tag: &str) -> bool {
    OPAQUE_HTML.contains(&tag)
}

pub fn is_void(tag: &str) -> bool {
    VOID_HTML.contains(&tag)
}

/// Where a table part must sit for the HTML parser to leave it alone.
pub fn required_parent(tag: &str) -> Option<&'static [&'static str]> {
    Some(match tag {
        "caption" | "colgroup" | "thead" | "tbody" | "tfoot" => &["table"],
        "tr" => &["thead", "tbody", "tfoot"],
        "td" | "th" => &["tr"],
        "col" => &["colgroup"],
        "legend" => &["fieldset"],
        _ => return None,
    })
}

/// Ancestors a bare element of this tag is created under when its defaults are measured.
pub fn probe_wrappers(tag: &str) -> &'static [&'static str] {
    match tag {
        "caption" | "colgroup" | "thead" | "tbody" | "tfoot" => &["table"],
        "tr" => &["table", "tbody"],
        "td" | "th" => &["table", "tbody", "tr"],
        "col" => &["table", "colgroup"],
        "legend" => &["fieldset"],
        _ => &[],
    }
}

pub const F_LAYOUT: u8 = 1; // affects layout only: never paints, clips or creates an effect
pub const F_OUTER: u8 = 2; // how a box takes part in its parent's layout
pub const F_CONTAINER: u8 = 4; // how a box lays out its children
pub const F_SIZE: u8 = 8;

const LAYOUT_ONLY: &[&str] = &[
    "display",
    "position",
    "top",
    "right",
    "bottom",
    "left",
    "float",
    "clear",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "box-sizing",
    "flex-basis",
    "flex-grow",
    "flex-shrink",
    "flex-direction",
    "flex-wrap",
    "order",
    "align-content",
    "align-items",
    "align-self",
    "justify-content",
    "justify-items",
    "justify-self",
    "row-gap",
    "column-gap",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-column-end",
    "grid-column-start",
    "grid-row-end",
    "grid-row-start",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "vertical-align",
    "table-layout",
    "aspect-ratio",
    "unicode-bidi",
];

const OUTER: &[&str] = &[
    "position",
    "top",
    "right",
    "bottom",
    "left",
    "float",
    "clear",
    "flex-basis",
    "flex-grow",
    "flex-shrink",
    "order",
    "align-self",
    "justify-self",
    "grid-column-end",
    "grid-column-start",
    "grid-row-end",
    "grid-row-start",
    "vertical-align",
];

const CONTAINER: &[&str] = &[
    "flex-direction",
    "flex-wrap",
    "align-content",
    "align-items",
    "justify-content",
    "justify-items",
    "row-gap",
    "column-gap",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
];

const SIZE: &[&str] = &["width", "height", "min-width", "min-height", "max-width", "max-height"];

pub fn flags(t: &Table) -> Vec<u8> {
    let mut f = vec![0u8; t.nprops()];
    for (list, bit) in [(LAYOUT_ONLY, F_LAYOUT), (OUTER, F_OUTER), (CONTAINER, F_CONTAINER), (SIZE, F_SIZE)] {
        for name in list {
            if let Some(p) = t.prop(name) {
                f[p] |= bit;
            }
        }
    }
    f
}

pub const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];
pub const CORNERS: [&str; 4] = ["top-left", "top-right", "bottom-right", "bottom-left"];

/// Ids of the properties the reducer reads by name. `None` when this browser lacks one.
#[derive(Debug, Default, Clone)]
pub struct P {
    pub display: Option<usize>,
    pub position: Option<usize>,
    pub inset: [Option<usize>; 4],
    pub float: Option<usize>,
    pub clear: Option<usize>,
    pub z_index: Option<usize>,
    pub visibility: Option<usize>,
    pub color: Option<usize>,
    pub opacity: Option<usize>,
    pub margin: [Option<usize>; 4],
    pub padding: [Option<usize>; 4],
    pub border_style: [Option<usize>; 4],
    pub border_width: [Option<usize>; 4],
    pub border_color: [Option<usize>; 4],
    pub border_image_source: Option<usize>,
    pub radius: [Option<usize>; 4],
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub min_width: Option<usize>,
    pub min_height: Option<usize>,
    pub max_width: Option<usize>,
    pub max_height: Option<usize>,
    pub box_sizing: Option<usize>,
    pub aspect_ratio: Option<usize>,
    pub flex_basis: Option<usize>,
    pub flex_grow: Option<usize>,
    pub flex_shrink: Option<usize>,
    pub flex_direction: Option<usize>,
    pub flex_wrap: Option<usize>,
    pub order: Option<usize>,
    pub align_items: Option<usize>,
    pub align_self: Option<usize>,
    pub justify_content: Option<usize>,
    pub justify_items: Option<usize>,
    pub justify_self: Option<usize>,
    pub row_gap: Option<usize>,
    pub column_gap: Option<usize>,
    pub column_count: Option<usize>,
    pub column_width: Option<usize>,
    pub grid_container: Vec<usize>,
    pub grid_item: Vec<usize>,
    pub vertical_align: Option<usize>,
    pub overflow_x: Option<usize>,
    pub overflow_y: Option<usize>,
    pub overflow_clip_margin: Option<usize>,
    pub text_overflow: Option<usize>,
    pub resize: Option<usize>,
    pub outline_style: Option<usize>,
    pub outline_width: Option<usize>,
    pub outline_color: Option<usize>,
    pub outline_offset: Option<usize>,
    pub td_line: Option<usize>,
    pub td_color: Option<usize>,
    pub td_style: Option<usize>,
    pub td_thickness: Option<usize>,
    pub transform: Option<usize>,
    pub rotate: Option<usize>,
    pub scale: Option<usize>,
    pub translate: Option<usize>,
    pub offset_path: Option<usize>,
    pub transform_origin: Option<usize>,
    pub perspective: Option<usize>,
    pub perspective_origin: Option<usize>,
    pub bg_color: Option<usize>,
    pub bg_image: Option<usize>,
    pub bg_layer: Vec<usize>,
    pub bg_clip: Option<usize>,
    pub box_shadow: Option<usize>,
    pub backdrop_filter: Option<usize>,
    pub object_fit: Option<usize>,
    pub object_position: Option<usize>,
    pub unicode_bidi: Option<usize>,
    pub direction: Option<usize>,
    pub writing_mode: Option<usize>,
    pub zoom: Option<usize>,
    pub list_style_type: Option<usize>,
    pub list_style_image: Option<usize>,
    pub white_space_collapse: Option<usize>,
    pub content: Option<usize>,
    pub line_height: Option<usize>,
    pub border_image: Vec<usize>,
    pub mask_image: Option<usize>,
    pub mask_layer: Vec<usize>,
}

impl P {
    pub fn new(t: &Table) -> P {
        let g = |n: &str| t.prop(n);
        let four = |pre: &str, post: &str| -> [Option<usize>; 4] {
            let mut a = [None; 4];
            for (i, s) in SIDES.iter().enumerate() {
                a[i] = t.prop(&format!("{pre}{s}{post}"));
            }
            a
        };
        let list = |names: &[&str]| -> Vec<usize> { names.iter().filter_map(|n| t.prop(n)).collect() };
        let mut radius = [None; 4];
        for (i, c) in CORNERS.iter().enumerate() {
            radius[i] = t.prop(&format!("border-{c}-radius"));
        }
        P {
            display: g("display"),
            position: g("position"),
            inset: [g("top"), g("right"), g("bottom"), g("left")],
            float: g("float"),
            clear: g("clear"),
            z_index: g("z-index"),
            visibility: g("visibility"),
            color: g("color"),
            opacity: g("opacity"),
            margin: four("margin-", ""),
            padding: four("padding-", ""),
            border_style: four("border-", "-style"),
            border_width: four("border-", "-width"),
            border_color: four("border-", "-color"),
            border_image_source: g("border-image-source"),
            radius,
            width: g("width"),
            height: g("height"),
            min_width: g("min-width"),
            min_height: g("min-height"),
            max_width: g("max-width"),
            max_height: g("max-height"),
            box_sizing: g("box-sizing"),
            aspect_ratio: g("aspect-ratio"),
            flex_basis: g("flex-basis"),
            flex_grow: g("flex-grow"),
            flex_shrink: g("flex-shrink"),
            flex_direction: g("flex-direction"),
            flex_wrap: g("flex-wrap"),
            order: g("order"),
            align_items: g("align-items"),
            align_self: g("align-self"),
            justify_content: g("justify-content"),
            justify_items: g("justify-items"),
            justify_self: g("justify-self"),
            row_gap: g("row-gap"),
            column_gap: g("column-gap"),
            column_count: g("column-count"),
            column_width: g("column-width"),
            grid_container: list(&[
                "grid-auto-columns",
                "grid-auto-flow",
                "grid-auto-rows",
                "grid-template-areas",
                "grid-template-columns",
                "grid-template-rows",
            ]),
            grid_item: list(&["grid-column-end", "grid-column-start", "grid-row-end", "grid-row-start"]),
            vertical_align: g("vertical-align"),
            overflow_x: g("overflow-x"),
            overflow_y: g("overflow-y"),
            overflow_clip_margin: g("overflow-clip-margin"),
            text_overflow: g("text-overflow"),
            resize: g("resize"),
            outline_style: g("outline-style"),
            outline_width: g("outline-width"),
            outline_color: g("outline-color"),
            outline_offset: g("outline-offset"),
            td_line: g("text-decoration-line"),
            td_color: g("text-decoration-color"),
            td_style: g("text-decoration-style"),
            td_thickness: g("text-decoration-thickness"),
            transform: g("transform"),
            rotate: g("rotate"),
            scale: g("scale"),
            translate: g("translate"),
            offset_path: g("offset-path"),
            transform_origin: g("transform-origin"),
            perspective: g("perspective"),
            perspective_origin: g("perspective-origin"),
            bg_color: g("background-color"),
            bg_image: g("background-image"),
            bg_layer: list(&[
                "background-position",
                "background-size",
                "background-repeat",
                "background-attachment",
                "background-origin",
                "background-blend-mode",
            ]),
            bg_clip: g("background-clip"),
            box_shadow: g("box-shadow"),
            backdrop_filter: g("backdrop-filter"),
            object_fit: g("object-fit"),
            object_position: g("object-position"),
            unicode_bidi: g("unicode-bidi"),
            direction: g("direction"),
            writing_mode: g("writing-mode"),
            zoom: g("zoom"),
            list_style_type: g("list-style-type"),
            list_style_image: g("list-style-image"),
            white_space_collapse: g("white-space-collapse"),
            content: g("content"),
            line_height: g("line-height"),
            border_image: list(&["border-image-outset", "border-image-repeat", "border-image-slice", "border-image-width"]),
            mask_image: g("mask-image"),
            mask_layer: list(&["mask-clip", "mask-composite", "mask-mode", "mask-origin", "mask-position", "mask-repeat", "mask-size"]),
        }
    }
}
