//! Painted toolbar glyphs. The bundled egui fonts don't cover the
//! symbol range — ❖ ⟳ ⌄ 🖨 📋 ⌁ used to render as tofu squares — so
//! every icon here is stroked/filled shapes drawn at paint time, in the
//! same spirit as `cite::cite_button` and `theme_toggle`.

use egui::epaint::CubicBezierShape;
use egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

fn st(w: f32, col: Color32) -> Stroke {
    Stroke::new(w, col)
}

/// The ❖ logo mark: a leaf — two cubic bulges meeting tip-to-stem.
pub fn leaf(p: &Painter, r: Rect, col: Color32) {
    let c = r.center();
    let s = r.width().min(r.height()) * 0.5;
    let tip = pos2(c.x + s * 0.62, c.y - s * 0.78);
    let stem = pos2(c.x - s * 0.62, c.y + s * 0.78);
    p.add(CubicBezierShape::from_points_stroke(
        [stem, pos2(c.x - s * 0.85, c.y - s * 0.5), pos2(c.x + s * 0.02, c.y - s), tip],
        true,
        col,
        Stroke::NONE,
    ));
    p.add(CubicBezierShape::from_points_stroke(
        [stem, pos2(c.x + s * 0.02, c.y + s * 0.92), pos2(c.x + s * 0.9, c.y + s * 0.08), tip],
        true,
        col,
        Stroke::NONE,
    ));
    p.line_segment([stem, tip], st(0.9, col.gamma_multiply(0.45)));
}

/// Circular arrow (recompile, refresh).
pub fn rebuild(p: &Painter, r: Rect, col: Color32) {
    let c = r.center();
    let rad = r.width().min(r.height()) * 0.34;
    const N: usize = 20;
    let mut pts = Vec::with_capacity(N + 1);
    for i in 0..=N {
        let a = std::f32::consts::TAU * (0.82 * i as f32 / N as f32) + 0.45;
        pts.push(c + vec2(rad * a.cos(), rad * a.sin()));
    }
    p.add(Shape::line(pts.clone(), st(1.4, col)));
    if let (Some(&prev), Some(&tip)) = (pts.get(N.saturating_sub(1)), pts.last()) {
        let d = (tip - prev).normalized();
        let n = vec2(-d.y, d.x);
        p.add(Shape::convex_polygon(vec![tip + d * 3.4, tip + n * 2.4, tip - n * 2.4], col, Stroke::NONE));
    }
}

/// Small down-pointing chevron (dropdown affordance).
pub fn chevron_down(p: &Painter, r: Rect, col: Color32) {
    let c = r.center();
    let w = r.width().min(r.height()) * 0.3;
    let h = w * 0.55;
    p.add(Shape::line(vec![pos2(c.x - w, c.y - h * 0.6), pos2(c.x, c.y + h), pos2(c.x + w, c.y - h * 0.6)], st(1.5, col)));
}

/// Printer glyph (print the PDF).
pub fn printer(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let body = Rect::from_center_size(r.center() + vec2(0.0, s * 0.1), vec2(s * 0.7, s * 0.34));
    // Paper in, body, paper out.
    let paper_in = Rect::from_min_max(pos2(body.left() + s * 0.12, body.top() - s * 0.24), pos2(body.right() - s * 0.12, body.top() + s * 0.04));
    p.rect_stroke(paper_in, s * 0.03, stroke, StrokeKind::Inside);
    p.rect_stroke(body, s * 0.07, stroke, StrokeKind::Inside);
    let tray = Rect::from_min_max(pos2(body.left() + s * 0.16, body.bottom() - s * 0.04), pos2(body.right() - s * 0.16, body.bottom() + s * 0.2));
    p.rect_stroke(tray, s * 0.03, stroke, StrokeKind::Inside);
    p.circle_filled(pos2(body.right() - s * 0.12, body.top() + s * 0.09), s * 0.035, col);
}

/// Page with a folded corner and a plus (new document from template).
pub fn page_plus(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let page = Rect::from_center_size(r.center() - vec2(s * 0.08, 0.0), vec2(s * 0.54, s * 0.68));
    let f = s * 0.16;
    let tl = page.left_top();
    let tr = page.right_top();
    p.add(Shape::line(
        vec![pos2(tl.x, tl.y), pos2(tr.x - f, tl.y), pos2(tr.x, tl.y + f), pos2(tr.x, page.bottom()), pos2(tl.x, page.bottom()), pos2(tl.x, tl.y)],
        stroke,
    ));
    p.add(Shape::line(vec![pos2(tr.x - f, tl.y), pos2(tr.x - f, tl.y + f), pos2(tr.x, tl.y + f)], stroke));
    let pc = pos2(page.right() + s * 0.02, page.bottom() - s * 0.14);
    let e = s * 0.12;
    p.line_segment([pc - vec2(e, 0.0), pc + vec2(e, 0.0)], st(1.5, col));
    p.line_segment([pc - vec2(0.0, e), pc + vec2(0.0, e)], st(1.5, col));
}

