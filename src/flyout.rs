use crate::http::wide;
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use windows_sys::core::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_REFRESH: u32 = WM_APP + 30;
pub const WM_OPEN_PROVIDER: u32 = WM_APP + 31;
pub const WM_DISMISSED: u32 = WM_APP + 32;
const CLASS: *const u16 = w!("TaskbarUsageFlyout");
const WM_MOUSE_LEAVE_MSG: u32 = 0x02A3;
const MIN_WIDTH: f32 = 360.0;
const MAX_WIDTH: f32 = 560.0;
const WORK_MARGIN: f32 = 12.0;

#[repr(usize)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    #[default]
    Claude,
    Codex,
    Spark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderHit {
    Close,
    Pin,
    Refresh,
    Provider(ProviderKind),
    None,
}

#[derive(Clone, Default)]
pub struct Bar {
    pub label: String,
    pub percent: f64,
    pub elapsed: f64,
    pub reset: String,
    pub ahead_by: f64,
    pub projected: f64,
    pub full_at: Option<String>,
}

#[derive(Clone, Default)]
pub struct AuxStatus {
    pub name: String,
    pub up: bool,
    pub busy: bool,
}

#[derive(Clone, Default)]
pub struct SparkStats {
    pub tok_s: f64,
    pub peak: f64,
    pub running: i64,
    pub waiting: i64,
    pub kv_pct: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub gpu_util: f64,
    pub gpu_temp: Option<f64>,
    pub gpu_power: Option<f64>,
    pub mem_pct: Option<f64>,
    pub swap_pct: Option<f64>,
    pub aux: Vec<AuxStatus>,
}

#[derive(Clone, Default)]
pub struct Section {
    pub provider: ProviderKind,
    pub title: String,
    pub chip: String,
    pub bars: Vec<Bar>,
    pub chart: Vec<f64>,
    pub today: String,
    pub as_of: String,
    pub error: Option<String>,
    pub stale: bool,
    pub spark: Option<SparkStats>,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub sections: Vec<Section>,
    pub updated: String,
}

#[allow(dead_code)]
fn worst_limit_index(bars: &[Bar]) -> Option<usize> {
    bars.iter()
        .enumerate()
        .max_by_key(|(_, bar)| {
            let state = crate::render::strip_item_state(
                Some(bar.percent),
                Some((bar.elapsed / 100.0) as f32),
                Some(bar.projected as f32),
                bar.full_at.is_some(),
            );
            match state {
                crate::render::ItemState::Ok => 0,
                crate::render::ItemState::Warn => 1,
                crate::render::ItemState::Crit => 2,
            }
        })
        .map(|(index, _)| index)
}

#[allow(dead_code)]
fn provider_status_text(provider: &str, bars: &[Bar]) -> String {
    let Some(bar) = worst_limit_index(bars).and_then(|index| bars.get(index)) else {
        return format!("{provider} · No readings");
    };
    if let Some(time) = &bar.full_at {
        format!("{provider} · Runs out {time}")
    } else {
        format!("{provider} · On pace")
    }
}

#[allow(dead_code)]
fn age_copy(seconds: i64, stale: bool) -> String {
    let seconds = seconds.max(0);
    let value = if seconds >= 3600 {
        format!("{} h", seconds / 3600)
    } else if seconds >= 60 {
        format!("{} m", seconds / 60)
    } else {
        format!("{} s", seconds)
    };
    format!("{value} {}", if stale { "old" } else { "ago" })
}

#[allow(dead_code)]
fn reset_time_copy(today: bool, weekday: &str, time: &str) -> String {
    if today {
        time.to_owned()
    } else {
        format!("{weekday} {time}")
    }
}

#[derive(Clone)]
struct View {
    snapshot: Snapshot,
    dark: bool,
    dpi: u32,
    width: i32,
}

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(null_mut()) };
    static OWNER: Cell<HWND> = const { Cell::new(null_mut()) };
    static HOVER: Cell<bool> = const { Cell::new(false) };
    static PINNED: Cell<bool> = const { Cell::new(false) };
    static CLICK_MODE: Cell<bool> = const { Cell::new(false) };
    static HOVER_HIT: Cell<HeaderHit> = const { Cell::new(HeaderHit::None) };
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
}

fn control_rects(width: i32, scale: f32) -> [(HeaderHit, RECT); 3] {
    let pad = (16.0 * scale).round() as i32;
    let size = (20.0 * scale).round() as i32;
    let gap = (4.0 * scale).round() as i32;
    let top = (8.0 * scale).round() as i32;
    let bottom = top + size;
    let close_right = width - pad;
    let close = RECT {
        left: close_right - size,
        top,
        right: close_right,
        bottom,
    };
    let pin = RECT {
        left: close.left - gap - size,
        top,
        right: close.left - gap,
        bottom,
    };
    let refresh = RECT {
        left: pin.left - gap - size,
        top,
        right: pin.left - gap,
        bottom,
    };
    [
        (HeaderHit::Close, close),
        (HeaderHit::Pin, pin),
        (HeaderHit::Refresh, refresh),
    ]
}

