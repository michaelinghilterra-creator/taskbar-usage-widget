//! Draws the widget into a premultiplied BGRA pixel buffer.
//!
//! GDI can't draw text with alpha, so text is drawn white-on-black into a mask
//! and its brightness becomes coverage, then tinted. The Claude mark is drawn
//! analytically (supersampled capsules), so it stays crisp at any DPI.

use crate::http::wide;
use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;

#[derive(Clone, Copy, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

pub struct Palette {
    pub label: Rgba,
    pub value: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
    pub divider: Rgba,
    pub hover: Rgba,
}

pub fn palette(dark: bool) -> Palette {
    let c = |hex: u32, a: f32| {
        Rgba(
            ((hex >> 16) & 255) as f32 / 255.0,
            ((hex >> 8) & 255) as f32 / 255.0,
            (hex & 255) as f32 / 255.0,
            a,
        )
    };
    if dark {
        Palette {
            label: c(0xFFFFFF, 0.62),
            value: c(0xFFFFFF, 1.0),
            warn: c(0xF5A35C, 1.0),
            crit: c(0xFF7B72, 1.0),
            divider: c(0xFFFFFF, 0.22),
            hover: c(0xFFFFFF, 0.07),
        }
    } else {
        Palette {
            label: c(0x000000, 0.52),
            value: c(0x1B1B1B, 1.0),
            warn: c(0xB4500F, 1.0),
            crit: c(0xC42B1C, 1.0),
            divider: c(0x000000, 0.18),
            hover: c(0xFFFFFF, 0.55),
        }
    }
}

const RING_BLUE: Rgba = Rgba(0.298, 0.608, 0.961, 1.0); // #4C9BF5
pub const CLAUDE_ORANGE: Rgba = Rgba(0.851, 0.467, 0.341, 1.0); // #D97757

/// One account's numbers.
#[derive(Clone)]
pub struct Row {
    pub five: Option<f64>,
    pub week: Option<f64>,
    /// Fable's weekly limit; its group only shows when some account has one.
    pub fable: Option<f64>,
    /// A change being animated: the values before it (5h, week, Fable) and progress 0 to 1.
    pub roll: Option<([Option<f64>; 3], f32)>,
    pub stale: bool,
    /// Elapsed fraction marker and projected percent for 5h/week/Fable.
    pub pace: [Option<f32>; 3],
    pub projected: [Option<f32>; 3],
    pub text: [Option<String>; 3],
    pub critical: [bool; 3],
}

/// Most accounts that fit in one column.
pub const MAX_ROWS_PER_COLUMN: usize = 3;
/// The "Default" font size; other sizes scale relative to it.
pub const DEFAULT_FONT_SIZE: u32 = 14;

#[derive(Clone)]
pub struct Model {
    /// One per account, laid out top to bottom, then left to right.
    pub rows: Vec<Row>,
    pub hover: bool,
    pub dark: bool,
    /// Show what's left (100 - used) instead of what's used.
    pub show_left: bool,
    /// Text size in pixels at 100% scaling.
    pub font_size: u32,
    /// Leave out the Claude mark.
    pub hide_icon: bool,
    /// Draw the pixel-art mark instead of the asterisk.
    pub pixel_icon: bool,
    /// Icon size in pixels at 100% scaling.
    pub icon_size: u32,
    /// Draw a progress ring for each limit.
    pub rings: bool,
    /// With rings: put the number inside a larger ring instead of beside a small one.
    pub ring_text: bool,
    /// With rings: only the ring turns orange / red near the limit, the number keeps its color.
    pub ring_color_only: bool,
    /// With numbers inside: smaller rings that keep space above and below (small taskbars).
    pub ring_space: bool,
    /// With numbers inside: gap in pixels (at 100% scaling) between the number and the ring.
    pub ring_margin: u32,
    /// Leave out the `5h` / `W` labels, showing only the numbers.
    pub hide_labels: bool,
    /// Show `42` instead of `42%`.
    pub hide_percent: bool,
    /// Leave out the 5-hour, weekly or Fable group.
    pub hide_five: bool,
    pub hide_week: bool,
    pub hide_fable: bool,
    /// Accounts stacked per column (1 to MAX_ROWS_PER_COLUMN); more start a new column.
    pub rows_per_col: usize,
    pub provider_mark: u8, // 0 Claude, 1 Codex, 2 Spark
    pub codex_rows: Vec<Row>,
    pub spark_row: Option<Row>,
    pub show_claude: bool,
    pub show_codex: bool,
    pub show_spark: bool,
    pub spark_points: Vec<f32>,
    pub spark_busy: bool,
    pub spark_down: bool,
    pub spark_pulse_alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Claude,
    Codex,
    Spark,
}

#[derive(Clone, Copy, Debug)]
pub struct GroupRange {
    pub group: Group,
    pub x0: i32,
    pub x1: i32,
}

pub struct Frame {
    pub w: i32,
    pub h: i32,
    pub px: Vec<[u8; 4]>, // premultiplied B, G, R, A
    pub groups: Vec<GroupRange>,
}

fn value_text(v: Option<f64>, show_left: bool, hide_percent: bool) -> String {
    match v {
        Some(p) => {
            let used = if hide_percent && !show_left { p.round().max(0.0) as i64 } else { p.round().clamp(0.0, 100.0) as i64 };
            let n = if show_left { 100 - used } else { used };
            if hide_percent { n.to_string() } else { format!("{n}%") }
        }
        None => "–".into(),
    }
}

fn value_color(v: Option<f64>, pal: &Palette) -> Rgba {
    match v {
        // Claude Code's thresholds: warning above 75 %, critical above 95 %.
        Some(p) if p > 95.0 => pal.crit,
        Some(p) if p > 75.0 => pal.warn,
        _ => pal.value,
    }
}

