//! Draws the widget into a premultiplied BGRA pixel buffer.
//!
//! GDI can't draw text with alpha, so text is drawn white-on-black into a mask
//! and its brightness becomes coverage, then tinted. The Claude mark is drawn
//! analytically (supersampled capsules), so it stays crisp at any DPI.

use crate::http::wide;
use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub label: Rgba,
    pub value: Rgba,
    pub warn: Rgba,
    pub crit: Rgba,
    pub divider: Rgba,
    pub hover: Rgba,
    pub track: Rgba,
    pub elapsed: Rgba,
    pub claude: Rgba,
    pub codex: Rgba,
    pub spark: Rgba,
    pub card_background: Rgba,
    pub card_border: Rgba,
    pub panel: Rgba,
    pub panel_border: Rgba,
    pub faint: Rgba,
    pub meter_track: Rgba,
    pub meter_elapsed: Rgba,
    pub separator: Rgba,
    pub good: Rgba,
    pub idle: Rgba,
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
            label: c(0x9A9AA3, 1.0),
            value: c(0xF2F2F4, 1.0),
            warn: c(0xE8A33D, 1.0),
            crit: c(0xFF6B61, 1.0),
            divider: c(0x3A3A40, 1.0),
            hover: c(0xFFFFFF, 0.07),
            track: c(0x3A3A40, 1.0),
            elapsed: c(0x5A5A62, 1.0),
            claude: c(0xD97757, 1.0),
            codex: c(0x4C9AFF, 1.0),
            spark: c(0x76B900, 1.0),
            card_background: c(0x1E1E21, 1.0),
            card_border: c(0x323238, 1.0),
            panel: c(0x26262B, 1.0),
            panel_border: c(0x2E2E33, 1.0),
            faint: c(0x6E6E77, 1.0),
            meter_track: c(0x36363C, 1.0),
            meter_elapsed: c(0x4C4C54, 1.0),
            separator: c(0x34343A, 1.0),
            good: c(0x7EAE5B, 1.0),
            idle: c(0x8A8A93, 1.0),
        }
    } else {
        Palette {
            label: c(0x5F5F68, 1.0),
            value: c(0x1B1B1F, 1.0),
            warn: c(0xA86200, 1.0),
            crit: c(0xC42B1C, 1.0),
            divider: c(0xD0D0D6, 1.0),
            hover: c(0xFFFFFF, 0.55),
            track: c(0xD4D4DA, 1.0),
            elapsed: c(0xB0B0B8, 1.0),
            claude: c(0xD97757, 1.0),
            codex: c(0x4C9AFF, 1.0),
            spark: c(0x76B900, 1.0),
            card_background: c(0xF3F3F6, 1.0),
            card_border: c(0xDCDCE1, 1.0),
            panel: c(0xFFFFFF, 1.0),
            panel_border: c(0xE4E4E9, 1.0),
            faint: c(0x85858E, 1.0),
            meter_track: c(0xE4E4E9, 1.0),
            meter_elapsed: c(0xC8C8CF, 1.0),
            separator: c(0xE8E8EC, 1.0),
            good: c(0x41781C, 1.0),
            idle: c(0x8A8A93, 1.0),
        }
    }
}

const RING_BLUE: Rgba = Rgba(0.298, 0.604, 1.0, 1.0); // #4C9AFF
pub const CLAUDE_ORANGE: Rgba = Rgba(0.851, 0.467, 0.341, 1.0); // #D97757
const SPARK_GREEN: Rgba = Rgba(0.463, 0.725, 0.0, 1.0); // #76B900

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemState {
    Ok,
    Warn,
    Crit,
}