/// Blank page with folded corner (new file).
pub fn page(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let page = Rect::from_center_size(r.center(), vec2(s * 0.6, s * 0.74));
    let f = s * 0.16;
    let tl = page.left_top();
    let tr = page.right_top();
    p.add(Shape::line(
        vec![pos2(tl.x, tl.y), pos2(tr.x - f, tl.y), pos2(tr.x, tl.y + f), pos2(tr.x, page.bottom()), pos2(tl.x, page.bottom()), pos2(tl.x, tl.y)],
        stroke,
    ));
    p.add(Shape::line(vec![pos2(tr.x - f, tl.y), pos2(tr.x - f, tl.y + f), pos2(tr.x, tl.y + f)], stroke));
}

/// Plus sign centered in `r`.
fn plus(p: &Painter, c: Pos2, e: f32, col: Color32) {
    p.line_segment([c - vec2(e, 0.0), c + vec2(e, 0.0)], st(1.5, col));
    p.line_segment([c - vec2(0.0, e), c + vec2(0.0, e)], st(1.5, col));
}

/// Page + plus (new file).
pub fn page_new(p: &Painter, r: Rect, col: Color32) {
    page(p, r.shrink2(vec2(r.width() * 0.1, 0.0)), col);
    plus(p, r.center() + vec2(r.width() * 0.18, r.height() * 0.2), r.width() * 0.12, col);
}

/// Folder outline (open file dialog / folder).
pub fn folder(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let c = r.center();
    let w = s * 0.4;
    let h = s * 0.28;
    let tab = s * 0.14;
    let top = c.y - h;
    let bot = c.y + h;
    p.add(Shape::line(
        vec![
            pos2(c.x - w, bot),
            pos2(c.x - w, top),
            pos2(c.x - w + tab, top),
            pos2(c.x - w + tab + s * 0.08, top - s * 0.1),
            pos2(c.x + w * 0.4, top - s * 0.1),
            pos2(c.x + w, top + s * 0.05),
            pos2(c.x + w, bot),
            pos2(c.x - w, bot),
        ],
        stroke,
    ));
}

/// Folder + plus (new folder).
pub fn folder_new(p: &Painter, r: Rect, col: Color32) {
    folder(p, r.shrink2(vec2(r.width() * 0.08, 0.0)), col);
    plus(p, r.center() + vec2(r.width() * 0.22, r.height() * 0.16), r.width() * 0.1, col);
}

/// Folder with an open lid flap (open file…).
pub fn folder_open(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let c = r.center();
    let w = s * 0.4;
    let h = s * 0.28;
    let tab = s * 0.14;
    let top = c.y - h;
    let bot = c.y + h;
    // Back half with tab.
    p.add(Shape::line(
        vec![
            pos2(c.x - w, top + s * 0.06),
            pos2(c.x - w, top),
            pos2(c.x - w + tab, top),
            pos2(c.x - w + tab + s * 0.08, top - s * 0.1),
            pos2(c.x + w * 0.4, top - s * 0.1),
            pos2(c.x + w, top + s * 0.06),
        ],
        stroke,
    ));
    // Front flap, tilted open.
    p.add(Shape::line(
        vec![pos2(c.x - w, top + s * 0.06), pos2(c.x - w - s * 0.06, bot), pos2(c.x + w + s * 0.06, bot), pos2(c.x + w, top + s * 0.06)],
        stroke,
    ));
}

/// Outgoing arrow into a tray (export/save a copy).
pub fn export(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.25, col);
    let c = r.center();
    let shaft = [pos2(c.x, r.top() + s * 0.18), pos2(c.x, r.bottom() - s * 0.3)];
    p.add(Shape::line(shaft.to_vec(), stroke));
    p.add(Shape::line(vec![pos2(c.x - s * 0.16, c.y + s * 0.04), pos2(c.x, c.y + s * 0.2), pos2(c.x + s * 0.16, c.y + s * 0.04)], stroke));
    let tray = Rect::from_min_max(pos2(r.left() + s * 0.22, r.bottom() - s * 0.22), pos2(r.right() - s * 0.22, r.bottom() - s * 0.14));
    p.add(Shape::line(vec![tray.left_bottom(), tray.left_top(), tray.right_top(), tray.right_bottom()], stroke));
}

/// Sliders (settings/preferences).
pub fn settings(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.25, col);
    for (i, knob) in [0.34, 0.66, 0.44].iter().enumerate() {
        let y = r.top() + s * (0.28 + i as f32 * 0.22);
        p.line_segment([pos2(r.left() + s * 0.18, y), pos2(r.right() - s * 0.18, y)], stroke);
        p.circle_stroke(pos2(r.left() + s * *knob, y), s * 0.07, stroke);
    }
}