unsafe fn make_font(px: i32, faces: &[&str]) -> HFONT {
    for (i, face) in faces.iter().enumerate() {
        let f = CreateFontW(
            -px, 0, 0, 0, FW_NORMAL as i32, 0, 0, 0,
            DEFAULT_CHARSET as u32, OUT_TT_PRECIS as u32, CLIP_DEFAULT_PRECIS as u32,
            DEFAULT_QUALITY as u32, (DEFAULT_PITCH | FF_SWISS) as u32,
            wide(face).as_ptr(),
        );
        if i + 1 == faces.len() || font_is(f, face) {
            return f;
        }
        DeleteObject(f);
    }
    unreachable!()
}

/// GDI silently substitutes missing fonts; check we got the face we asked for.
unsafe fn font_is(f: HFONT, face: &str) -> bool {
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    let old = SelectObject(dc, f);
    let mut buf = [0u16; 64];
    let n = GetTextFaceW(dc, buf.len() as i32, buf.as_mut_ptr());
    SelectObject(dc, old);
    DeleteDC(dc);
    let got = String::from_utf16_lossy(&buf[..n.max(1) as usize - 1]);
    got.eq_ignore_ascii_case(face)
}

struct Seg {
    text: Vec<u16>,
    bold: bool,
    /// The small font that fits inside a ring.
    small: bool,
    x: i32,
    w: i32,
    /// Vertical band of the account row this text belongs to.
    y0: i32,
    y1: i32,
    color: Rgba,
    /// Vertical motion blur radius in pixels.
    blur: i32,
    /// Room past `w` for glyph overhang.
    slack: i32,
}

struct Ring {
    x: i32,
    y: i32,
    /// Filled part, 0 to 1, clockwise from the top.
    frac: f32,
    color: Rgba,
    pace: Option<f32>,
}