fn contains(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

pub fn header_hit_test(x: i32, y: i32, width: i32, scale: f32, sections: &[Section]) -> HeaderHit {
    for (hit, rect) in control_rects(width, scale) {
        if contains(rect, x, y) {
            return hit;
        }
    }
    let pad = (16.0 * scale).round() as i32;
    let provider_right = pad + (132.0 * scale).round() as i32;
    let mut top = (44.0 * scale).round() as i32;
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            top += (15.0 * scale).round() as i32;
        }
        let bottom = top + (24.0 * scale).round() as i32;
        if x >= pad && x < provider_right && y >= top && y < bottom {
            return HeaderHit::Provider(section.provider);
        }
        top += (section_height(section) as f32 * scale).round() as i32;
    }
    HeaderHit::None
}

pub unsafe fn register() {
    let class = WNDCLASSW {
        lpfnWndProc: Some(proc_),
        hInstance: GetModuleHandleW(null()),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        lpszClassName: CLASS,
        ..std::mem::zeroed()
    };
    RegisterClassW(&class);
}

pub fn is_hovered() -> bool {
    HOVER.with(Cell::get)
}
pub fn is_pinned() -> bool {
    PINNED.with(Cell::get)
}
pub fn is_click_mode() -> bool {
    CLICK_MODE.with(Cell::get)
}
pub fn is_persistent() -> bool {
    is_pinned() || is_click_mode()
}
pub fn is_open() -> bool {
    WINDOW.with(|window| unsafe { !window.get().is_null() && IsWindowVisible(window.get()) != 0 })
}
pub unsafe fn hide() {
    if !is_persistent() {
        WINDOW.with(|window| ShowWindow(window.get(), SW_HIDE));
    }
}

pub unsafe fn close() {
    PINNED.with(|pinned| pinned.set(false));
    CLICK_MODE.with(|mode| mode.set(false));
    HOVER_HIT.with(|hit| hit.set(HeaderHit::None));
    WINDOW.with(|window| {
        if !window.get().is_null() {
            ShowWindow(window.get(), SW_HIDE);
        }
    });
}

pub unsafe fn show(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND) {
    show_impl(snapshot, anchor, dark, owner, is_click_mode());
}

pub unsafe fn show_click(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND) {
    CLICK_MODE.with(|mode| mode.set(true));
    show_impl(snapshot, anchor, dark, owner, true);
}

