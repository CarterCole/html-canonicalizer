//! Box geometry in Blink layout units (1/64 CSS px), as reported by the page agent.

pub const UNIT: f64 = 64.0;

/// x, y, width, height.
pub type Rect = [i32; 4];
pub type Rects = Vec<Rect>;

pub fn from_flat(v: &[i32]) -> Rects {
    v.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
}

pub fn bounds(rs: &[Rect]) -> Option<Rect> {
    let mut it = rs.iter();
    let f = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (f[0], f[1], f[0] + f[2], f[1] + f[3]);
    for r in it {
        x0 = x0.min(r[0]);
        y0 = y0.min(r[1]);
        x1 = x1.max(r[0] + r[2]);
        y1 = y1.max(r[1] + r[3]);
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

pub fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => bounds(&[a, b]),
        (a, None) => a,
        (None, b) => b,
    }
}

pub fn has_area(rs: &[Rect]) -> bool {
    rs.iter().any(|r| r[2] > 0 && r[3] > 0)
}

pub fn translate(rs: &[Rect], dx: i32, dy: i32) -> Rects {
    rs.iter().map(|r| [r[0] - dx, r[1] - dy, r[2], r[3]]).collect()
}

/// Canonical form of a run of text fragments: zero-width fragments dropped, and fragments
/// that touch on the same line joined. A text node split in two and the same text in one
/// node then compare equal.
pub fn merge_lines(rs: &[Rect], tol: i32) -> Rects {
    let mut out: Rects = Vec::with_capacity(rs.len());
    for r in rs {
        if r[2] <= 0 {
            continue;
        }
        if let Some(last) = out.last_mut() {
            let same_line = (last[1] - r[1]).abs() <= tol && (last[3] - r[3]).abs() <= tol;
            let touches = (last[0] + last[2] - r[0]).abs() <= tol;
            if same_line && touches {
                last[2] = r[0] + r[2] - last[0];
                continue;
            }
        }
        out.push(*r);
    }
    out
}

pub fn eq(a: &[Rect], b: &[Rect], tol: i32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (0..4).all(|i| (x[i] - y[i]).abs() <= tol))
}

/// Largest edge difference between two rect lists, in layout units (`None` if the counts differ).
pub fn max_delta(a: &[Rect], b: &[Rect]) -> Option<i32> {
    if a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).flat_map(|(x, y)| (0..4).map(move |i| (x[i] - y[i]).abs())).max().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_touching_fragments_on_one_line() {
        let rs = vec![[0, 0, 640, 1152], [640, 0, 320, 1152], [0, 1152, 100, 1152]];
        assert_eq!(merge_lines(&rs, 1), vec![[0, 0, 960, 1152], [0, 1152, 100, 1152]]);
    }

    #[test]
    fn drops_zero_width_fragments() {
        let rs = vec![[0, 0, 0, 1152], [0, 0, 640, 1152]];
        assert_eq!(merge_lines(&rs, 1), vec![[0, 0, 640, 1152]]);
    }

    #[test]
    fn does_not_merge_across_a_gap() {
        let rs = vec![[0, 0, 640, 1152], [700, 0, 320, 1152]];
        assert_eq!(merge_lines(&rs, 1).len(), 2);
    }

    #[test]
    fn tolerance() {
        assert!(eq(&[[0, 0, 10, 10]], &[[1, 0, 10, 9]], 1));
        assert!(!eq(&[[0, 0, 10, 10]], &[[2, 0, 10, 10]], 1));
        assert_eq!(bounds(&[[0, 0, 10, 10], [5, 5, 20, 2]]), Some([0, 0, 25, 10]));
    }
}