struct Divider {
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

/// Renders the widget for a taskbar of height `h` at `scale` (DPI / 96), or with
/// `vert = Some(width)` for a vertical taskbar: one line per limit, stacked under
/// the icon, shrunk until it fits that width.
pub fn render(m: &Model, h: i32, scale: f32, vert: Option<i32>) -> Frame {
    let mut frames = Vec::new();
    if m.show_claude {
        let mut model = m.clone();
        model.codex_rows.clear();
        model.spark_row = None;
        frames.push((Group::Claude, render_one(&model, h, scale, vert)));
    }
    if m.show_codex {
        let mut model = m.clone();
        model.rows = model.codex_rows.clone();
        model.codex_rows.clear();
        model.spark_row = None;
        model.provider_mark = 1;
        model.hide_fable = true;
        model.hide_five = !model.rows.iter().any(|row| row.five.is_some());
        model.rows_per_col = 1;
        frames.push((Group::Codex, render_one(&model, h, scale, vert)));
    }
    if m.show_spark {
        if let Some(row) = m.spark_row.clone() {
            let mut model = m.clone();
            model.rows = vec![row];
            model.codex_rows.clear();
            model.spark_row = None;
            model.provider_mark = 2;
            model.hide_five = false;
            model.hide_week = true;
            model.hide_fable = true;
            model.hide_labels = true;
            model.hide_percent = true;
            model.show_left = false;
            model.rings = false;
            model.rows_per_col = 1;
            let mut frame = render_one(&model, h, scale, vert);
            decorate_spark(
                &mut frame,
                &m.spark_points,
                m.spark_busy,
                m.spark_down,
                m.spark_pulse_alpha,
                scale,
            );
            frames.push((Group::Spark, frame));
        }
    }
    if frames.is_empty() {
        return Frame {
            w: 1,
            h: h.max(1),
            px: vec![[0, 0, 0, 1]; h.max(1) as usize],
            groups: Vec::new(),
        };
    }
    combine(frames, vert.is_some(), m.dark, scale)
}

struct Bar {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    frac: f32,
    pace: Option<f32>,
    color: Rgba,
}

fn decorate_spark(f: &mut Frame, points: &[f32], busy: bool, down: bool, pulse: f32, scale: f32) {
    let add = (44.0 * scale).round() as i32;
    let old = f.w;
    let mut px = vec![[0, 0, 0, 1]; ((old + add) * f.h) as usize];
    for y in 0..f.h {
        for x in 0..old {
            px[(y * (old + add) + x) as usize] = f.px[(y * old + x) as usize];
        }
    }
    let alpha = if busy { pulse.clamp(0.45, 1.0) } else { 1.0 };
    let color = if down { [28, 43, 196] } else { [0, 185, 118] };
    let dot = [
        (color[0] as f32 * alpha) as u8,
        (color[1] as f32 * alpha) as u8,
        (color[2] as f32 * alpha) as u8,
        (255.0 * alpha) as u8,
    ];
    let cy = f.h / 2;
    for y in cy - 3..=cy + 3 {
        for x in old + 3..old + 9 {
            if (x - (old + 6)).pow(2) + (y - cy).pow(2) <= 9 {
                px[(y * (old + add) + x) as usize] = dot;
            }
        }
    }
    if !points.is_empty() {
        let green = [0, 185, 118]; // premultiplied as BGRA by blend_pixel
        let peak = points.iter().copied().fold(1.0f32, f32::max);
        let x0 = old + 12;
        let graph_width = (add - 15).max(1);
        let baseline = f.h - 4;
        let graph_height = (f.h - 8).max(1) as f32;
        let sample = |position: f32| {
            let last = points.len().saturating_sub(1);
            let index = position.floor() as usize;
            let t = position - index as f32;
            let p0 = points[index.saturating_sub(1).min(last)];
            let p1 = points[index.min(last)];
            let p2 = points[(index + 1).min(last)];
            let p3 = points[(index + 2).min(last)];
            let t2 = t * t;
            let t3 = t2 * t;
            (0.5
                * ((2.0 * p1)
                    + (-p0 + p2) * t
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
                    + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3))
                .clamp(0.0, peak)
        };
        let mut curve = Vec::with_capacity((graph_width + 1) as usize);
        for dx in 0..=graph_width {
            let position = if points.len() == 1 {
                0.0
            } else {
                dx as f32 * (points.len() - 1) as f32 / graph_width as f32
            };
            let value = sample(position);
            curve.push(baseline as f32 - value / peak * graph_height);
        }

        // Soft area fill, strongest at the curve and fading toward the baseline.
        for (dx, curve_y) in curve.iter().copied().enumerate() {
            let x = x0 + dx as i32;
            let top = curve_y.ceil() as i32;
            let depth = (baseline - top).max(1) as f32;
            for y in top..=baseline {
                let fade = 1.0 - (y - top) as f32 / depth;
                blend_pixel(
                    &mut px[(y * (old + add) + x) as usize],
                    green,
                    0.025 + 0.15 * fade,
                );
            }
        }

        // Smooth 1.5 px stroke at 100% scale.
        let radius = (0.75 * scale).max(0.75);
        for (dx, curve_y) in curve.iter().copied().enumerate() {
            let x = x0 + dx as i32;
            let extent = (radius + 1.0).ceil() as i32;
            for y in curve_y.round() as i32 - extent..=curve_y.round() as i32 + extent {
                if y >= 0 && y < f.h && x >= 0 && x < old + add {
                    let coverage = (radius + 0.5 - (y as f32 + 0.5 - curve_y).abs())
                        .clamp(0.0, 1.0);
                    blend_pixel(
                        &mut px[(y * (old + add) + x) as usize],
                        green,
                        coverage,
                    );
                }
            }
        }

        // Emphasize the newest sample.
        let last_x = x0 + graph_width;
        let last_y = *curve.last().unwrap_or(&(baseline as f32));
        let dot_radius = (1.8 * scale).max(1.5);
        let extent = dot_radius.ceil() as i32;
        for y in last_y.round() as i32 - extent..=last_y.round() as i32 + extent {
            for x in last_x - extent..=last_x + extent {
                if y >= 0 && y < f.h && x >= 0 && x < old + add {
                    let dx = x as f32 + 0.5 - last_x as f32;
                    let dy = y as f32 + 0.5 - last_y;
                    let coverage = (dot_radius + 0.5 - (dx * dx + dy * dy).sqrt())
                        .clamp(0.0, 1.0);
                    blend_pixel(
                        &mut px[(y * (old + add) + x) as usize],
                        green,
                        coverage,
                    );
                }
            }
        }
    }
    f.w = old + add;
    f.px = px;
}

fn blend_pixel(pixel: &mut [u8; 4], color: [u8; 3], alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    let keep = 1.0 - alpha;
    for channel in 0..3 {
        pixel[channel] = (color[channel] as f32 * alpha + pixel[channel] as f32 * keep) as u8;
    }
    pixel[3] = (255.0 * alpha + pixel[3] as f32 * keep) as u8;
}

fn combine(frames: Vec<(Group, Frame)>, vertical: bool, dark: bool, scale: f32) -> Frame {
    let gap = scale.round().max(1.0) as i32;
    let divider = palette(dark).divider;
    let alpha = divider.3;
    let divider_px = [
        (divider.2 * alpha * 255.0) as u8,
        (divider.1 * alpha * 255.0) as u8,
        (divider.0 * alpha * 255.0) as u8,
        (alpha * 255.0) as u8,
    ];
    let w = if vertical {
        frames.iter().map(|(_, frame)| frame.w).max().unwrap_or(1)
    } else {
        frames.iter().map(|(_, frame)| frame.w).sum::<i32>()
            + gap * (frames.len() as i32 - 1)
    };
    let h = if vertical {
        frames.iter().map(|(_, frame)| frame.h).sum::<i32>()
            + gap * (frames.len() as i32 - 1)
    } else {
        frames.iter().map(|(_, frame)| frame.h).max().unwrap_or(1)
    };
    let mut px = vec![[0, 0, 0, 1]; (w * h) as usize];
    let (mut ox, mut oy) = (0, 0);
    let mut groups = Vec::new();
    let divider_len = (20.0 * scale).round() as i32;
    for (i, (group, frame)) in frames.iter().enumerate() {
        for y in 0..frame.h {
            for x in 0..frame.w {
                px[((oy + y) * w + ox + x) as usize] = frame.px[(y * frame.w + x) as usize];
            }
        }
        groups.push(GroupRange {
            group: *group,
            x0: ox,
            x1: ox + frame.w,
        });
        if i + 1 < frames.len() {
            if vertical {
                let x0 = ((w - divider_len) / 2).max(0);
                let x1 = (x0 + divider_len).min(w);
                for x in x0..x1 {
                    px[((oy + frame.h) * w + x) as usize] = divider_px;
                }
                oy += frame.h + gap;
            } else {
                let y0 = ((h - divider_len) / 2).max(0);
                let y1 = (y0 + divider_len).min(h);
                for y in y0..y1 {
                    px[(y * w + ox + frame.w) as usize] = divider_px;
                }
                ox += frame.w + gap;
            }
        }
    }
    Frame { w, h, px, groups }
}

fn render_one(m: &Model, h: i32, scale: f32, vert: Option<i32>) -> Frame {
    let Some(avail) = vert else { return layout(m, h, scale, false) };
    let mut k = 1.0;
    loop {
        let f = layout(m, 0, scale * k, true);
        if f.w <= avail || k < 0.4 {
            return f;
        }
        k *= avail as f32 / f.w as f32 * 0.97;
    }
}

/// `vertical`: `h` is unused and the height follows the content.
fn layout(m: &Model, h: i32, scale: f32, vertical: bool) -> Frame {
    let pal = palette(m.dark);
    let s = |v: f32| (v * scale).round() as i32;
    // Vertical lines are sized as on a 32 px tall taskbar.
    let mut h = if vertical { s(32.0) } else { h };
    let empty = [Row {
        five: None,
        week: None,
        fable: None,
        roll: None,
        stale: false,
        pace: [None; 3],
        projected: [None; 3],
        text: [None, None, None],
        critical: [false; 3],
    }];
    let rows: &[Row] = if m.rows.is_empty() { &empty } else { &m.rows };
    // Vertical: every account in one column, one line per limit.
    let rows_per_col = if vertical { rows.len() } else { m.rows_per_col.clamp(1, MAX_ROWS_PER_COLUMN) };
    let per_col = if vertical { 1 } else { rows.len().min(rows_per_col) };
    // One account uses the full height like before; stacked accounts share
    // it, with a small margin, and the text shrinks to fit.
    let (band_top, row_h) = if per_col == 1 {
        (0.0, h as f32)
    } else {
        let margin = s(1.0) as f32;
        (margin, (h as f32 - 2.0 * margin) / per_col as f32)
    };
    let row_band = |r: usize| -> (i32, i32) {
        let y0 = (band_top + r as f32 * row_h).round() as i32;
        let y1 = (band_top + (r + 1) as f32 * row_h).round() as i32;
        (y0, y1.min(h))
    };
    unsafe {
        let mut font_px = s(m.font_size as f32);
        if per_col > 1 {
            // Stacked rows: the Default size fills most of a row, and the
            // other sizes scale from there, never taller than the row or
            // larger than they'd be on a single row.
            let fit = row_h * 0.8 * m.font_size as f32 / DEFAULT_FONT_SIZE as f32;
            font_px = font_px.min(fit.min(row_h).round() as i32).max(1);
        }
        let inside = m.rings && m.ring_text;
        // Ring diameter: big enough for the number inside, or about text height beside it.
        // "Space around the rings": 14 % of the row free above and below instead of
        // reaching the taskbar's edges (vertical lines have their own spacing).
        let fill = if inside && m.ring_space && !vertical { (row_h * 0.72).round() as i32 } else { row_h as i32 - s(2.0) };
        let ring_d = if !m.rings { 0 } else { fill.clamp(1, s(if inside { 34.0 } else { 16.0 })) };
        let ring_gap = if m.rings && !inside { s(5.0) } else { 0 };
        let mut small_px = ((ring_d as f32 * 0.32).round() as i32).max(1);
        let mut f_small = make_font(
            small_px,
            &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"],
        );
        let f_label = make_font(font_px, &["Segoe UI Variable Text", "Segoe UI"]);
        let f_value = make_font(font_px, &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"]);

        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let measure = |t: &[u16], f: HFONT| -> i32 {
            SelectObject(dc, f);
            let mut sz = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(dc, t.as_ptr(), t.len() as i32, &mut sz);
            sz.cx
        };
        // Blank space before the first glyph and after the last one.
        let bearings = |t: &[u16], f: HFONT| -> (i32, i32) {
            SelectObject(dc, f);
            let abc = |c: u16| {
                let mut a = std::mem::zeroed::<ABC>();
                GetCharABCWidthsW(dc, c as u32, c as u32, &mut a);
                a
            };
            match (t.first(), t.last()) {
                (Some(&a), Some(&b)) => (abc(a).abcA, abc(b).abcC),
                _ => (0, 0),
            }
        };
        if inside {
            // The widest number, "100%" (or "100"), fills the ring's inner width minus
            // the margin, growing or shrinking the text as needed.
            let room = (ring_d as f32 - 2.0 * (ring_d as f32 * 0.1).max(1.5) - 2.0 * s(m.ring_margin as f32) as f32).max(1.0);
            let widest = measure(&(if m.hide_percent { "100" } else { "100%" }).encode_utf16().collect::<Vec<u16>>(), f_small) as f32;
            if widest > 0.0 && (widest - room).abs() >= 0.5 {
                small_px = ((small_px as f32 * room / widest).floor() as i32).max(1);
                DeleteObject(f_small);
                f_small = make_font(small_px, &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"]);
            }
        }

        let dim = |stale: bool, c: Rgba| if stale { Rgba(c.0, c.1, c.2, c.3 * 0.5) } else { c };
        let all_stale = rows.iter().all(|r| r.stale);
        let pad_l = s(8.0);
        let icon = if m.hide_icon { 0 } else { s(m.icon_size as f32).min(h) };
        let gap_icon = if m.hide_icon { 0 } else { s(7.0) };
        let gap_lv = if m.hide_labels { 0 } else { s(5.0) };
        let gap_div = s(10.0);
        let div_w = s(1.0).max(1);
        let pad_r = s(4.0);
        let div_h = if per_col == 1 { s(20.0) } else { (row_h as i32 - s(2.0)).max(1) };

        let mut x = pad_l;
        let mut icon_x = x;
        let pad_v = s(6.0);
        // Vertical: the icon gets a line of its own, then each limit one line.
        let mut y = pad_v + if icon > 0 { icon + pad_v } else { 0 };
        let line_h = ((font_px as f32 * 1.6).round() as i32).max(ring_d + s(4.0));
        let mut hlines = Vec::new();
        if !vertical {
            x += icon + gap_icon;
        }
        let mut segs = Vec::new();
        let mut dividers = Vec::new();
        let mut rings = Vec::new();
        let mut bars = Vec::new();
        let enc = |t: &str| -> Vec<u16> { t.encode_utf16().collect() };
        let columns: Vec<&[Row]> = rows.chunks(rows_per_col).collect();
        for (ci, col) in columns.iter().enumerate() {
            // Line the column's rows up: each field is as wide as its widest value.
            let labels = [enc("5h"), enc("W"), enc("F")];
            // Which of 5h / week / Fable are shown, as indexes into `labels`.
            let ids: Vec<usize> = [!m.hide_five, !m.hide_week, !m.hide_fable && rows.iter().any(|r| r.fable.is_some())]
                .iter()
                .enumerate()
                .filter_map(|(k, on)| on.then_some(k))
                .collect();
            let groups = ids.len();
            let mut label_w: Vec<i32> = ids
                .iter()
                .map(|&k| &labels[k])
                .map(|l| if m.hide_labels { 0 } else { measure(l, f_label) })
                .collect();
            let values: Vec<Vec<Vec<u16>>> = col
                .iter()
                .map(|r| {
                    ids.iter()
                        .map(|&k| {
                            r.text[k].as_ref().map_or_else(
                                || {
                                    enc(&value_text(
                                        [r.five, r.week, r.fable][k],
                                        m.show_left,
                                        m.hide_percent,
                                    ))
                                },
                                |text| enc(text),
                            )
                        })
                        .collect()
                })
                .collect();
            let mut value_w = vec![0; groups];
            for v in &values {
                for g in 0..groups {
                    value_w[g] = value_w[g].max(measure(&v[g], if inside { f_small } else { f_value }));
                }
            }
            if !m.rings && m.provider_mark != 2 {
                for width in &mut value_w {
                    *width = (*width).max(s(28.0));
                }
            }
            let mut group_x = vec![pad_l; groups];
            let mut div_x = vec![0; groups];
            if vertical {
                // Labels share the widest label's width so the values line up.
                let lw = label_w.iter().copied().max().unwrap_or(0);
                label_w.iter_mut().for_each(|l| *l = lw);
                let vw = value_w.iter().copied().max().unwrap_or(0);
                x = pad_l + lw + gap_lv + if inside { ring_d } else { ring_d + ring_gap + vw };
            } else {
                for g in 0..groups {
                    group_x[g] = x;
                    x += label_w[g] + gap_lv + if inside { ring_d } else { ring_d + ring_gap + value_w[g] } + gap_div;
                    div_x[g] = x;
                    x += div_w;
                    if g + 1 < groups {
                        x += gap_div;
                    }
                }
            }
            for (ri, (row, vals)) in col.iter().zip(&values).enumerate() {
                let (y0, y1) = row_band(ri);
                // Eased progress of a rolling change (ease in-out cubic) and its speed, for the blur.
                let (p, speed) = match row.roll {
                    Some((_, t)) if t < 0.5 => (4.0 * t * t * t, 12.0 * t * t),
                    Some((_, t)) => {
                        let u = 2.0 - 2.0 * t.min(1.0);
                        (1.0 - u * u * u / 2.0, 3.0 * u * u)
                    }
                    None => (1.0, 0.0),
                };
                for g in 0..groups {
                    let (y0, y1) = if vertical {
                        y += line_h;
                        (y - line_h, y)
                    } else {
                        (y0, y1)
                    };
                    let val = [row.five, row.week, row.fable][ids[g]];
                    if !m.hide_labels {
                        segs.push(Seg { text: labels[ids[g]].clone(), bold: false, small: false, x: group_x[g], w: label_w[g], y0, y1, color: pal.label, blur: 0, slack: s(2.0) });
                    }
                    let dy = y0 + ((y1 - y0) - div_h) / 2;
                    if !vertical {
                        dividers.push(Divider { x0: div_x[g], x1: div_x[g] + div_w, y0: dy.max(0), y1: (dy + div_h).min(h) });
                    }
                    let content_x = group_x[g] + label_w[g] + gap_lv;
                    let pi = ids[g];
                    let color = match (val, row.pace[pi], row.projected[pi]) {
                        _ if row.critical[pi] => pal.crit,
                        (Some(_),_,Some(p)) if p>=100.0 => pal.crit,
                        (Some(v),Some(e),_) if v>=50.0 && v>e as f64*100.0+10.0 => pal.warn,
                        _ => value_color(val,&pal),
                    };
                    if m.rings {
                        // The ring shows the same quantity as the number.
                        let frac = |v: Option<f64>| {
                            let used = v.map_or(0.0, |q| q.round().clamp(0.0, 100.0) as f32 / 100.0);
                            if v.is_some() && m.show_left { 1.0 - used } else { used }
                        };
                        let to = frac(val);
                        rings.push(Ring {
                            x: content_x,
                            y: y0 + ((y1 - y0) - ring_d) / 2,
                            frac: row.roll.map_or(to, |(from, _)| frac(from[ids[g]]) + (to - frac(from[ids[g]])) * p),
                            color: dim(row.stale, if color == pal.value { RING_BLUE } else { color }),
                            pace: row.pace[ids[g]],
                        });
                    }
                    let vf = if inside { f_small } else { f_value };
                    let text_x = |t: &[u16]| {
                        if inside {
                            let (a, c) = bearings(t, vf);
                            content_x + (ring_d - (measure(t, vf) - a - c)) / 2 - a
                        } else {
                            content_x + ring_d + ring_gap
                        }
                    };
                    if !m.rings && m.provider_mark != 2 {
                        bars.push(Bar {
                            x: text_x(&vals[g]),
                            y: y1 - s(5.0),
                            w: value_w[g],
                            h: s(2.0).max(1),
                            frac: val.unwrap_or(0.0).clamp(0.0, 100.0) as f32 / 100.0,
                            pace: row.pace[pi],
                            color: dim(
                                row.stale,
                                if color == pal.value { RING_BLUE } else { color },
                            ),
                        });
                    }
                    let color = dim(row.stale, if m.rings && m.ring_color_only { pal.value } else { color });
                    let old = row
                        .roll
                        .map(|(from, _)| enc(&value_text(from[ids[g]], m.show_left, m.hide_percent)))
                        .filter(|o| *o != vals[g]);
                    let Some(old) = old else {
                        let vw = measure(&vals[g], vf);
                        segs.push(Seg { text: vals[g].clone(), bold: true, small: inside, x: text_x(&vals[g]), w: vw, y0, y1, color, blur: 0, slack: s(2.0) });
                        continue;
                    };
                    // Rolling: each changed character's old glyph leaves upward while the new
                    // one arrives from below, one line apart, each drawn in its own line box.
                    let mut line = ((if inside { small_px } else { font_px }) as f32 * 1.25).round() as i32;
                    line += (line - (y1 - y0)) & 1; // same parity as the row, so still glyphs don't shift
                    let top = y0 + ((y1 - y0) - line) / 2;
                    let moved = (p * line as f32).round() as i32;
                    let blur = (speed * line as f32 * 0.048).round() as i32;
                    for (text, other, is_new) in [(&old, &vals[g], false), (&vals[g], &old, true)] {
                        let base = text_x(text);
                        for i in 0..text.len() {
                            let changed = other.get(i) != Some(&text[i]);
                            if !changed && !is_new {
                                continue; // unchanged: drawn once, from the new text
                            }
                            let (dy, alpha) = match (changed, is_new) {
                                (false, _) => (0, 1.0),
                                (true, true) => (line - moved, p),
                                (true, false) => (-moved, 1.0 - p),
                            };
                            let cx = measure(&text[..i], vf);
                            segs.push(Seg {
                                text: vec![text[i]],
                                bold: true,
                                small: inside,
                                x: base + cx,
                                w: measure(&text[..=i], vf) - cx,
                                y0: top + dy,
                                y1: top + dy + line,
                                color: Rgba(color.0, color.1, color.2, color.3 * alpha),
                                blur: if changed { blur } else { 0 },
                                slack: 0,
                            });
                        }
                    }
                }
                if vertical && ri + 1 < col.len() {
                    // A rule between accounts.
                    hlines.push(y + gap_div / 2);
                    y += gap_div;
                }
            }
            if ci + 1 < columns.len() {
                x += gap_div;
            }
        }
        let w = if vertical { x + pad_l } else { x + pad_r };
        if vertical {
            h = y + pad_v;
            icon_x = (w - icon) / 2;
            for &hy in &hlines {
                dividers.push(Divider { x0: pad_l, x1: w - pad_l, y0: hy, y1: hy + div_w });
            }
        }

        // Text mask: white text on black.
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        let old_bmp = SelectObject(dc, bmp);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00FFFFFF);
        for seg in &segs {
            SelectObject(dc, if seg.small { f_small } else if seg.bold { f_value } else { f_label });
            let mut rc = RECT { left: seg.x, top: seg.y0, right: seg.x + seg.w + seg.slack, bottom: seg.y1 };
            DrawTextW(dc, seg.text.as_ptr(), seg.text.len() as i32, &mut rc,
                DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX);
        }
        GdiFlush();
        let n = (w * h) as usize;
        // ClearType coverage per subpixel, as R, G, B (the DIB is BGRA). On a light
        // taskbar it is averaged to gray: one alpha can't carry per-channel coverage,
        // and the error that leaves (bg x coverage difference) shows on light
        // backgrounds only.
        let mask: Vec<[u8; 3]> = std::slice::from_raw_parts(bits as *const [u8; 4], n)
            .iter()
            .map(|p| {
                if m.dark {
                    [p[2], p[1], p[0]]
                } else {
                    let g = ((p[0] as u32 + p[1] as u32 + p[2] as u32) / 3) as u8;
                    [g, g, g]
                }
            })
            .collect();
        SelectObject(dc, old_bmp);
        DeleteObject(bmp);
        DeleteDC(dc);
        DeleteObject(f_label);
        DeleteObject(f_value);
        DeleteObject(f_small);

        // Compose (premultiplied floats).
        let mut buf = vec![[0f32; 4]; n];
        let over = |buf: &mut [[f32; 4]], i: usize, c: Rgba, cov: f32| {
            let a = c.3 * cov;
            if a <= 0.0 {
                return;
            }
            let d = &mut buf[i];
            let k = 1.0 - a;
            d[0] = c.2 * a + d[0] * k;
            d[1] = c.1 * a + d[1] * k;
            d[2] = c.0 * a + d[2] * k;
            d[3] = a + d[3] * k;
        };

        // Hover pill (rounded rect, like taskbar buttons).
        let (top, bottom) = (s(4.0) as f32, (h - s(4.0)) as f32);
        if m.hover {
            let r = s(4.0) as f32;
            for y in 0..h {
                for xx in 0..w {
                    let cov = rounded_rect_cov(xx as f32 + 0.5, y as f32 + 0.5, 0.0, top, w as f32, bottom, r);
                    over(&mut buf, (y * w + xx) as usize, pal.hover, cov);
                }
            }
        }

        // Dividers.
        for d in &dividers {
            for y in d.y0..d.y1 {
                for xx in d.x0..d.x1 {
                    over(&mut buf, (y * w + xx) as usize, pal.divider, 1.0);
                }
            }
        }

        // Number-mode pace bars: usage fill plus a bright elapsed-time marker.
        for bar in &bars {
            let radius = bar.h as f32 / 2.0;
            let fill_right = bar.x as f32 + bar.w as f32 * bar.frac;
            for y in bar.y..bar.y + bar.h {
                for x in bar.x..bar.x + bar.w {
                    let coverage = rounded_rect_cov(
                        x as f32 + 0.5,
                        y as f32 + 0.5,
                        bar.x as f32,
                        bar.y as f32,
                        (bar.x + bar.w) as f32,
                        (bar.y + bar.h) as f32,
                        radius,
                    );
                    let index = (y * w + x) as usize;
                    over(&mut buf, index, pal.divider, coverage);
                    if x as f32 + 0.5 <= fill_right {
                        over(&mut buf, index, bar.color, coverage);
                    }
                }
            }
            if let Some(pace) = bar.pace {
                let tick_x = bar.x + (pace.clamp(0.0, 1.0) * (bar.w - 1) as f32).round() as i32;
                for y in bar.y - 1..bar.y + bar.h + 1 {
                    if y >= 0 && y < h {
                        over(&mut buf, (y * w + tick_x) as usize, pal.value, 1.0);
                    }
                }
            }
        }

        // Usage rings: a faint track, then the filled arc clockwise from the top.
        let rad = ring_d as f32 / 2.0;
        let stroke = (ring_d as f32 * 0.1).max(1.5);
        for r in &rings {
            for y in 0..ring_d {
                for xx in 0..ring_d {
                    let (dx, dy) = (xx as f32 + 0.5 - rad, y as f32 + 0.5 - rad);
                    let d = (dx * dx + dy * dy).sqrt();
                    let band = (stroke / 2.0 + 0.5 - (d - (rad - stroke / 2.0)).abs()).clamp(0.0, 1.0);
                    let i = (r.y + y) * w + r.x + xx;
                    if band <= 0.0 || r.y + y < 0 || r.y + y >= h || i as usize >= n {
                        continue;
                    }
                    over(&mut buf, i as usize, pal.divider, band);
                    let a = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU);
                    let arc = if r.frac >= 1.0 {
                        1.0
                    } else if r.frac <= 0.0 {
                        0.0
                    } else {
                        // Distance along the ring to the arc's end, in pixels.
                        ((r.frac * std::f32::consts::TAU - a) * d + 0.5).clamp(0.0, 1.0)
                    };
                    over(&mut buf, i as usize, r.color, band * arc);
                }
            }
            if let Some(p)=r.pace { let a=p.clamp(0.0,1.0)*std::f32::consts::TAU; let (sn,cs)=a.sin_cos(); for q in 0..stroke.ceil() as i32+2 { let rr=rad-q as f32; let xx=(r.x as f32+rad+sn*rr).round()as i32; let yy=(r.y as f32+rad-cs*rr).round()as i32; if xx>=0&&xx<w&&yy>=0&&yy<h{over(&mut buf,(yy*w+xx)as usize,pal.label,1.0);} } }
        }

        // Provider mark.
        let iy = if vertical { pad_v } else { (h - icon) / 2 };
        let mark = match m.provider_mark { 1=>draw_codex_mark(icon),2=>draw_spark_mark(icon),_ if m.pixel_icon=>draw_pixel_mark(icon),_=>draw_mark(icon) };
        let mark_color=match m.provider_mark{2=>Rgba(0.463,0.725,0.0,1.0),1=>RING_BLUE,_=>CLAUDE_ORANGE};
        for y in 0..icon {
            for xx in 0..icon {
                let cov = mark[(y * icon + xx) as usize];
                over(&mut buf, ((iy + y) * w + icon_x + xx) as usize, dim(all_stale, mark_color), cov);
            }
        }

        // Text.
        for seg in &segs {
            for y in seg.y0.max(0)..seg.y1.min(h) {
                for xx in seg.x.max(0)..(seg.x + seg.w + seg.slack).min(w) {
                    let i = (y * w + xx) as usize;
                    // Motion blur: a vertical box average kept inside the glyph's own line box.
                    let mut sum = [0u32; 3];
                    for yy in (y - seg.blur..=y + seg.blur).filter(|yy| (seg.y0.max(0)..seg.y1.min(h)).contains(yy)) {
                        let mk = mask[(yy * w + xx) as usize];
                        (0..3).for_each(|c| sum[c] += mk[c] as u32);
                    }
                    let cov = sum.map(|v| v as f32 / (255.0 * (2 * seg.blur + 1) as f32));
                    if cov.iter().any(|&c| c > 0.0) {
                        // Per-channel "over" in premultiplied BGRA; alpha takes the
                        // strongest channel.
                        let c = seg.color;
                        let a = cov.map(|v| c.3 * v);
                        let d = &mut buf[i];
                        d[0] = c.2 * a[2] + d[0] * (1.0 - a[2]);
                        d[1] = c.1 * a[1] + d[1] * (1.0 - a[1]);
                        d[2] = c.0 * a[0] + d[2] * (1.0 - a[0]);
                        let am = a[0].max(a[1]).max(a[2]);
                        d[3] = am + d[3] * (1.0 - am);
                    }
                }
            }
        }

        // Keep the whole widget hit-testable (alpha 0 pixels are click-through).
        let px = buf
            .iter()
            .map(|p| {
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                if p[3] < 1.0 / 255.0 {
                    [0, 0, 0, 1]
                } else {
                    [q(p[0]), q(p[1]), q(p[2]), q(p[3])]
                }
            })
            .collect();
        Frame { w, h, px, groups: Vec::new() }
    }
}

fn draw_codex_mark(size:i32)->Vec<f32>{ let mut o=vec![0.0;(size*size)as usize];let c=(size-1)as f32/2.0;let r=c*0.82;for y in 0..size{for x in 0..size{let dx=x as f32-c;let dy=y as f32-c;let a=dy.atan2(dx);let hex=r*(std::f32::consts::PI/6.0).cos()/((a.rem_euclid(std::f32::consts::PI/3.0)-std::f32::consts::PI/6.0).cos());let d=(dx*dx+dy*dy).sqrt();if(d-hex).abs()<1.4||(d<r*0.48&&((dx+dy).abs()<1.2||(dx-dy).abs()<1.2)){o[(y*size+x)as usize]=1.0}}}o}
fn draw_spark_mark(size: i32) -> Vec<f32> {
    let mut out = vec![0.0; (size * size) as usize];
    let n = size as f32;
    let (left, top, right, bottom) = (n * 0.22, n * 0.22, n * 0.78, n * 0.78);
    let radius = n * 0.11;
    let stroke = (n * 0.085).max(1.0);
    let pin_half = (n * 0.045).max(0.7);
    let pin_length = n * 0.16;
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let outer = rounded_rect_cov(px, py, left, top, right, bottom, radius);
            let inner = rounded_rect_cov(
                px,
                py,
                left + stroke,
                top + stroke,
                right - stroke,
                bottom - stroke,
                (radius - stroke).max(0.0),
            );
            let mut coverage = (outer - inner).max(0.0);
            for position in [0.39, 0.61] {
                let along = n * position;
                let horizontal = py >= along - pin_half && py <= along + pin_half
                    && ((px >= left - pin_length && px <= left)
                        || (px >= right && px <= right + pin_length));
                let vertical = px >= along - pin_half && px <= along + pin_half
                    && ((py >= top - pin_length && py <= top)
                        || (py >= bottom && py <= bottom + pin_length));
                if horizontal || vertical {
                    coverage = 1.0;
                }
            }
            out[(y * size + x) as usize] = coverage;
        }
    }
    out
}