pub fn strip_item_state(
    used: Option<f64>,
    elapsed_fraction: Option<f32>,
    projected: Option<f32>,
    critical: bool,
) -> ItemState {
    if critical
        || used.is_some_and(|value| value > 95.0)
        || projected.is_some_and(|value| value >= 100.0)
    {
        ItemState::Crit
    } else if used.is_some_and(|value| {
        value > 50.0
            && elapsed_fraction.is_some_and(|elapsed| value > elapsed as f64 * 100.0 + 10.0)
    }) {
        ItemState::Warn
    } else {
        ItemState::Ok
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentBand {
    Used,
    Projected,
    Elapsed,
    Track,
}

#[allow(dead_code)]
pub fn segment_band(index: usize, used: f32, projected: f32, elapsed: f32) -> SegmentBand {
    let midpoint = (index as f32 + 0.5) * 5.0;
    if midpoint < used {
        SegmentBand::Used
    } else if midpoint < projected {
        SegmentBand::Projected
    } else if midpoint < elapsed {
        SegmentBand::Elapsed
    } else {
        SegmentBand::Track
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

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
    /// No reading has arrived yet and a request is in flight.
    pub loading: bool,
    /// Opacity for loading tracks and critical-entry number pulses.
    pub pulse_alpha: f32,
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
            let used = if hide_percent && !show_left {
                p.round().max(0.0) as i64
            } else {
                p.round().clamp(0.0, 100.0) as i64
            };
            let n = if show_left { 100 - used } else { used };
            if hide_percent {
                n.to_string()
            } else {
                format!("{n}%")
            }
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
            -px,
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            DEFAULT_QUALITY as u32,
            (DEFAULT_PITCH | FF_SWISS) as u32,
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

fn displayed_value_color(
    provider_mark: u8,
    value: Option<f64>,
    critical: bool,
    pace: Option<f32>,
    projected: Option<f32>,
    palette: &Palette,
) -> Rgba {
    if provider_mark == 2 {
        return if critical {
            palette.crit
        } else {
            palette.value
        };
    }
    match (value, pace, projected) {
        _ if critical => palette.crit,
        (Some(_), _, Some(projected)) if projected >= 100.0 => palette.crit,
        (Some(value), Some(elapsed), _)
            if value >= 50.0 && value > elapsed as f64 * 100.0 + 10.0 =>
        {
            palette.warn
        }
        _ => value_color(value, palette),
    }
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
    if !m.rings {
        return render_forecast_strip(m, h, scale, vert.is_some());
    }
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
                m.dark,
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

fn color_for_state(state: ItemState, accent: Rgba, pal: &Palette) -> Rgba {
    match state {
        ItemState::Ok => accent,
        ItemState::Warn => pal.warn,
        ItemState::Crit => pal.crit,
    }
}

fn number_color_for_state(state: ItemState, pal: &Palette) -> Rgba {
    match state {
        ItemState::Ok => pal.value,
        ItemState::Warn => pal.warn,
        ItemState::Crit => pal.crit,
    }
}

fn over_pixel(pixel: &mut [u8; 4], color: Rgba, coverage: f32) {
    let alpha = (color.3 * coverage).clamp(0.0, 1.0);
    let source = [color.2, color.1, color.0];
    let old_alpha = pixel[3] as f32 / 255.0;
    for channel in 0..3 {
        let old = pixel[channel] as f32 / 255.0;
        pixel[channel] = ((source[channel] * alpha + old * (1.0 - alpha)) * 255.0 + 0.5) as u8;
    }
    pixel[3] = ((alpha + old_alpha * (1.0 - alpha)) * 255.0 + 0.5) as u8;
}

fn paint_rect(frame: &mut Frame, rect: (i32, i32, i32, i32), radius: f32, color: Rgba) {
    let (left, top, right, bottom) = rect;
    for y in top.max(0)..bottom.min(frame.h) {
        for x in left.max(0)..right.min(frame.w) {
            let coverage = rounded_rect_cov(
                x as f32 + 0.5,
                y as f32 + 0.5,
                left as f32,
                top as f32,
                right as f32,
                bottom as f32,
                radius,
            );
            over_pixel(&mut frame.px[(y * frame.w + x) as usize], color, coverage);
        }
    }
}

fn paint_mask(frame: &mut Frame, x: i32, y: i32, size: i32, mask: &[f32], color: Rgba) {
    for my in 0..size {
        for mx in 0..size {
            let (px, py) = (x + mx, y + my);
            if px >= 0 && px < frame.w && py >= 0 && py < frame.h {
                over_pixel(
                    &mut frame.px[(py * frame.w + px) as usize],
                    color,
                    mask[(my * size + mx) as usize],
                );
            }
        }
    }
}

unsafe fn paint_text(
    frame: &mut Frame,
    text: &str,
    rect: RECT,
    px: i32,
    semibold: bool,
    color: Rgba,
    flags: u32,
) {
    if text.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
        return;
    }
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        ..std::mem::zeroed()
    };
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    let mut bits = std::ptr::null_mut();
    let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    let old_bmp = SelectObject(dc, bmp);
    let font = make_font(
        px.max(1),
        if semibold {
            &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"]
        } else {
            &["Segoe UI Variable Text", "Segoe UI"]
        },
    );
    let old_font = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, 0x00FFFFFF);
    let wide = text.encode_utf16().collect::<Vec<_>>();
    let mut local = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    DrawTextW(
        dc,
        wide.as_ptr(),
        wide.len() as i32,
        &mut local,
        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | flags,
    );
    GdiFlush();
    let mask = std::slice::from_raw_parts(bits.cast::<[u8; 4]>(), (width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let sample = mask[(y * width + x) as usize];
            let coverage = (sample[0] as f32 + sample[1] as f32 + sample[2] as f32) / (3.0 * 255.0);
            if coverage > 0.0 {
                over_pixel(
                    &mut frame.px[((rect.top + y) * frame.w + rect.left + x) as usize],
                    color,
                    coverage,
                );
            }
        }
    }
    SelectObject(dc, old_font);
    SelectObject(dc, old_bmp);
    DeleteObject(font);
    DeleteObject(bmp);
    DeleteDC(dc);
}

unsafe fn measure_text_width(text: &str, px: i32, semibold: bool) -> i32 {
    let dc = CreateCompatibleDC(std::ptr::null_mut());
    let font = make_font(
        px.max(1),
        if semibold {
            &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"]
        } else {
            &["Segoe UI Variable Text", "Segoe UI"]
        },
    );
    let old_font = SelectObject(dc, font);
    let wide = text.encode_utf16().collect::<Vec<_>>();
    let mut size = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(dc, wide.as_ptr(), wide.len() as i32, &mut size);
    SelectObject(dc, old_font);
    DeleteObject(font);
    DeleteDC(dc);
    size.cx.max(1)
}

fn new_frame(width: i32, height: i32) -> Frame {
    Frame {
        w: width.max(1),
        h: height.max(1),
        px: vec![[0, 0, 0, 1]; (width.max(1) * height.max(1)) as usize],
        groups: Vec::new(),
    }
}

fn limit_ids(m: &Model, rows: &[Row], claude: bool) -> Vec<usize> {
    [
        !m.hide_five && (claude || rows.iter().any(|row| row.five.is_some() || row.loading)),
        !m.hide_week,
        !m.hide_fable && rows.iter().any(|row| row.fable.is_some()),
    ]
    .iter()
    .enumerate()
    .filter_map(|(index, shown)| shown.then_some(index))
    .collect()
}

fn limit_group_width(item_count: usize, scale: f32) -> i32 {
    let s = |value: f32| (value * scale).round() as i32;
    s(16.0)
        + if item_count == 0 {
            0
        } else {
            s(8.0) + item_count as i32 * s(30.0) + item_count.saturating_sub(1) as i32 * s(8.0)
        }
}

fn limit_group_height(item_count: usize, scale: f32) -> i32 {
    let s = |value: f32| (value * scale).round() as i32;
    s(16.0)
        + if item_count == 0 {
            0
        } else {
            s(8.0) + item_count as i32 * s(22.0) + item_count.saturating_sub(1) as i32 * s(6.0)
        }
}

fn render_forecast_strip(m: &Model, h: i32, scale: f32, vertical: bool) -> Frame {
    let pal = palette(m.dark);
    let claude_ids = limit_ids(m, &m.rows, true);
    let codex_ids = limit_ids(m, &m.codex_rows, false);
    let spark_shown = m.show_spark && m.spark_row.is_some();
    let s = |value: f32| (value * scale).round() as i32;
    let mut shown = Vec::new();
    if m.show_claude {
        shown.push((Group::Claude, limit_group_width(claude_ids.len(), scale)));
    }
    if m.show_codex {
        shown.push((Group::Codex, limit_group_width(codex_ids.len(), scale)));
    }
    if spark_shown {
        shown.push((Group::Spark, s(104.0)));
    }
    if shown.is_empty() {
        return new_frame(1, h);
    }
    if vertical {
        let heights = shown
            .iter()
            .map(|(group, _)| match group {
                Group::Claude => limit_group_height(claude_ids.len(), scale),
                Group::Codex => limit_group_height(codex_ids.len(), scale),
                Group::Spark => s(68.0),
            })
            .collect::<Vec<_>>();
        let width = shown.iter().map(|(_, width)| *width).max().unwrap_or(1) + s(8.0);
        let height = s(8.0)
            + heights.iter().sum::<i32>()
            + s(9.0) * heights.len().saturating_sub(1) as i32
            + s(8.0);
        let mut frame = new_frame(width, height);
        let mut y = s(8.0);
        for (index, ((group, group_width), group_height)) in
            shown.iter().zip(heights.iter()).enumerate()
        {
            let x = (width - group_width) / 2;
            draw_forecast_group(&mut frame, m, *group, x, y, scale, true, &pal);
            frame.groups.push(GroupRange {
                group: *group,
                x0: 0,
                x1: width,
            });
            y += group_height;
            if index + 1 < shown.len() {
                let left = (width - s(20.0)) / 2;
                paint_rect(
                    &mut frame,
                    (left, y + s(4.0), left + s(20.0), y + s(5.0)),
                    0.0,
                    pal.divider,
                );
                y += s(9.0);
            }
        }
        return frame;
    }
    let separator_space = s(17.0);
    let width = s(8.0)
        + shown.iter().map(|(_, width)| *width).sum::<i32>()
        + separator_space * shown.len().saturating_sub(1) as i32;
    let mut frame = new_frame(width, h.max(s(48.0)));
    let mut x = s(4.0);
    for (index, (group, group_width)) in shown.iter().enumerate() {
        draw_forecast_group(&mut frame, m, *group, x, 0, scale, false, &pal);
        frame.groups.push(GroupRange {
            group: *group,
            x0: x,
            x1: x + group_width,
        });
        x += group_width;
        if index + 1 < shown.len() {
            let top = (frame.h - s(20.0)) / 2;
            paint_rect(
                &mut frame,
                (x + s(8.0), top, x + s(9.0), top + s(20.0)),
                0.0,
                pal.divider,
            );
            x += separator_space;
        }
    }
    frame
}

fn draw_forecast_group(
    frame: &mut Frame,
    m: &Model,
    group: Group,
    x: i32,
    y: i32,
    scale: f32,
    vertical: bool,
    pal: &Palette,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let (rows, accent, icon) = match group {
        Group::Claude => (&m.rows, pal.claude, draw_mark(s(16.0))),
        Group::Codex => (&m.codex_rows, pal.codex, draw_codex_mark(s(16.0))),
        Group::Spark => (&m.rows, pal.spark, draw_spark_mark(s(16.0))),
    };
    if group == Group::Spark {
        draw_forecast_spark(frame, m, x, y, scale, vertical, pal);
        return;
    }
    let ids = limit_ids(m, rows, group == Group::Claude);
    let row = rows.first();
    let stale = row.is_some_and(|row| row.stale);
    let opacity = if stale { 0.45 } else { 1.0 };
    let icon_color = Rgba(accent.0, accent.1, accent.2, opacity);
    let icon_x = if vertical {
        x + (limit_group_width(ids.len(), scale) - s(16.0)) / 2
    } else {
        x
    };
    let icon_y = if vertical { y } else { (frame.h - s(16.0)) / 2 };
    paint_mask(frame, icon_x, icon_y, s(16.0), &icon, icon_color);
    let mut item_x = if vertical { x } else { x + s(24.0) };
    let mut item_y = if vertical {
        y + s(24.0)
    } else {
        (frame.h - s(22.0)) / 2
    };
    for id in ids {
        let (value, elapsed, projected, critical, loading, pulse) =
            row.map_or((None, None, None, false, false, 1.0), |row| {
                (
                    [row.five, row.week, row.fable][id],
                    row.pace[id],
                    row.projected[id],
                    row.critical[id],
                    row.loading,
                    row.pulse_alpha,
                )
            });
        let state = strip_item_state(value, elapsed, projected, critical);
        let fill_color = color_for_state(state, accent, pal);
        let mut number_state_color = number_color_for_state(state, pal);
        number_state_color.3 *= opacity * if state == ItemState::Crit { pulse } else { 1.0 };
        let muted = Rgba(pal.label.0, pal.label.1, pal.label.2, opacity);
        let item_width = s(30.0);
        let label = ["5h", "W", "F"][id];
        let number = if loading {
            "··".to_owned()
        } else {
            value
                .map(|value| {
                    let used = value.round().clamp(0.0, 100.0) as i32;
                    if m.show_left { 100 - used } else { used }.to_string()
                })
                .unwrap_or_else(|| "—".to_owned())
        };
        unsafe {
            paint_text(
                frame,
                if m.hide_labels { "" } else { label },
                RECT {
                    left: item_x,
                    top: item_y,
                    right: item_x + item_width,
                    bottom: item_y + s(15.0),
                },
                s(10.0),
                false,
                muted,
                DT_LEFT,
            );
            let number_rect = RECT {
                left: item_x,
                top: item_y,
                right: item_x + item_width,
                bottom: item_y + s(15.0),
            };
            let number_color = if loading || value.is_none() {
                muted
            } else {
                number_state_color
            };
            let old_number = row.and_then(|row| row.roll).and_then(|(from, t)| {
                from[id].map(|old| {
                    let used = old.round().clamp(0.0, 100.0) as i32;
                    let text = (if m.show_left { 100 - used } else { used }).to_string();
                    (text, ease_out_cubic(t * 1.2))
                })
            });
            if let Some((old, progress)) = old_number.filter(|(old, _)| *old != number) {
                paint_text(
                    frame,
                    &old,
                    number_rect,
                    s(13.0),
                    true,
                    Rgba(
                        number_color.0,
                        number_color.1,
                        number_color.2,
                        number_color.3 * (1.0 - progress),
                    ),
                    DT_RIGHT,
                );
                paint_text(
                    frame,
                    &number,
                    number_rect,
                    s(13.0),
                    true,
                    Rgba(
                        number_color.0,
                        number_color.1,
                        number_color.2,
                        number_color.3 * progress,
                    ),
                    DT_RIGHT,
                );
            } else {
                paint_text(
                    frame,
                    &number,
                    number_rect,
                    s(13.0),
                    true,
                    number_color,
                    DT_RIGHT,
                );
            }
        }
        let bar_top = item_y + s(19.0);
        let bar_bottom = bar_top + s(3.0).max(1);
        let track_alpha = opacity * if loading { pulse } else { 1.0 };
        paint_rect(
            frame,
            (item_x, bar_top, item_x + item_width, bar_bottom),
            1.5 * scale,
            Rgba(pal.track.0, pal.track.1, pal.track.2, track_alpha),
        );
        if !loading && value.is_some() {
            if let Some(elapsed) = elapsed {
                let right = item_x + (item_width as f32 * elapsed.clamp(0.0, 1.0)).round() as i32;
                paint_rect(
                    frame,
                    (item_x, bar_top, right, bar_bottom),
                    1.5 * scale,
                    Rgba(pal.elapsed.0, pal.elapsed.1, pal.elapsed.2, opacity),
                );
            }
            let target = value.unwrap_or_default().clamp(0.0, 100.0) as f32 / 100.0;
            let fraction = row.and_then(|row| row.roll).map_or(target, |(from, t)| {
                let start = from[id]
                    .unwrap_or(value.unwrap_or_default())
                    .clamp(0.0, 100.0) as f32
                    / 100.0;
                start + (target - start) * ease_out_cubic(t)
            });
            let right = item_x + (item_width as f32 * fraction).round() as i32;
            paint_rect(
                frame,
                (item_x, bar_top, right, bar_bottom),
                1.5 * scale,
                Rgba(fill_color.0, fill_color.1, fill_color.2, opacity),
            );
        }
        if vertical {
            item_y += s(28.0);
        } else {
            item_x += s(38.0);
        }
    }
}

fn draw_forecast_spark(
    frame: &mut Frame,
    m: &Model,
    x: i32,
    y: i32,
    scale: f32,
    vertical: bool,
    pal: &Palette,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let Some(row) = m.spark_row.as_ref() else {
        return;
    };
    let busy = m.spark_busy && !m.spark_down;
    let color = if m.spark_down {
        pal.crit
    } else if busy {
        pal.spark
    } else {
        pal.label
    };
    let icon = draw_spark_mark(s(16.0));
    let icon_x = if vertical {
        x + (s(104.0) - s(16.0)) / 2
    } else {
        x
    };
    let icon_y = if vertical { y } else { (frame.h - s(16.0)) / 2 };
    paint_mask(frame, icon_x, icon_y, s(16.0), &icon, color);
    let value = if m.spark_down {
        "down".to_owned()
    } else if !busy {
        "idle".to_owned()
    } else {
        format!("{:.0}", row.five.unwrap_or_default())
    };
    let value_x = if vertical { x } else { x + s(22.0) };
    let value_y = if vertical {
        y + s(24.0)
    } else {
        (frame.h - s(15.0)) / 2
    };
    let value_width = unsafe { measure_text_width(&value, s(13.0), true) };
    let unit_width = unsafe { measure_text_width("t/s", s(10.0), false) };
    unsafe {
        paint_text(
            frame,
            &value,
            RECT {
                left: value_x,
                top: value_y,
                right: value_x + value_width,
                bottom: value_y + s(15.0),
            },
            s(13.0),
            true,
            if busy { pal.value } else { color },
            DT_LEFT,
        );
        if busy {
            paint_text(
                frame,
                "t/s",
                RECT {
                    left: value_x + value_width + s(3.0),
                    top: value_y,
                    right: value_x + value_width + s(3.0) + unit_width,
                    bottom: value_y + s(15.0),
                },
                s(10.0),
                false,
                pal.label,
                DT_LEFT,
            );
        }
    }
    let chart_x = if vertical {
        x + (s(104.0) - s(36.0)) / 2
    } else if busy {
        value_x + value_width + s(3.0) + unit_width + s(6.0)
    } else {
        value_x + value_width + s(6.0)
    };
    let chart_y = if vertical {
        value_y + s(21.0)
    } else {
        (frame.h - s(18.0)) / 2
    };
    draw_strip_sparkline(
        frame,
        chart_x,
        chart_y,
        scale,
        &m.spark_points,
        busy,
        m.spark_down,
        m.spark_pulse_alpha,
        pal,
    );
}

fn draw_strip_sparkline(
    frame: &mut Frame,
    x: i32,
    y: i32,
    scale: f32,
    points: &[f32],
    busy: bool,
    down: bool,
    pulse: f32,
    pal: &Palette,
) {
    let s = |value: f32| (value * scale).round() as i32;
    let (width, height) = (s(36.0), s(18.0));
    if !busy || down || points.len() < 2 {
        paint_rect(
            frame,
            (x, y + height - s(1.0), x + width, y + height),
            0.0,
            pal.track,
        );
        return;
    }
    let peak = points.iter().copied().fold(1.0, f32::max);
    let coords = points
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let px =
                x + (index as f32 * (width - 1) as f32 / (points.len() - 1) as f32).round() as i32;
            let py = y + s(17.0) - (value / peak * s(14.0) as f32).round() as i32;
            (px, py)
        })
        .collect::<Vec<_>>();
    for column in 0..width {
        let position = column as f32 * (coords.len() - 1) as f32 / (width - 1).max(1) as f32;
        let index = position.floor() as usize;
        let t = position - index as f32;
        let a = coords[index.min(coords.len() - 1)];
        let b = coords[(index + 1).min(coords.len() - 1)];
        let curve_y = a.1 as f32 + (b.1 - a.1) as f32 * t;
        let px = x + column;
        for py in curve_y.ceil() as i32..y + height {
            let depth =
                ((py as f32 - curve_y) / ((y + height) as f32 - curve_y).max(1.0)).clamp(0.0, 1.0);
            over_pixel(
                &mut frame.px[(py * frame.w + px) as usize],
                pal.spark,
                0.38 * (1.0 - depth) + 0.02 * depth,
            );
        }
        for py in curve_y.round() as i32 - s(1.0)..=curve_y.round() as i32 + s(1.0) {
            if py >= 0 && py < frame.h {
                let coverage =
                    (1.25 * scale + 0.5 - (py as f32 + 0.5 - curve_y).abs()).clamp(0.0, 1.0);
                over_pixel(
                    &mut frame.px[(py * frame.w + px) as usize],
                    pal.spark,
                    coverage,
                );
            }
        }
    }
    let (last_x, last_y) = *coords.last().unwrap();
    for py in last_y - s(5.0)..=last_y + s(5.0) {
        for px in last_x - s(5.0)..=last_x + s(5.0) {
            if px < 0 || px >= frame.w || py < 0 || py >= frame.h {
                continue;
            }
            let distance = (((px - last_x).pow(2) + (py - last_y).pow(2)) as f32).sqrt();
            if distance <= 3.85 * scale {
                over_pixel(
                    &mut frame.px[(py * frame.w + px) as usize],
                    Rgba(pal.spark.0, pal.spark.1, pal.spark.2, 0.22 * pulse),
                    1.0,
                );
            }
            if distance <= 1.75 * scale {
                over_pixel(&mut frame.px[(py * frame.w + px) as usize], pal.spark, 1.0);
            }
        }
    }
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

fn decorate_spark(
    f: &mut Frame,
    points: &[f32],
    busy: bool,
    down: bool,
    pulse: f32,
    scale: f32,
    dark: bool,
) {
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
    {
        let line_color = color;
        let x0 = old + 12;
        let graph_width = (add - 15).max(1);
        let graph_height = (15.0 * scale).round().max(8.0) as i32;
        let graph_top = (f.h - graph_height) / 2;
        let baseline = graph_top + graph_height;
        let baseline_color = if dark { [72, 72, 72] } else { [205, 205, 205] };
        for x in x0..=x0 + graph_width {
            blend_pixel(
                &mut px[(baseline * (old + add) + x) as usize],
                baseline_color,
                0.35,
            );
        }
        if points.len() < 3 {
            f.w = old + add;
            f.px = px;
            return;
        }
        let peak = points.iter().copied().fold(1.0f32, f32::max);
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
            (0.5 * ((2.0 * p1)
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
            curve.push(baseline as f32 - value / peak * graph_height as f32);
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
                    line_color,
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
                    let coverage =
                        (radius + 0.5 - (y as f32 + 0.5 - curve_y).abs()).clamp(0.0, 1.0);
                    blend_pixel(
                        &mut px[(y * (old + add) + x) as usize],
                        line_color,
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
                    let coverage = (dot_radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                    blend_pixel(
                        &mut px[(y * (old + add) + x) as usize],
                        line_color,
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
        frames.iter().map(|(_, frame)| frame.w).sum::<i32>() + gap * (frames.len() as i32 - 1)
    };
    let h = if vertical {
        frames.iter().map(|(_, frame)| frame.h).sum::<i32>() + gap * (frames.len() as i32 - 1)
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
    let Some(avail) = vert else {
        return layout(m, h, scale, false);
    };
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
        loading: false,
        pulse_alpha: 1.0,
    }];
    let rows: &[Row] = if m.rows.is_empty() { &empty } else { &m.rows };
    // Vertical: every account in one column, one line per limit.
    let rows_per_col = if vertical {
        rows.len()
    } else {
        m.rows_per_col.clamp(1, MAX_ROWS_PER_COLUMN)
    };
    let per_col = if vertical {
        1
    } else {
        rows.len().min(rows_per_col)
    };
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
        let fill = if inside && m.ring_space && !vertical {
            (row_h * 0.72).round() as i32
        } else {
            row_h as i32 - s(2.0)
        };
        let ring_d = if !m.rings {
            0
        } else {
            fill.clamp(1, s(if inside { 34.0 } else { 16.0 }))
        };
        let ring_gap = if m.rings && !inside { s(5.0) } else { 0 };
        let mut small_px = ((ring_d as f32 * 0.32).round() as i32).max(1);
        let mut f_small = make_font(
            small_px,
            &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"],
        );
        let f_label = make_font(font_px, &["Segoe UI Variable Text", "Segoe UI"]);
        let f_value = make_font(
            font_px,
            &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"],
        );

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
            let room = (ring_d as f32
                - 2.0 * (ring_d as f32 * 0.1).max(1.5)
                - 2.0 * s(m.ring_margin as f32) as f32)
                .max(1.0);
            let widest = measure(
                &(if m.hide_percent { "100" } else { "100%" })
                    .encode_utf16()
                    .collect::<Vec<u16>>(),
                f_small,
            ) as f32;
            if widest > 0.0 && (widest - room).abs() >= 0.5 {
                small_px = ((small_px as f32 * room / widest).floor() as i32).max(1);
                DeleteObject(f_small);
                f_small = make_font(
                    small_px,
                    &["Segoe UI Variable Text Semibold", "Segoe UI Semibold"],
                );
            }
        }

        let dim = |stale: bool, c: Rgba| {
            if stale {
                Rgba(c.0, c.1, c.2, c.3 * 0.5)
            } else {
                c
            }
        };
        let all_stale = rows.iter().all(|r| r.stale);
        let pad_l = s(8.0);
        let icon = if m.hide_icon {
            0
        } else {
            s(m.icon_size as f32).min(h)
        };
        let gap_icon = if m.hide_icon { 0 } else { s(7.0) };
        let gap_lv = if m.hide_labels { 0 } else { s(5.0) };
        let gap_div = s(10.0);
        let div_w = s(1.0).max(1);
        let pad_r = s(4.0);
        let div_h = if per_col == 1 {
            s(20.0)
        } else {
            (row_h as i32 - s(2.0)).max(1)
        };

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
            let ids: Vec<usize> = [
                !m.hide_five,
                !m.hide_week,
                !m.hide_fable && rows.iter().any(|r| r.fable.is_some()),
            ]
            .iter()
            .enumerate()
            .filter_map(|(k, on)| on.then_some(k))
            .collect();
            let groups = ids.len();
            let mut label_w: Vec<i32> = ids
                .iter()
                .map(|&k| &labels[k])
                .map(|l| {
                    if m.hide_labels {
                        0
                    } else {
                        measure(l, f_label)
                    }
                })
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
                    value_w[g] =
                        value_w[g].max(measure(&v[g], if inside { f_small } else { f_value }));
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
                x = pad_l
                    + lw
                    + gap_lv
                    + if inside {
                        ring_d
                    } else {
                        ring_d + ring_gap + vw
                    };
            } else {
                for g in 0..groups {
                    group_x[g] = x;
                    x += label_w[g]
                        + gap_lv
                        + if inside {
                            ring_d
                        } else {
                            ring_d + ring_gap + value_w[g]
                        }
                        + gap_div;
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
                        segs.push(Seg {
                            text: labels[ids[g]].clone(),
                            bold: false,
                            small: false,
                            x: group_x[g],
                            w: label_w[g],
                            y0,
                            y1,
                            color: pal.label,
                            blur: 0,
                            slack: s(2.0),
                        });
                    }
                    let dy = y0 + ((y1 - y0) - div_h) / 2;
                    if !vertical && g + 1 < groups {
                        dividers.push(Divider {
                            x0: div_x[g],
                            x1: div_x[g] + div_w,
                            y0: dy.max(0),
                            y1: (dy + div_h).min(h),
                        });
                    }
                    let content_x = group_x[g] + label_w[g] + gap_lv;
                    let pi = ids[g];
                    let color = displayed_value_color(
                        m.provider_mark,
                        val,
                        row.critical[pi],
                        row.pace[pi],
                        row.projected[pi],
                        &pal,
                    );
                    if m.rings {
                        // The ring shows the same quantity as the number.
                        let frac = |v: Option<f64>| {
                            let used =
                                v.map_or(0.0, |q| q.round().clamp(0.0, 100.0) as f32 / 100.0);
                            if v.is_some() && m.show_left {
                                1.0 - used
                            } else {
                                used
                            }
                        };
                        let to = frac(val);
                        rings.push(Ring {
                            x: content_x,
                            y: y0 + ((y1 - y0) - ring_d) / 2,
                            frac: row.roll.map_or(to, |(from, _)| {
                                frac(from[ids[g]]) + (to - frac(from[ids[g]])) * p
                            }),
                            color: dim(
                                row.stale,
                                if color == pal.value { RING_BLUE } else { color },
                            ),
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
                    let color = dim(
                        row.stale,
                        if m.rings && m.ring_color_only {
                            pal.value
                        } else {
                            color
                        },
                    );
                    let old = row
                        .roll
                        .map(|(from, _)| {
                            enc(&value_text(from[ids[g]], m.show_left, m.hide_percent))
                        })
                        .filter(|o| *o != vals[g]);
                    let Some(old) = old else {
                        let vw = measure(&vals[g], vf);
                        segs.push(Seg {
                            text: vals[g].clone(),
                            bold: true,
                            small: inside,
                            x: text_x(&vals[g]),
                            w: vw,
                            y0,
                            y1,
                            color,
                            blur: 0,
                            slack: s(2.0),
                        });
                        continue;
                    };
                    // Rolling: each changed character's old glyph leaves upward while the new
                    // one arrives from below, one line apart, each drawn in its own line box.
                    let mut line =
                        ((if inside { small_px } else { font_px }) as f32 * 1.25).round() as i32;
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
                dividers.push(Divider {
                    x0: pad_l,
                    x1: w - pad_l,
                    y0: hy,
                    y1: hy + div_w,
                });
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
            SelectObject(
                dc,
                if seg.small {
                    f_small
                } else if seg.bold {
                    f_value
                } else {
                    f_label
                },
            );
            let mut rc = RECT {
                left: seg.x,
                top: seg.y0,
                right: seg.x + seg.w + seg.slack,
                bottom: seg.y1,
            };
            DrawTextW(
                dc,
                seg.text.as_ptr(),
                seg.text.len() as i32,
                &mut rc,
                DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_NOPREFIX,
            );
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
                    let cov = rounded_rect_cov(
                        xx as f32 + 0.5,
                        y as f32 + 0.5,
                        0.0,
                        top,
                        w as f32,
                        bottom,
                        r,
                    );
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
                    let band =
                        (stroke / 2.0 + 0.5 - (d - (rad - stroke / 2.0)).abs()).clamp(0.0, 1.0);
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
            if let Some(p) = r.pace {
                let a = p.clamp(0.0, 1.0) * std::f32::consts::TAU;
                let (sn, cs) = a.sin_cos();
                for q in 0..stroke.ceil() as i32 + 2 {
                    let rr = rad - q as f32;
                    let xx = (r.x as f32 + rad + sn * rr).round() as i32;
                    let yy = (r.y as f32 + rad - cs * rr).round() as i32;
                    if xx >= 0 && xx < w && yy >= 0 && yy < h {
                        over(&mut buf, (yy * w + xx) as usize, pal.label, 1.0);
                    }
                }
            }
        }

        // Provider mark.
        let iy = if vertical { pad_v } else { (h - icon) / 2 };
        let mark = match m.provider_mark {
            1 => draw_codex_mark(icon),
            2 => draw_spark_mark(icon),
            _ if m.pixel_icon => draw_pixel_mark(icon),
            _ => draw_mark(icon),
        };
        let mark_color = match m.provider_mark {
            2 if m.spark_down => pal.crit,
            2 if m.spark_busy => SPARK_GREEN,
            2 => pal.label,
            1 => RING_BLUE,
            _ => CLAUDE_ORANGE,
        };
        for y in 0..icon {
            for xx in 0..icon {
                let cov = mark[(y * icon + xx) as usize];
                over(
                    &mut buf,
                    ((iy + y) * w + icon_x + xx) as usize,
                    dim(all_stale, mark_color),
                    cov,
                );
            }
        }

        // Text.
        for seg in &segs {
            for y in seg.y0.max(0)..seg.y1.min(h) {
                for xx in seg.x.max(0)..(seg.x + seg.w + seg.slack).min(w) {
                    let i = (y * w + xx) as usize;
                    // Motion blur: a vertical box average kept inside the glyph's own line box.
                    let mut sum = [0u32; 3];
                    for yy in (y - seg.blur..=y + seg.blur)
                        .filter(|yy| (seg.y0.max(0)..seg.y1.min(h)).contains(yy))
                    {
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
        Frame {
            w,
            h,
            px,
            groups: Vec::new(),
        }
    }
}

pub(crate) fn draw_codex_mark(size: i32) -> Vec<f32> {
    let mut out = vec![0.0; (size * size) as usize];
    let n = size as f32;
    let points = std::array::from_fn::<_, 6, _>(|index| {
        let angle = (30.0 + index as f32 * 60.0).to_radians();
        (
            n * (0.5 + 0.43 * angle.cos()),
            n * (0.5 + 0.43 * angle.sin()),
        )
    });
    let stroke = (n * 0.085).max(1.3);
    let prompt = [
        ((n * 0.33, n * 0.38), (n * 0.45, n * 0.50)),
        ((n * 0.45, n * 0.50), (n * 0.33, n * 0.62)),
        ((n * 0.52, n * 0.63), (n * 0.67, n * 0.63)),
    ];
    let distance = |px: f32, py: f32, a: (f32, f32), b: (f32, f32)| {
        let vx = b.0 - a.0;
        let vy = b.1 - a.1;
        let length_sq = vx * vx + vy * vy;
        let t = (((px - a.0) * vx + (py - a.1) * vy) / length_sq).clamp(0.0, 1.0);
        let dx = px - (a.0 + t * vx);
        let dy = py - (a.1 + t * vy);
        (dx * dx + dy * dy).sqrt()
    };
    let samples = 4;
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..samples {
                for sx in 0..samples {
                    let px = x as f32 + (sx as f32 + 0.5) / samples as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / samples as f32;
                    let mut d = f32::MAX;
                    for i in 0..points.len() {
                        d = d.min(distance(px, py, points[i], points[(i + 1) % points.len()]));
                    }
                    for &(a, b) in &prompt {
                        d = d.min(distance(px, py, a, b));
                    }
                    if d <= stroke * 0.5 {
                        hits += 1;
                    }
                }
            }
            out[(y * size + x) as usize] = hits as f32 / (samples * samples) as f32;
        }
    }
    out
}
pub(crate) fn draw_spark_mark(size: i32) -> Vec<f32> {
    let mut out = vec![0.0; (size * size) as usize];
    let n = size as f32;
    let (left, top, right, bottom) = (n * 0.24, n * 0.24, n * 0.76, n * 0.76);
    let radius = n * 0.08;
    let stroke = (n * 0.09).max(1.3);
    let pin_half = stroke * 0.5;
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
            let center = rounded_rect_cov(px, py, n * 0.41, n * 0.41, n * 0.59, n * 0.59, n * 0.03);
            coverage = coverage.max(center);
            for position in [0.38, 0.62] {
                let along = n * position;
                let horizontal = py >= along - pin_half
                    && py <= along + pin_half
                    && ((px >= n * 0.06 && px <= left) || (px >= right && px <= n * 0.94));
                let vertical = px >= along - pin_half
                    && px <= along + pin_half
                    && ((py >= n * 0.06 && py <= top) || (py >= bottom && py <= n * 0.94));
                if horizontal || vertical {
                    coverage = 1.0;
                }
            }
            out[(y * size + x) as usize] = coverage;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_value_color_ignores_throughput_as_percent() {
        let palette = palette(true);
        assert_eq!(
            displayed_value_color(2, Some(148.0), false, None, None, &palette),
            palette.value
        );
        assert_eq!(
            displayed_value_color(2, Some(148.0), true, None, None, &palette),
            palette.crit
        );
    }

    #[test]
    fn strip_state_uses_pace_projection_and_critical_rules() {
        assert_eq!(
            strip_item_state(Some(42.0), Some(0.50), Some(84.0), false),
            ItemState::Ok
        );
        assert_eq!(
            strip_item_state(Some(62.0), Some(0.40), Some(88.0), false),
            ItemState::Warn
        );
        assert_eq!(
            strip_item_state(Some(40.0), Some(0.20), Some(100.0), false),
            ItemState::Crit
        );
        assert_eq!(
            strip_item_state(Some(96.0), Some(0.99), Some(97.0), false),
            ItemState::Crit
        );
        assert_eq!(
            strip_item_state(Some(20.0), Some(0.20), Some(20.0), true),
            ItemState::Crit
        );
    }

    #[test]
    fn ok_strip_number_uses_text_while_fill_uses_provider_accent() {
        for dark in [true, false] {
            let palette = palette(dark);
            assert_eq!(
                number_color_for_state(ItemState::Ok, &palette),
                palette.value
            );
            assert_eq!(
                color_for_state(ItemState::Ok, palette.claude, &palette),
                palette.claude
            );
            assert_eq!(
                number_color_for_state(ItemState::Warn, &palette),
                palette.warn
            );
            assert_eq!(
                number_color_for_state(ItemState::Crit, &palette),
                palette.crit
            );
        }
    }

    #[test]
    fn segment_midpoints_follow_used_projected_elapsed_order() {
        assert_eq!(segment_band(0, 12.0, 37.0, 62.0), SegmentBand::Used);
        assert_eq!(segment_band(2, 12.0, 37.0, 62.0), SegmentBand::Projected);
        assert_eq!(segment_band(7, 12.0, 37.0, 62.0), SegmentBand::Elapsed);
        assert_eq!(segment_band(12, 12.0, 37.0, 62.0), SegmentBand::Track);
    }

    #[test]
    fn ease_out_reaches_both_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5);
    }
}

fn rounded_rect_cov(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> f32 {
    let rad = rad.min((r - l).max(0.0) * 0.5).min((b - t).max(0.0) * 0.5);
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
pub(crate) fn draw_mark(size: i32) -> Vec<f32> {
    const SS: i32 = 4; // supersampling
    let n = 12;
    let c = size as f32 / 2.0;
    let stroke = (size as f32 * 0.10).max(1.4);
    let spokes: Vec<((f32, f32), (f32, f32))> = (0..n)
        .map(|i| {
            let angle = i as f32 * std::f32::consts::TAU / n as f32;
            let inner = size as f32 * 0.10;
            let outer = size as f32 * if i % 2 == 0 { 0.47 } else { 0.36 };
            (
                (angle.cos() * inner, angle.sin() * inner),
                (angle.cos() * outer, angle.sin() * outer),
            )
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
                    let inside = spokes.iter().any(|&(start, end)| {
                        let vx = end.0 - start.0;
                        let vy = end.1 - start.1;
                        let t = (((px - start.0) * vx + (py - start.1) * vy) / (vx * vx + vy * vy))
                            .clamp(0.0, 1.0);
                        let (dx, dy) = (px - start.0 - vx * t, py - start.1 - vy * t);
                        dx * dx + dy * dy <= (stroke * 0.5).powi(2)
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

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardGlyph {
    Refresh,
    Pin,
    PinFilled,
    Close,
    Warning,
}

pub(crate) fn draw_card_glyph(size: i32, glyph: CardGlyph) -> Vec<f32> {
    const SS: i32 = 4;
    let scale = size as f32 / 14.0;
    let line_distance = |px: f32, py: f32, a: (f32, f32), b: (f32, f32)| {
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let length = vx * vx + vy * vy;
        let t = if length == 0.0 {
            0.0
        } else {
            (((px - a.0) * vx + (py - a.1) * vy) / length).clamp(0.0, 1.0)
        };
        let (dx, dy) = (px - a.0 - t * vx, py - a.1 - t * vy);
        (dx * dx + dy * dy).sqrt()
    };
    let mut output = vec![0.0; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / scale;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / scale;
                    let stroke = 0.65;
                    let inside = match glyph {
                        CardGlyph::Close => {
                            line_distance(px, py, (3.5, 3.5), (10.5, 10.5)) <= stroke
                                || line_distance(px, py, (10.5, 3.5), (3.5, 10.5)) <= stroke
                        }
                        CardGlyph::Pin | CardGlyph::PinFilled => {
                            let circle = ((px - 7.0).powi(2) + (py - 5.0).powi(2)).sqrt();
                            (matches!(glyph, CardGlyph::PinFilled) && circle <= 2.8)
                                || (circle - 2.8).abs() <= stroke
                                || line_distance(px, py, (7.0, 7.8), (7.0, 12.5)) <= stroke
                        }
                        CardGlyph::Warning => {
                            let edges = [
                                ((6.0, 1.5), (11.0, 10.5)),
                                ((11.0, 10.5), (1.0, 10.5)),
                                ((1.0, 10.5), (6.0, 1.5)),
                            ];
                            edges
                                .iter()
                                .any(|&(a, b)| line_distance(px, py, a, b) <= stroke)
                                || line_distance(px, py, (6.0, 5.0), (6.0, 7.3)) <= stroke
                                || ((px - 6.0).powi(2) + (py - 9.0).powi(2)).sqrt() <= 0.75
                        }
                        CardGlyph::Refresh => {
                            let dx = px - 7.0;
                            let dy = py - 7.0;
                            let angle = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                            let arc = (dx * dx + dy * dy).sqrt();
                            ((arc - 4.5).abs() <= stroke && (angle >= 45.0 || angle <= 315.0))
                                || line_distance(px, py, (10.2, 1.4), (10.2, 3.9)) <= stroke
                                || line_distance(px, py, (10.2, 3.9), (7.7, 3.9)) <= stroke
                        }
                    };
                    if inside {
                        hits += 1;
                    }
                }
            }
            output[(y * size + x) as usize] = hits as f32 / (SS * SS) as f32;
        }
    }
    output
}