unsafe fn show_impl(snapshot: &Snapshot, anchor: RECT, dark: bool, owner: HWND, click: bool) {
    let hwnd = WINDOW.with(|slot| {
        let mut hwnd = slot.get();
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!("Usage details"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            );
            slot.set(hwnd);
        }
        hwnd
    });
    OWNER.with(|slot| slot.set(owner));
    if hwnd.is_null() {
        return;
    }
    let dpi = GetDpiForWindow(owner).max(96);
    let pinned_rect = if is_pinned() && IsWindowVisible(hwnd) != 0 {
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        Some(rect)
    } else {
        None
    };
    let width = pinned_rect
        .map(|rect| rect.right - rect.left)
        .unwrap_or_else(|| desired_width(anchor, dpi));
    VIEW.with(|view| {
        *view.borrow_mut() = Some(View {
            snapshot: snapshot.clone(),
            dark,
            dpi,
            width,
        });
    });
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    let style = if click {
        style & !WS_EX_NOACTIVATE
    } else {
        style | WS_EX_NOACTIVATE
    };
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
    SetWindowPos(
        hwnd,
        null_mut(),
        0,
        0,
        0,
        0,
        SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let frame = render_with_hover(
        snapshot,
        dark,
        dpi,
        is_pinned(),
        HOVER_HIT.with(Cell::get),
        width,
    );
    let point = pinned_rect.map_or_else(
        || position(anchor, frame.0, frame.1, dpi),
        |rect| POINT {
            x: rect.left,
            y: rect.top,
        },
    );
    present(hwnd, point, frame);
    if click {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    } else {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

unsafe fn monitor_info(anchor: RECT) -> MONITORINFO {
    let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(monitor, &mut info);
    info
}

unsafe fn desired_width(anchor: RECT, dpi: u32) -> i32 {
    let info = monitor_info(anchor);
    let work = info.rcWork;
    let scale = dpi as f32 / 96.0;
    let margin = (WORK_MARGIN * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;
    let min = (MIN_WIDTH * scale).round() as i32;
    let max = (MAX_WIDTH * scale).round() as i32;
    let horizontal = anchor.right - anchor.left >= anchor.bottom - anchor.top;
    let available = if horizontal {
        work.right - margin - anchor.left
    } else if anchor.right <= work.left + 4 {
        work.right - margin - anchor.right - gap
    } else if anchor.left >= work.right - 4 {
        anchor.left - gap - work.left - margin
    } else {
        work.right - margin - anchor.left
    };
    available.clamp(min, max)
}

unsafe fn position(anchor: RECT, width: i32, height: i32, dpi: u32) -> POINT {
    let info = monitor_info(anchor);
    let (work, screen) = (info.rcWork, info.rcMonitor);
    let scale = dpi as f32 / 96.0;
    let margin = (WORK_MARGIN * scale).round() as i32;
    let gap = (8.0 * scale).round() as i32;
    let horizontal = anchor.right - anchor.left >= anchor.bottom - anchor.top;
    let available = work.right - margin - anchor.left;
    let mut x = if horizontal && available > width {
        work.right - margin - width
    } else {
        anchor.left
    };
    let mut y = anchor.top - height - gap;
    if anchor.bottom <= work.top + 4 {
        y = anchor.bottom + gap;
    } else if anchor.right <= work.left + 4 {
        x = anchor.right + gap;
        y = anchor.top;
    } else if anchor.left >= work.right - 4 {
        x = anchor.left - width - gap;
        y = anchor.top;
    } else if anchor.top < screen.top + height {
        y = anchor.bottom + gap;
    }
    let right_bound = if horizontal {
        work.right - margin
    } else {
        work.right
    };
    POINT {
        x: x.clamp(work.left, (right_bound - width).max(work.left)),
        y: y.clamp(work.top, (work.bottom - height).max(work.top)),
    }
}

pub fn render(snapshot: &Snapshot, dark: bool, dpi: u32, pinned: bool) -> (i32, i32, Vec<u32>) {
    let width = (MIN_WIDTH * dpi as f32 / 96.0).round() as i32;
    render_at_width(snapshot, dark, dpi, pinned, width)
}

pub fn render_at_width(
    snapshot: &Snapshot,
    dark: bool,
    dpi: u32,
    pinned: bool,
    width: i32,
) -> (i32, i32, Vec<u32>) {
    render_with_hover(snapshot, dark, dpi, pinned, HeaderHit::None, width)
}

fn render_with_hover(
    snapshot: &Snapshot,
    dark: bool,
    dpi: u32,
    pinned: bool,
    hovered: HeaderHit,
    width: i32,
) -> (i32, i32, Vec<u32>) {
    let scale = dpi as f32 / 96.0;
    let sections: i32 = snapshot.sections.iter().map(section_height).sum();
    let gaps = snapshot.sections.len().saturating_sub(1) as i32 * 15;
    let height = ((16 + 28 + sections + gaps + 16) as f32 * scale).round() as i32;
    let mut pixels = vec![0u32; (width * height) as usize];
    let radius = (10.0 * scale) as i32;
    let card = if dark { (32, 32, 34) } else { (248, 248, 250) };
    let border = if dark { (72, 72, 76) } else { (205, 205, 210) };
    for y in 0..height {
        for x in 0..width {
            if rounded(x, y, width, height, radius) {
                let edge = !rounded(x - 1, y - 1, width - 2, height - 2, radius - 1);
                let (red, green, blue) = if edge { border } else { card };
                pixels[(y * width + x) as usize] =
                    (247u32 << 24) | ((red as u32) << 16) | ((green as u32) << 8) | blue as u32;
            }
        }
    }
    unsafe {
        draw_content(
            &mut pixels,
            width,
            height,
            snapshot,
            dark,
            scale,
            pinned,
            hovered,
        )
    };
    (width, height, pixels)
}

fn section_height(section: &Section) -> i32 {
    let mut height = 24 + section.bars.len() as i32 * 55;
    height += match section.provider {
        ProviderKind::Claude => 78,
        ProviderKind::Codex => 20,
        ProviderKind::Spark => 192,
    };
    if section.error.is_some() || section.stale {
        height += 18;
    }
    height
}

fn rounded(x: i32, y: i32, width: i32, height: i32, radius: i32) -> bool {
    if x < 0 || y < 0 || x >= width || y >= height {
        return false;
    }
    let cx = if x < radius {
        radius
    } else if x >= width - radius {
        width - radius - 1
    } else {
        x
    };
    let cy = if y < radius {
        radius
    } else if y >= height - radius {
        height - radius - 1
    } else {
        y
    };
    (x - cx) * (x - cx) + (y - cy) * (y - cy) <= radius * radius
}

unsafe fn draw_content(
    pixels: &mut [u32],
    width: i32,
    height: i32,
    snapshot: &Snapshot,
    dark: bool,
    scale: f32,
    pinned: bool,
    hovered: HeaderHit,
) {
    let dc = CreateCompatibleDC(null_mut());
    let mut bits = null_mut();
    let info = bitmap_info(width, height);
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
    let old_bitmap = SelectObject(dc, bitmap);
    SetBkMode(dc, TRANSPARENT as i32);

    let normal = make_font(12.0, FW_NORMAL as i32, scale);
    let small = make_font(10.5, FW_NORMAL as i32, scale);
    let semibold = make_font(13.0, FW_SEMIBOLD as i32, scale);
    let header = make_font(14.0, FW_SEMIBOLD as i32, scale);
    let big = make_font(17.0, FW_SEMIBOLD as i32, scale);
    let text = if dark {
        rgb(235, 235, 238)
    } else {
        rgb(30, 30, 34)
    };
    let muted = if dark {
        rgb(164, 164, 172)
    } else {
        rgb(95, 95, 104)
    };
    let track = if dark {
        rgb(66, 66, 72)
    } else {
        rgb(224, 224, 229)
    };
    let separator = if dark {
        rgb(55, 55, 59)
    } else {
        rgb(220, 220, 224)
    };
    let tile = if dark {
        rgb(42, 42, 46)
    } else {
        rgb(238, 238, 241)
    };
    let card_bg = if dark {
        rgb(32, 32, 34)
    } else {
        rgb(248, 248, 250)
    };
    let green = rgb(118, 185, 0);
    let dim_green = if dark {
        rgb(126, 174, 91)
    } else {
        rgb(65, 120, 28)
    };
    let amber = rgb(224, 148, 55);
    let red = if dark {
        rgb(255, 123, 114)
    } else {
        rgb(196, 43, 28)
    };
    let pad = (16.0 * scale) as i32;
    let mut y = (14.0 * scale) as i32;

    let controls = control_rects(width, scale);
    for (hit, rect) in controls {
        if hit == hovered {
            fill_round(
                dc,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                tile,
            );
        }
    }

    SelectObject(dc, header);
    draw_text(dc, pad, y, "Usage", text);
    SelectObject(dc, small);
    let controls_left = controls[2].1.left;
    draw_text_right(dc, controls_left - 6, y + 2, &snapshot.updated, muted);
    let refresh = controls[2].1;
    draw_shared_glyph(
        dc,
        refresh.left + (4.0 * scale) as i32,
        refresh.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        crate::render::CardGlyph::Refresh,
        muted,
        card_bg,
    );
    let pin = controls[1].1;
    draw_shared_glyph(
        dc,
        pin.left + (4.0 * scale) as i32,
        pin.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        if pinned {
            crate::render::CardGlyph::PinFilled
        } else {
            crate::render::CardGlyph::Pin
        },
        muted,
        card_bg,
    );
    let close = controls[0].1;
    draw_shared_glyph(
        dc,
        close.left + (4.0 * scale) as i32,
        close.top + (4.0 * scale) as i32,
        (14.0 * scale).round() as i32,
        crate::render::CardGlyph::Close,
        muted,
        card_bg,
    );
    y += (30.0 * scale) as i32;

    for (index, section) in snapshot.sections.iter().enumerate() {
        if index > 0 {
            let line_y = y + (6.0 * scale) as i32;
            draw_line(dc, pad, line_y, width - pad, line_y, separator, 1);
            y += (15.0 * scale) as i32;
        }
        let accent = provider_accent(section.provider);
        let dimmed = section.stale || section.error.is_some();
        draw_provider_mark(
            dc,
            pad,
            y + 1,
            (14.0 * scale) as i32,
            section.provider,
            accent,
            card_bg,
        );
        SelectObject(dc, semibold);
        draw_text(
            dc,
            pad + (20.0 * scale) as i32,
            y,
            &section.title,
            if dimmed { muted } else { text },
        );
        if !section.chip.is_empty() {
            SelectObject(dc, small);
            draw_chip(dc, width - pad, y - 1, &section.chip, scale, tile, muted);
        }
        y += (24.0 * scale) as i32;

        for bar in &section.bars {
            SelectObject(dc, normal);
            draw_text(dc, pad, y, &bar.label, muted);
            let reset = if bar.reset.is_empty() {
                String::new()
            } else {
                format!(" · resets {}", bar.reset)
            };
            draw_text_right(dc, width - pad, y, &reset, muted);
            let reset_width = text_width(dc, &reset);
            SelectObject(dc, semibold);
            draw_text_right(
                dc,
                width - pad - reset_width,
                y,
                &format!("{:.0}%", bar.percent),
                if dimmed { muted } else { text },
            );

            let bar_top = y + (20.0 * scale) as i32;
            let bar_height = (6.0 * scale).round() as i32;
            let bar_width = width - 2 * pad;
            fill_round(dc, pad, bar_top, bar_width, bar_height, track);
            let fill_width =
                ((bar_width as f64 * bar.percent.clamp(0.0, 100.0) / 100.0).round() as i32).max(2);
            let fill = if section.error.is_some() {
                muted
            } else if section.stale {
                blend_color(track, accent, 0.58)
            } else if bar.percent > 95.0 {
                red
            } else if bar.percent > 75.0 {
                amber
            } else {
                accent
            };
            fill_round(dc, pad, bar_top, fill_width, bar_height, fill);
            let tick_x = pad + (bar_width as f64 * bar.elapsed.clamp(0.0, 100.0) / 100.0) as i32;
            draw_line(
                dc,
                tick_x,
                bar_top - (3.0 * scale) as i32,
                tick_x,
                bar_top + bar_height + (3.0 * scale) as i32,
                text,
                (2.0 * scale).round() as i32,
            );

            SelectObject(dc, small);
            let ahead = bar.ahead_by.round() as i64;
            let (arrow, wording, color) = if ahead > 0 {
                let ending = bar
                    .full_at
                    .as_ref()
                    .map(|at| format!("empty ~{at}"))
                    .unwrap_or_else(|| format!("ends near {:.0}%", bar.projected));
                (
                    "▲",
                    format!("{ahead} ahead of pace · {ending}"),
                    if bar.full_at.is_some() { red } else { amber },
                )
            } else {
                (
                    "▼",
                    format!("{} under pace · ends near {:.0}%", -ahead, bar.projected),
                    dim_green,
                )
            };
            let forecast_y = bar_top + bar_height + (7.0 * scale) as i32;
            draw_text(
                dc,
                pad,
                forecast_y,
                arrow,
                if dimmed { muted } else { color },
            );
            draw_text(
                dc,
                pad + (13.0 * scale) as i32,
                forecast_y,
                &wording,
                if dimmed { muted } else { color },
            );
            y += (55.0 * scale) as i32;
        }

        match section.provider {
            ProviderKind::Claude => {
                draw_area_chart(
                    dc,
                    &section.chart,
                    pad,
                    y,
                    width - pad,
                    y + (40.0 * scale) as i32,
                    accent,
                    tile,
                    separator,
                    true,
                );
                SelectObject(dc, small);
                draw_text(dc, pad, y + (42.0 * scale) as i32, "24 h", muted);
                draw_text_right(dc, width - pad, y + (42.0 * scale) as i32, "now", muted);
                y += (58.0 * scale) as i32;
                SelectObject(dc, normal);
                draw_text(
                    dc,
                    pad,
                    y,
                    &section.today,
                    if dimmed { muted } else { text },
                );
                y += (20.0 * scale) as i32;
            }
            ProviderKind::Codex => {
                SelectObject(dc, normal);
                draw_text(
                    dc,
                    pad,
                    y,
                    &section.today,
                    if dimmed { muted } else { text },
                );
                draw_text_right(dc, width - pad, y, &section.as_of, muted);
                y += (20.0 * scale) as i32;
            }
            ProviderKind::Spark => {
                if let Some(stats) = &section.spark {
                    draw_stat_grid(
                        dc,
                        stats,
                        pad,
                        y,
                        width - pad,
                        scale,
                        tile,
                        text,
                        muted,
                        big,
                        small,
                    );
                    y += (106.0 * scale) as i32;
                    draw_area_chart(
                        dc,
                        &section.chart,
                        pad,
                        y,
                        width - pad,
                        y + (44.0 * scale) as i32,
                        accent,
                        tile,
                        separator,
                        false,
                    );
                    y += (58.0 * scale) as i32;
                    draw_aux(
                        dc,
                        &stats.aux,
                        pad,
                        y,
                        width - pad,
                        scale,
                        tile,
                        text,
                        green,
                        red,
                        small,
                    );
                    y += (28.0 * scale) as i32;
                }
            }
        }
        if let Some(error) = &section.error {
            SelectObject(dc, small);
            draw_text(dc, pad, y, &format!("⚠ {error}"), muted);
            y += (18.0 * scale) as i32;
        } else if section.stale {
            SelectObject(dc, small);
            draw_text(dc, pad, y, "⚠ Data is stale", muted);
            y += (18.0 * scale) as i32;
        }
    }

    let mut read_info = info;
    GetDIBits(
        dc,
        bitmap,
        0,
        height as u32,
        pixels.as_mut_ptr().cast(),
        &mut read_info,
        DIB_RGB_COLORS,
    );
    for y in 0..height {
        for x in 0..width {
            if rounded(x, y, width, height, (10.0 * scale) as i32) {
                pixels[(y * width + x) as usize] |= 247u32 << 24;
            }
        }
    }
    SelectObject(dc, old_bitmap);
    for font in [normal, small, semibold, header, big] {
        DeleteObject(font);
    }
    DeleteObject(bitmap);
    DeleteDC(dc);
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        },
        ..unsafe { std::mem::zeroed() }
    }
}

unsafe fn make_font(size: f32, weight: i32, scale: f32) -> HFONT {
    CreateFontW(
        -(size * scale).round() as i32,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Segoe UI").as_ptr(),
    )
}
unsafe fn draw_text(dc: HDC, x: i32, y: i32, value: &str, color: COLORREF) {
    let v = wide(value);
    SetTextColor(dc, color);
    TextOutW(dc, x, y, v.as_ptr(), v.len().saturating_sub(1) as i32);
}
unsafe fn text_width(dc: HDC, value: &str) -> i32 {
    let v = wide(value);
    let mut s = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(dc, v.as_ptr(), v.len().saturating_sub(1) as i32, &mut s);
    s.cx
}
unsafe fn draw_text_right(dc: HDC, right: i32, y: i32, value: &str, color: COLORREF) {
    draw_text(dc, right - text_width(dc, value), y, value, color);
}
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

unsafe fn draw_line(dc: HDC, x1: i32, y1: i32, x2: i32, y2: i32, color: COLORREF, width: i32) {
    let pen = CreatePen(PS_SOLID, width.max(1), color);
    let old = SelectObject(dc, pen);
    MoveToEx(dc, x1, y1, null_mut());
    LineTo(dc, x2, y2);
    SelectObject(dc, old);
    DeleteObject(pen);
}
unsafe fn fill_round(dc: HDC, x: i32, y: i32, width: i32, height: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, x, y, x + width, y + height, height, height);
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(brush);
}
fn provider_accent(provider: ProviderKind) -> COLORREF {
    match provider {
        ProviderKind::Claude => rgb(217, 119, 87),
        ProviderKind::Codex => rgb(76, 154, 255),
        ProviderKind::Spark => rgb(118, 185, 0),
    }
}
unsafe fn draw_chip(
    dc: HDC,
    right: i32,
    y: i32,
    value: &str,
    scale: f32,
    fill: COLORREF,
    color: COLORREF,
) {
    let width = text_width(dc, value) + (12.0 * scale) as i32;
    let height = (18.0 * scale) as i32;
    fill_round(dc, right - width, y, width, height, fill);
    draw_text(
        dc,
        right - width + (6.0 * scale) as i32,
        y + 1,
        value,
        color,
    );
}