/// Question mark (keyboard shortcuts) — drawn arc + stem + dot.
pub fn question(p: &Painter, r: Rect, col: Color32) {
    let c = r.center();
    let s = r.width().min(r.height());
    let stroke = st(1.4, col);
    // Hook arc: ~200° sweep, gap at bottom-right where the stem starts.
    let rad = s * 0.2;
    let top = c + vec2(0.0, -s * 0.16);
    let mut pts = Vec::with_capacity(13);
    for i in 0..=12 {
        let a = std::f32::consts::TAU * (i as f32 / 12.0 * 0.62) - std::f32::consts::PI * 0.85;
        pts.push(top + vec2(rad * a.cos(), rad * a.sin()));
    }
    p.add(Shape::line(pts, stroke));
    let stem_bot = pos2(c.x, c.y + s * 0.06);
    p.line_segment([pos2(c.x + rad * 0.2, top.y + rad * 0.9), stem_bot], stroke);
    p.circle_filled(pos2(c.x, c.y + s * 0.28), s * 0.07, col);
}

/// Branch glyph (Git collaboration): trunk plus a diverging branch.
pub fn branch(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.25, col);
    let x = r.center().x;
    let y0 = r.top() + s * 0.18;
    let y1 = r.bottom() - s * 0.18;
    p.line_segment([pos2(x, y0), pos2(x, y1)], stroke);
    let bx = x + s * 0.24;
    let by = y0 + s * 0.18;
    p.line_segment([pos2(x, y0 + s * 0.3), pos2(bx, by)], stroke);
    p.line_segment([pos2(bx, by), pos2(bx, y0 + s * 0.58)], stroke);
    for c in [pos2(x, y0), pos2(bx, by), pos2(x, y1)] {
        p.circle_filled(c, s * 0.075, col);
    }
}

/// Small filled dot (unsaved-changes marker).
pub fn dot(p: &Painter, r: Rect, col: Color32) {
    p.circle_filled(r.center(), r.width().min(r.height()) * 0.22, col);
}

/// Check mark (saved state).
pub fn check(p: &Painter, r: Rect, col: Color32) {
    let c = r.center();
    let s = r.width().min(r.height());
    p.add(Shape::line(
        vec![pos2(c.x - s * 0.28, c.y + s * 0.02), pos2(c.x - s * 0.06, c.y + s * 0.24), pos2(c.x + s * 0.3, c.y - s * 0.22)],
        st(1.5, col),
    ));
}

/// Two overlapping pages (copy path to clipboard).
pub fn copy(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.1, col);
    let c = r.center();
    // Back page, offset up-left.
    let bw = s * 0.28;
    let bh = s * 0.34;
    let bx = c.x - s * 0.18;
    let by = c.y - s * 0.14;
    p.add(Shape::line(vec![pos2(bx - bw, by + bh), pos2(bx - bw, by - bh), pos2(bx + bw, by - bh), pos2(bx + bw, by + bh * 0.2)], stroke));
    // Front page.
    let fx = c.x + s * 0.1;
    let fy = c.y + s * 0.12;
    let fw = s * 0.3;
    let fh = s * 0.36;
    p.rect_stroke(Rect::from_center_size(pos2(fx, fy), vec2(fw * 2.0, fh * 2.0)).shrink(0.0), 1.0, stroke, StrokeKind::Inside);
}

/// Up arrow into a tray (parent directory).
pub fn dir_up(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.3, col);
    let c = r.center();
    let x = c.x;
    let top = c.y - s * 0.34;
    let bot = c.y + s * 0.18;
    p.add(Shape::line(vec![pos2(x, bot), pos2(x, top)], stroke));
    p.add(Shape::line(vec![pos2(x - s * 0.22, top + s * 0.2), pos2(x, top), pos2(x + s * 0.22, top + s * 0.2)], stroke));
    // Tray line.
    p.add(Shape::line(vec![pos2(c.x - s * 0.34, bot + s * 0.14), pos2(c.x + s * 0.34, bot + s * 0.14)], stroke));
}

/// Pencil slashed over a baseline (edit the path).
pub fn pencil(p: &Painter, r: Rect, col: Color32) {
    let s = r.width().min(r.height());
    let stroke = st(1.2, col);
    let c = r.center();
    let a = pos2(c.x - s * 0.32, c.y + s * 0.32);
    let b = pos2(c.x + s * 0.28, c.y - s * 0.28);
    p.add(Shape::line(vec![a, b], st(3.0, col)));
    // Tip.
    let d = (b - a).normalized();
    let n = vec2(-d.y, d.x);
    p.add(Shape::convex_polygon(vec![b + d * s * 0.14, b - n * s * 0.09, b + n * s * 0.09], col, Stroke::NONE));
    // Baseline.
    p.add(Shape::line(vec![pos2(c.x - s * 0.34, c.y + s * 0.4), pos2(c.x + s * 0.34, c.y + s * 0.4)], stroke));
}