fn rounded_rect_cov(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> f32 {
    let cx = x.clamp(l + rad, r - rad);
    let cy = y.clamp(t + rad, b - rad);
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - rad;
    (0.5 - d).clamp(0.0, 1.0)
}

/// Pixel-art mark (Claude Code's little creature), as wide as the asterisk.
/// Cells may cover part of a pixel; edge pixels get the covered fraction.
fn draw_pixel_mark(size: i32) -> Vec<f32> {
    const ART: [&[u8; 12]; 8] = [
        b"..########..",
        b"..########..",
        b"..#.####.#..",
        b"############",
        b"..########..",
        b"..########..",
        b"..#.#..#.#..",
        b"..#.#..#.#..",
    ];
    let cell = size as f32 / 12.0;
    let oy = (size as f32 - 8.0 * cell) / 2.0;
    // How much of pixel `p` the span [a, b) covers.
    let overlap = |p: i32, a: f32, b: f32| (b.min(p as f32 + 1.0) - a.max(p as f32)).max(0.0);
    let mut out = vec![0f32; (size * size) as usize];
    for (r, row) in ART.iter().enumerate() {
        for (c, _) in row.iter().enumerate().filter(|(_, ch)| **ch == b'#') {
            let (x0, y0) = (c as f32 * cell, oy + r as f32 * cell);
            let (x1, y1) = (x0 + cell, y0 + cell);
            for y in (y0.floor() as i32).max(0)..(y1.ceil() as i32).min(size) {
                for x in (x0.floor() as i32).max(0)..(x1.ceil() as i32).min(size) {
                    let v = &mut out[(y * size + x) as usize];
                    *v = (*v + overlap(x, x0, x1) * overlap(y, y0, y1)).min(1.0);
                }
            }
        }
    }
    out
}

/// Claude's asterisk-like mark: 12 rounded spokes of slightly varying length.
fn draw_mark(size: i32) -> Vec<f32> {
    const SS: i32 = 4; // supersampling
    let n = 12;
    let c = size as f32 / 2.0;
    let r = size as f32 / 2.0;
    let lens = [1.0, 0.82, 0.95, 0.78, 1.0, 0.86, 0.92, 0.8, 0.98, 0.84, 0.9, 0.8];
    let half_w = (size as f32 * 0.052).max(0.8);
    let spokes: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let a = (i as f32) * std::f32::consts::TAU / n as f32 + 0.13;
            let len = (r - half_w) * lens[i as usize];
            (a.cos() * len, a.sin() * len)
        })
        .collect();
    let mut out = vec![0f32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = x as f32 + (sx as f32 + 0.5) / SS as f32 - c;
                    let py = y as f32 + (sy as f32 + 0.5) / SS as f32 - c;
                    let inside = spokes.iter().any(|&(ex, ey)| {
                        // distance from point to segment (0,0)-(ex,ey)
                        let t = ((px * ex + py * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
                        let (dx, dy) = (px - ex * t, py - ey * t);
                        // taper: thinner toward the tip
                        let hw = half_w * (1.0 + 0.25 * t);
                        dx * dx + dy * dy <= hw * hw
                    });
                    if inside {
                        hits += 1;
                    }
                }
            }
            out[(y * size + x) as usize] = hits as f32 / (SS * SS) as f32;
        }
    }
    out
}