unsafe fn draw_provider_mark(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    provider: ProviderKind,
    color: COLORREF,
    background: COLORREF,
) {
    let mask = match provider {
        ProviderKind::Claude => crate::render::draw_mark(size),
        ProviderKind::Codex => crate::render::draw_codex_mark(size),
        ProviderKind::Spark => crate::render::draw_spark_mark(size),
    };
    draw_mask(dc, x, y, size, &mask, color, background);
}

unsafe fn draw_mask(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    mask: &[f32],
    color: COLORREF,
    background: COLORREF,
) {
    for py in 0..size {
        for px in 0..size {
            let coverage = mask[(py * size + px) as usize];
            if coverage > 0.0 {
                SetPixelV(dc, x + px, y + py, blend_color(background, color, coverage));
            }
        }
    }
}

unsafe fn draw_shared_glyph(
    dc: HDC,
    x: i32,
    y: i32,
    size: i32,
    glyph: crate::render::CardGlyph,
    color: COLORREF,
    background: COLORREF,
) {
    let mask = crate::render::draw_card_glyph(size, glyph);
    draw_mask(dc, x, y, size, &mask, color, background);
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_stat_grid(
    dc: HDC,
    s: &SparkStats,
    left: i32,
    top: i32,
    right: i32,
    scale: f32,
    tile: COLORREF,
    text: COLORREF,
    muted: COLORREF,
    big: HFONT,
    small: HFONT,
) {
    let gap = (6.0 * scale) as i32;
    let width = (right - left - 2 * gap) / 3;
    let height = (48.0 * scale) as i32;
    let v = [
        (
            "tok/s",
            format!("{:.0}", s.tok_s),
            format!("peak {:.0}", s.peak),
        ),
        (
            "running / waiting",
            format!("{} / {}", s.running, s.waiting),
            String::new(),
        ),
        ("KV", percent(s.kv_pct), String::new()),
        ("TTFT", unit(s.ttft_ms, "ms"), String::new()),
        (
            "GPU",
            format!("{:.0}%", s.gpu_util),
            format!("{} C · {} W", number(s.gpu_temp), number(s.gpu_power)),
        ),
        (
            "Memory",
            percent(s.mem_pct),
            format!("swap {} %", number(s.swap_pct)),
        ),
    ];
    for (i, (label, value, detail)) in v.iter().enumerate() {
        let col = i % 3;
        let row = i / 3;
        let x = left + col as i32 * (width + gap);
        let y = top + row as i32 * (height + gap);
        fill_round(dc, x, y, width, height, tile);
        SelectObject(dc, small);
        draw_text(dc, x + (7.0 * scale) as i32, y + 4, label, muted);
        SelectObject(dc, big);
        draw_text(
            dc,
            x + (7.0 * scale) as i32,
            y + (17.0 * scale) as i32,
            value,
            text,
        );
        if !detail.is_empty() {
            SelectObject(dc, small);
            let detail_y = if i == 0 {
                y + 4
            } else {
                y + (33.0 * scale) as i32
            };
            draw_text_right(dc, x + width - 6, detail_y, detail, muted);
        }
    }
}
fn number(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.0}")).unwrap_or_else(|| "-".into())
}
fn percent(v: Option<f64>) -> String {
    v.map(|n| format!("{n:.0}%")).unwrap_or_else(|| "-".into())
}
fn unit(v: Option<f64>, suffix: &str) -> String {
    v.map(|n| format!("{n:.0} {suffix}"))
        .unwrap_or_else(|| "-".into())
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_aux(
    dc: HDC,
    aux: &[AuxStatus],
    left: i32,
    top: i32,
    right: i32,
    scale: f32,
    fill: COLORREF,
    text: COLORREF,
    green: COLORREF,
    red: COLORREF,
    font: HFONT,
) {
    SelectObject(dc, font);
    let mut x = left;
    for service in aux {
        let state = if service.busy {
            "busy"
        } else if service.up {
            "idle"
        } else {
            "down"
        };
        let label = format!("● {} {state}", service.name);
        let width = text_width(dc, &label) + (12.0 * scale) as i32;
        if x + width > right {
            break;
        }
        fill_round(dc, x, top, width, (20.0 * scale) as i32, fill);
        let color = if service.busy {
            green
        } else if service.up {
            text
        } else {
            red
        };
        draw_text(dc, x + (6.0 * scale) as i32, top + 2, &label, color);
        x += width + (6.0 * scale) as i32;
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_area_chart(
    dc: HDC,
    values: &[f64],
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    color: COLORREF,
    background: COLORREF,
    grid: COLORREF,
    percent_scale: bool,
) {
    for step in 0..=2 {
        let y = top + (bottom - top) * step / 2;
        draw_line(dc, left, y, right, y, grid, 1);
    }
    for step in 0..=4 {
        let x = left + (right - left) * step / 4;
        draw_line(dc, x, top, x, bottom, grid, 1);
    }
    if values.len() < 3 {
        return;
    }
    let peak = if percent_scale {
        100.0
    } else {
        values.iter().copied().fold(1.0, f64::max)
    };
    let mut points = Vec::with_capacity(values.len() + 2);
    points.push(POINT { x: left, y: bottom });
    for (index, value) in values.iter().enumerate() {
        points.push(POINT {
            x: left + (right - left) * index as i32 / (values.len() - 1) as i32,
            y: bottom - ((bottom - top) as f64 * value.max(0.0) / peak) as i32,
        });
    }
    points.push(POINT {
        x: right,
        y: bottom,
    });
    let brush = CreateSolidBrush(blend_color(background, color, 0.25));
    let ob = SelectObject(dc, brush);
    let op = SelectObject(dc, GetStockObject(NULL_PEN));
    Polygon(dc, points.as_ptr(), points.len() as i32);
    SelectObject(dc, op);
    SelectObject(dc, ob);
    DeleteObject(brush);
    let line = &points[1..points.len() - 1];
    let pen = CreatePen(PS_SOLID, 2, color);
    let old = SelectObject(dc, pen);
    Polyline(dc, line.as_ptr(), line.len() as i32);
    SelectObject(dc, old);
    DeleteObject(pen);
}
fn blend_color(bg: COLORREF, fg: COLORREF, a: f32) -> COLORREF {
    let c = |s| {
        let b = ((bg >> s) & 255u32) as f32;
        let f = ((fg >> s) & 255u32) as f32;
        (b * (1.0 - a) + f * a).round() as u32
    };
    c(0) | (c(8) << 8) | (c(16) << 16)
}

unsafe fn present(hwnd: HWND, point: POINT, frame: (i32, i32, Vec<u32>)) {
    let (width, height, pixels) = frame;
    let screen = GetDC(null_mut());
    let dc = CreateCompatibleDC(screen);
    let mut bits = null_mut();
    let info = bitmap_info(width, height);
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
    let old = SelectObject(dc, bitmap);
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(
        hwnd, screen, &point, &size, dc, &source, 0, &blend, ULW_ALPHA,
    );
    SelectObject(dc, old);
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(null_mut(), screen);
}

unsafe fn redraw(hwnd: HWND) {
    let view = VIEW.with(|view| view.borrow().clone());
    let Some(view) = view else {
        return;
    };
    let mut window: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut window);
    let frame = render_with_hover(
        &view.snapshot,
        view.dark,
        view.dpi,
        is_pinned(),
        HOVER_HIT.with(Cell::get),
        view.width,
    );
    present(
        hwnd,
        POINT {
            x: window.left,
            y: window.top,
        },
        frame,
    );
}

unsafe fn hit_at(hwnd: HWND, x: i32, y: i32) -> HeaderHit {
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    VIEW.with(|view| {
        view.borrow().as_ref().map_or(HeaderHit::None, |view| {
            header_hit_test(
                x,
                y,
                client.right,
                view.dpi as f32 / 96.0,
                &view.snapshot.sections,
            )
        })
    })
}

unsafe fn dismiss(hwnd: HWND) {
    close();
    OWNER.with(|owner| PostMessageW(owner.get(), WM_DISMISSED, 0, 0));
    SetWindowLongPtrW(
        hwnd,
        GWL_EXSTYLE,
        (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 | WS_EX_NOACTIVATE) as isize,
    );
}

unsafe extern "system" fn proc_(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => {
            if is_click_mode() {
                MA_ACTIVATE as LRESULT
            } else {
                MA_NOACTIVATE as LRESULT
            }
        }
        WM_ACTIVATE => {
            if (wp & 0xffff) as u32 == WA_INACTIVE && is_click_mode() && !is_pinned() {
                dismiss(hwnd);
            }
            0
        }
        WM_KEYDOWN => {
            if wp == VK_ESCAPE as usize {
                dismiss(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, message, wp, lp)
        }
        WM_MOUSEMOVE => {
            HOVER.with(|h| h.set(true));
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            let hit = hit_at(hwnd, x, y);
            if HOVER_HIT.with(|old| old.replace(hit)) != hit {
                redraw(hwnd);
            }
            let cursor = if hit == HeaderHit::None {
                IDC_ARROW
            } else {
                IDC_HAND
            };
            SetCursor(LoadCursorW(null_mut(), cursor));
            let mut e = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut e);
            0
        }
        WM_MOUSE_LEAVE_MSG => {
            HOVER.with(|h| h.set(false));
            if HOVER_HIT.with(|hit| hit.replace(HeaderHit::None)) != HeaderHit::None {
                redraw(hwnd);
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = lp as i16 as i32;
            let y = (lp >> 16) as i16 as i32;
            match hit_at(hwnd, x, y) {
                HeaderHit::Close => dismiss(hwnd),
                HeaderHit::Pin => {
                    PINNED.with(|pinned| pinned.set(!pinned.get()));
                    redraw(hwnd);
                }
                HeaderHit::Refresh => {
                    OWNER.with(|o| PostMessageW(o.get(), WM_REFRESH, 0, 0));
                }
                HeaderHit::Provider(provider) => {
                    OWNER.with(|owner| {
                        PostMessageW(owner.get(), WM_OPEN_PROVIDER, provider as usize, 0)
                    });
                }
                HeaderHit::None if y < 38 && is_pinned() => {
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
                }
                HeaderHit::None => {}
            }
            0
        }
        WM_DESTROY => {
            WINDOW.with(|w| w.set(null_mut()));
            0
        }
        _ => DefWindowProcW(hwnd, message, 0, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_hit_testing_maps_controls_and_providers() {
        let width = MIN_WIDTH as i32;
        let sections = vec![
            Section {
                provider: ProviderKind::Claude,
                ..Default::default()
            },
            Section {
                provider: ProviderKind::Codex,
                ..Default::default()
            },
        ];
        assert_eq!(
            header_hit_test(width - 24, 18, width, 1.0, &sections),
            HeaderHit::Close
        );
        assert_eq!(
            header_hit_test(width - 48, 18, width, 1.0, &sections),
            HeaderHit::Pin
        );
        assert_eq!(
            header_hit_test(width - 72, 18, width, 1.0, &sections),
            HeaderHit::Refresh
        );
        assert_eq!(
            header_hit_test(28, 50, width, 1.0, &sections),
            HeaderHit::Provider(ProviderKind::Claude)
        );
        assert_eq!(
            header_hit_test(28, 171, width, 1.0, &sections),
            HeaderHit::Provider(ProviderKind::Codex)
        );
        assert_eq!(
            header_hit_test(180, 50, width, 1.0, &sections),
            HeaderHit::None
        );
    }

    #[test]
    fn worst_limit_and_status_copy_use_the_most_severe_limit() {
        let bars = vec![
            Bar {
                percent: 20.0,
                elapsed: 40.0,
                projected: 50.0,
                ..Default::default()
            },
            Bar {
                percent: 70.0,
                elapsed: 40.0,
                projected: 100.0,
                full_at: Some("8:52 PM".into()),
                ..Default::default()
            },
        ];
        assert_eq!(worst_limit_index(&bars), Some(1));
        assert_eq!(
            provider_status_text("Claude", &bars),
            "Claude · Runs out 8:52 PM"
        );
    }

    #[test]
    fn age_and_reset_copy_follow_compact_rules() {
        assert_eq!(age_copy(24, false), "24 s ago");
        assert_eq!(age_copy(300, false), "5 m ago");
        assert_eq!(age_copy(21600, true), "6 h old");
        assert_eq!(reset_time_copy(true, "Thu", "10:19 PM"), "10:19 PM");
        assert_eq!(reset_time_copy(false, "Thu", "8:59 AM"), "Thu 8:59 AM");
    }
}
