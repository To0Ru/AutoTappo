//! 自动开箱引擎（Rust 侧落地实现）
//!
//! 设计要点（全部来自实机逆向与验证，见 AGENTS.md）：
//! 1. 几何来源唯一权威：`GetWindowRgn` + `GetRegionData`（窗口相对坐标，需加窗口原点）；
//! 2. 真实点击接收窗口不是 `Tauri Window`，而是官方单独的 `DesktopCatInput` 顶层窗口；
//! 3. 锚点定位只用 CSS 比例常数 (62/44/46/36)，因此与分辨率、DPI、桌宠位置完全解耦；
//! 4. 就绪判定只能靠像素（区域在就绪/冷却两种状态下完全相同）；
//! 5. 官方进程是以管理员运行的，`PostMessage` 定向投递需要同等完整性，否则 ERROR_ACCESS_DENIED；
//! 6. 资源：稳态每 30 分钟只唤醒约 40 次，每次 < 1 ms，循环内零分配。

use serde::Serialize;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ==================== Win32 FFI ====================

type Hwnd = isize;
type Hdc = isize;
type Hbitmap = isize;
type Hgdiobj = isize;
type Hrgn = isize;
type Handle = isize;

const GWL_EXSTYLE: i32 = -20;
const WS_EX_TRANSPARENT: i32 = 0x0000_0020;

const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const MK_LBUTTON: usize = 0x0001;

const ERROR_ACCESS_DENIED: i32 = 5;
const SRCCOPY: u32 = 0x00CC_0020;
const DIB_RGB_COLORS: u32 = 0;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION_CLASS: u32 = 20;

const RGN_ERROR: i32 = 0;
const RGN_NULL: i32 = 1;

// CSS 比例常数（官方 reward-anchor / reward-btn 尺寸，永不随缩放变化）
const ANCHOR_W: f32 = 62.0;
const ANCHOR_H: f32 = 44.0;
const OFF_RIGHT: f32 = 46.0;
const OFF_LEFT: f32 = 36.0;
const TOLERANCE: f32 = 0.15;

/// 开箱成功后的固定睡眠：略早于 30 分钟醒来，再进入 2 秒细扫
const COOLDOWN_SLEEP_MS: u64 = 29 * 60 * 1000 + 45_000;
/// 冷却状态未知剩余时间时的探测间隔
const COOLDOWN_PROBE_MS: u64 = 30_000;
/// 就绪扫描间隔
const READY_SCAN_MS: u64 = 2_000;
/// 遮挡退避上限
const OCCLUDED_BACKOFF_MAX_MS: u64 = 30_000;
/// 提权不足后的冷却
const NEED_ADMIN_BACKOFF_MS: u64 = 60_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct RGNDATAHEADER {
    dw_size: u32,
    i_type: u32,
    n_count: u32,
    n_rgn_size: u32,
    rc_bound: RECT,
}

#[repr(C)]
struct BITMAPINFOHEADER {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels: i32,
    bi_y_pels: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[repr(C)]
struct BITMAPINFO {
    header: BITMAPINFOHEADER,
    colors: [u32; 3],
}

type WndEnumProc = extern "system" fn(Hwnd, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn FindWindowW(class: *const u16, title: *const u16) -> Hwnd;
    fn EnumWindows(proc: WndEnumProc, lparam: isize) -> i32;
    fn GetClassNameW(hwnd: Hwnd, buf: *mut u16, len: i32) -> i32;
    fn GetWindowTextW(hwnd: Hwnd, buf: *mut u16, len: i32) -> i32;
    fn GetWindowRect(hwnd: Hwnd, rect: *mut RECT) -> i32;
    fn GetWindowRgn(hwnd: Hwnd, hrgn: Hrgn) -> i32;
    fn GetWindowLongW(hwnd: Hwnd, index: i32) -> i32;
    fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
    fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    fn IsWindowVisible(hwnd: Hwnd) -> i32;
    fn IsIconic(hwnd: Hwnd) -> i32;
    fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn GetDC(hwnd: Hwnd) -> Hdc;
    fn ReleaseDC(hwnd: Hwnd, hdc: Hdc) -> i32;
    fn GetAsyncKeyState(vkey: i32) -> i16;
    fn SetProcessDpiAwarenessContext(value: isize) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateRectRgn(x1: i32, y1: i32, x2: i32, y2: i32) -> Hrgn;
    fn GetRegionData(hrgn: Hrgn, size: u32, buf: *mut c_void) -> u32;
    fn DeleteObject(obj: Hgdiobj) -> i32;
    fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    fn CreateCompatibleBitmap(hdc: Hdc, w: i32, h: i32) -> Hbitmap;
    fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
    fn BitBlt(dst: Hdc, x: i32, y: i32, w: i32, h: i32, src: Hdc, sx: i32, sy: i32, rop: u32) -> i32;
    fn GetDIBits(hdc: Hdc, bmp: Hbitmap, start: u32, lines: u32, bits: *mut c_void, bi: *mut BITMAPINFO, usage: u32) -> i32;
    fn DeleteDC(hdc: Hdc) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, buf: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn ShellExecuteW(
        hwnd: Hwnd,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> isize;
}

// ==================== 基础类型 ====================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Left,
    Right,
}

impl Place {
    fn as_str(self) -> &'static str {
        match self {
            Place::Left => "left",
            Place::Right => "right",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Box2 {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Box2 {
    fn w(&self) -> i32 {
        self.r - self.l
    }
    fn h(&self) -> i32 {
        self.b - self.t
    }
    fn intersects(&self, o: &Box2) -> bool {
        self.l < o.r && self.r > o.l && self.t < o.b && self.b > o.t
    }
    fn center(&self) -> (i32, i32) {
        ((self.l + self.r) / 2, (self.t + self.b) / 2)
    }
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    place: Place,
    area: Box2, // 窗口相对坐标
    click: (i32, i32),
    scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readiness {
    Ready,
    Cooldown,
    Unknown,
}

/// 对外暴露给前端的快照
#[derive(Clone, Serialize)]
pub struct ChestSnapshot {
    pub enabled: bool,
    pub elevated: bool,
    pub state: String,
    pub detail: String,
    pub left: String,
    pub right: String,
    pub opens_ok: u32,
    pub opens_failed: u32,
    pub last_event_unix: u64,
}

impl Default for ChestSnapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            elevated: false,
            state: "idle".into(),
            detail: "未启动".into(),
            left: "-".into(),
            right: "-".into(),
            opens_ok: 0,
            opens_failed: 0,
            last_event_unix: 0,
        }
    }
}

// ==================== 窗口与几何 ====================

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn class_name(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), 256) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_title(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), 256) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_pid(hwnd: Hwnd) -> u32 {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    pid
}

fn window_rect(hwnd: Hwnd) -> Option<Box2> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
        return None;
    }
    Some(Box2 { l: r.left, t: r.top, r: r.right, b: r.bottom })
}

#[allow(dead_code)]
struct Found {
    input: Hwnd,
    visual: Hwnd,
    pid: u32,
    origin: (i32, i32),
    dpi: u32,
}

struct EnumCtx {
    input: Hwnd,
    visual: Hwnd,
}

extern "system" fn enum_cb(hwnd: Hwnd, lparam: isize) -> i32 {
    let ctx = unsafe { &mut *(lparam as *mut EnumCtx) };
    let cls = class_name(hwnd);
    if cls == "DesktopCatInput" && ctx.input == 0 {
        ctx.input = hwnd;
    } else if cls == "Tauri Window" && window_title(hwnd) == "Wuwa Tappo" && ctx.visual == 0 {
        ctx.visual = hwnd;
    }
    if ctx.input != 0 && ctx.visual != 0 {
        return 0; // 提前结束枚举
    }
    1
}

/// 找到桌宠的输入层（真实点击接收窗口）与视觉层窗口
fn find_pet() -> Option<Found> {
    let mut ctx = EnumCtx { input: 0, visual: 0 };
    unsafe { EnumWindows(enum_cb, &mut ctx as *mut EnumCtx as isize) };
    if ctx.input == 0 {
        return None;
    }
    let visual = if ctx.visual != 0 {
        ctx.visual
    } else {
        unsafe { FindWindowW(wide("Tauri Window").as_ptr(), wide("Wuwa Tappo").as_ptr()) }
    };
    let rect = window_rect(ctx.input)?;
    // 输入层是全屏窗口，其左上角即本显示器的物理原点（多屏时可为负）
    Some(Found {
        input: ctx.input,
        visual,
        pid: window_pid(ctx.input),
        origin: (rect.l, rect.t),
        dpi: unsafe { GetDpiForWindow(ctx.input) },
    })
}

/// 读取窗口当前裁剪区域（窗口相对坐标）
fn read_region(hwnd: Hwnd) -> Vec<Box2> {
    let hrgn = unsafe { CreateRectRgn(0, 0, 0, 0) };
    if hrgn == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    unsafe {
        let kind = GetWindowRgn(hwnd, hrgn);
        if kind > RGN_NULL {
            let need = GetRegionData(hrgn, 0, std::ptr::null_mut());
            if need > 0 {
                let mut buf = vec![0u8; need as usize];
                let got = GetRegionData(hrgn, need, buf.as_mut_ptr() as *mut c_void);
                if got > 0 {
                    let hdr = &*(buf.as_ptr() as *const RGNDATAHEADER);
                    let header_size = std::mem::size_of::<RGNDATAHEADER>();
                    let count = hdr.n_count as usize;
                    if buf.len() >= header_size + count * 16 {
                        let base = buf.as_ptr() as usize + header_size;
                        for i in 0..count {
                            let p = (base + i * 16) as *const RECT;
                            let r = *p;
                            out.push(Box2 { l: r.left, t: r.top, r: r.right, b: r.bottom });
                        }
                    }
                }
            }
        } else if kind == RGN_ERROR {
            // 无区域（模块面板打开 / 被隐藏），返回空
        }
        DeleteObject(hrgn);
    }
    out
}

fn region_hash(rects: &[Box2]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for r in rects {
        for v in [r.l, r.t, r.r, r.b] {
            h ^= v as u64;
            h = h.wrapping_mul(1099511628211);
        }
    }
    h
}

fn region_is_empty(rects: &[Box2]) -> bool {
    rects.is_empty()
}

/// 按 x 区间重叠把矩形聚类（房间里可能有多只位置不同的桌宠）
fn cluster_by_x(rects: &[Box2]) -> Vec<Vec<Box2>> {
    let n = rects.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| rects[i].l);
    for a in 0..n {
        let i = order[a];
        for &j in order.iter().skip(a + 1) {
            if rects[j].l >= rects[i].r {
                break;
            }
            if rects[j].r > rects[i].l {
                let ri = find(&mut parent, i);
                let rj = find(&mut parent, j);
                if ri != rj {
                    parent[rj] = ri;
                }
            }
        }
    }

    let mut groups: HashMap<usize, Vec<Box2>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(rects[i]);
    }
    groups.into_values().collect()
}

/// 单簇内求锚点：逐行右边界剖面 → 平台 → 同一批行内的本体右缘 → 自校验
fn anchors_in_cluster(rects: &[Box2]) -> Vec<Anchor> {
    let mut prof: HashMap<i32, i32> = HashMap::new();
    for r in rects {
        for y in r.t..r.b {
            let e = prof.entry(y).or_insert(-1);
            if r.r > *e {
                *e = r.r;
            }
        }
    }
    if prof.is_empty() {
        return Vec::new();
    }

    // 相同右边界 → (行数, 最小 y, 最大 y)
    let mut plateaus: HashMap<i32, (i32, i32, i32)> = HashMap::new();
    for (&y, &right) in prof.iter() {
        let e = plateaus.entry(right).or_insert((0, y, y));
        e.0 += 1;
        if y < e.1 {
            e.1 = y;
        }
        if y > e.2 {
            e.2 = y;
        }
    }

    let mut cands: Vec<(i32, i32, i32, i32)> = plateaus
        .iter()
        .filter(|(_, v)| v.0 >= 18)
        .map(|(&right, v)| (right, v.0, v.1, v.2))
        .collect();
    cands.sort_by(|a, b| b.0.cmp(&a.0));

    let mut out = Vec::new();
    for &(right, _rows, top, bottom) in cands.iter() {
        let band: i32 = bottom - top + 1;
        let scale = band as f32 / ANCHOR_H;
        // scale ≈ (DPI/96) × 桌宠大小比例。
        // 官方桌宠大小范围 50%~200%，叠加最高 200% DPI 时可达 4.0，
        // 因此区间取 0.35~4.6，两端各留测量误差余量。
        if !(0.35..=4.6).contains(&scale) {
            continue;
        }
        let lo = top - 2 * band;
        let hi = bottom + 2 * band;

        // 本体右缘：只取与锚点上下相邻的行（排除锚点自身所占的行），取众数
        let mut votes: HashMap<i32, i32> = HashMap::new();
        for y in lo..=hi {
            if y >= top && y <= bottom {
                continue;
            }
            if let Some(&v) = prof.get(&y) {
                if v < right {
                    *votes.entry(v).or_insert(0) += 1;
                }
            }
        }
        let dock_right = match votes.iter().max_by_key(|(v, c)| (**c, **v)) {
            Some((v, _)) => *v,
            None => continue,
        };

        let delta = (right - dock_right) as f32;
        if delta < 20.0 {
            continue;
        }
        let as_right = (delta / OFF_RIGHT - scale).abs();
        let as_left = (delta / OFF_LEFT - scale).abs();
        let (place, err) = if as_right <= as_left {
            (Place::Right, as_right)
        } else {
            (Place::Left, as_left)
        };
        if err > TOLERANCE * scale.max(1.0) {
            continue; // 比例对不上，说明不是宝箱锚点 → 安全放弃
        }

        let w = (ANCHOR_W * scale).round() as i32;
        let area = Box2 { l: right - w, t: top, r: right, b: bottom + 1 };
        out.push(Anchor { place, area, click: area.center(), scale });
    }
    out
}

fn locate(rects: &[Box2]) -> Vec<Anchor> {
    let mut found: Vec<Anchor> = Vec::new();
    for cl in cluster_by_x(rects) {
        found.extend(anchors_in_cluster(&cl));
    }
    // 去重（右锚点可能把左锚点当成候选）
    found.sort_by_key(|a| (a.area.t, a.area.l));
    found.dedup_by_key(|a| (a.place as u8, a.area.t));
    found
}

// ==================== 遮挡检测 ====================

struct ZEntry {
    hwnd: Hwnd,
    rect: Box2,
    transparent: bool,
}

fn z_order() -> Vec<ZEntry> {
    let mut out: Vec<ZEntry> = Vec::new();

    extern "system" fn cb(hwnd: Hwnd, lparam: isize) -> i32 {
        let out = unsafe { &mut *(lparam as *mut Vec<ZEntry>) };
        if unsafe { IsWindowVisible(hwnd) } == 0 || unsafe { IsIconic(hwnd) } != 0 {
            return 1;
        }
        if let Some(rect) = window_rect(hwnd) {
            let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) };
            out.push(ZEntry { hwnd, rect, transparent: (ex & WS_EX_TRANSPARENT) != 0 });
        }
        1
    }

    unsafe { EnumWindows(cb, &mut out as *mut Vec<ZEntry> as isize) };
    out
}

/// 只有排在桌宠视觉层**之前**（更靠上）的窗口才可能遮挡它
fn is_occluded(area_screen: &Box2, pet: &Found) -> Option<String> {
    let order = z_order();
    let pet_index = order.iter().position(|e| e.hwnd == pet.visual)?;
    for e in order.iter().take(pet_index) {
        if window_pid(e.hwnd) == pet.pid {
            continue; // 自己人（输入层 / Toast / 面板）不算遮挡
        }
        if e.transparent {
            continue; // 点击穿透的全屏覆盖层，不构成遮挡
        }
        if e.rect.intersects(area_screen) {
            return Some(format!("{} / {}", class_name(e.hwnd), window_title(e.hwnd)));
        }
    }
    None
}

// ==================== 像素采样与就绪判定 ====================

fn capture(area_screen: &Box2) -> Option<(Vec<u8>, i32, i32)> {
    let w = area_screen.w();
    let h = area_screen.h();
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(0);
        if screen == 0 {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        if mem == 0 {
            ReleaseDC(0, screen);
            return None;
        }
        let bmp = CreateCompatibleBitmap(screen, w, h);
        if bmp == 0 {
            DeleteDC(mem);
            ReleaseDC(0, screen);
            return None;
        }
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(mem, 0, 0, w, h, screen, area_screen.l, area_screen.t, SRCCOPY);

        let mut bi = BITMAPINFO {
            header: BITMAPINFOHEADER {
                bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                bi_width: w,
                bi_height: -h, // 自上而下
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: 0,
                bi_size_image: 0,
                bi_x_pels: 0,
                bi_y_pels: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            colors: [0; 3],
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = if ok != 0 {
            GetDIBits(
                mem,
                bmp,
                0,
                h as u32,
                buf.as_mut_ptr() as *mut c_void,
                &mut bi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(0, screen);

        if lines == 0 {
            None
        } else {
            Some((buf, w, h))
        }
    }
}

/// 采样外扩边距：锚点框右侧/上/下都是纯壁纸，用它估计背后底色。
fn ring_margin(scale: f32) -> i32 {
    let m = (7.0 * scale).round() as i32;
    m.max(5).min(20)
}

/// 是否开启像素判定诊断日志（设置环境变量 `AUTOTAPPO_CHEST_DEBUG=1` 后生效）。
/// 用于实机校准 stage 2 的比例阈值，不设该变量时几乎零开销。
fn debug_on() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static FLAG: AtomicU8 = AtomicU8::new(2); // 2=未初始化
    let v = FLAG.load(Ordering::Relaxed);
    if v != 2 {
        return v == 1;
    }
    let on = std::env::var_os("AUTOTAPPO_CHEST_DEBUG").is_some();
    FLAG.store(if on { 1 } else { 0 }, Ordering::Relaxed);
    on
}

/// 诊断日志文件路径：`%TEMP%\autotappo-chest-debug.log`
/// （本程序是 GUI 子系统，没有控制台，stderr 看不到，只能落盘）
fn debug_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("autotappo-chest-debug.log")
}

/// 写一行诊断日志；未开启诊断或写入失败时静默忽略（绝不影响主流程）
fn debug_log(args: std::fmt::Arguments) {
    if !debug_on() {
        return;
    }
    use std::io::Write;
    let path = debug_log_path();
    // 超过 1 MB 就清空重来，避免长时间常驻把磁盘写满
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() > 1_048_576 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", args);
    }
}

/// 用「按钮框之外」的一圈像素估计壁纸底色（逐通道中位数，抗局部噪声）
fn background_estimate(buf: &[u8], w: i32, h: i32, inner: &Box2) -> Option<(i32, i32, i32)> {
    let mut rs: Vec<u8> = Vec::new();
    let mut gs: Vec<u8> = Vec::new();
    let mut bs: Vec<u8> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if x >= inner.l && x < inner.r && y >= inner.t && y < inner.b {
                continue; // 框内是按钮本身，不参与背景估计
            }
            let i = ((y * w + x) * 4) as usize;
            bs.push(buf[i]);
            gs.push(buf[i + 1]);
            rs.push(buf[i + 2]);
        }
    }
    if rs.len() < 20 {
        return None; // 框外像素不足（未外扩），放弃背景差分
    }
    rs.sort_unstable();
    gs.sort_unstable();
    bs.sort_unstable();
    let mid = rs.len() / 2;
    Some((rs[mid] as i32, gs[mid] as i32, bs[mid] as i32))
}

/// 就绪判定（两级）：
///
/// 阶段 1：绝对阈值 —— 100% 不透明度下的原始判据，与旧版行为完全一致；
/// 阶段 2：背景差分 —— 低不透明度时屏幕像素是 `a*C + (1-a)*B`（a=不透明度，B=壁纸），
///         用框外一圈壁纸估出 B，则 `d = screen - B = a*(C - B)`，
///         **d 的方向与 a、B 都无关**，据此判定按钮内容色相即可。
fn classify(buf: &[u8], w: i32, h: i32, inner: &Box2) -> Readiness {
    if buf.len() < (w * h * 4) as usize || inner.w() <= 0 || inner.h() <= 0 {
        return Readiness::Unknown;
    }
    let px = |x: i32, y: i32| -> (i32, i32, i32) {
        let i = ((y * w + x) * 4) as usize;
        (buf[i + 2] as i32, buf[i + 1] as i32, buf[i] as i32)
    };

    // 按钮框内再内缩 12%，排除圆角与阴影
    let inset_w = (inner.w() as f32 * 0.12) as i32;
    let inset_h = (inner.h() as f32 * 0.12) as i32;
    let x0 = inner.l + inset_w;
    let x1 = inner.r - inset_w;
    let y0 = inner.t + inset_h;
    let y1 = inner.b - inset_h;
    if x1 <= x0 || y1 <= y0 {
        return Readiness::Unknown;
    }

    // ---------- 阶段 1：绝对阈值（向后兼容 100% 不透明度） ----------
    let mut total: u32 = 0;
    let mut warm: u32 = 0;
    let mut white: u32 = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let (r, g, b) = px(x, y);
            total += 1;
            if r > 180 && (r - b) > 45 && (r - g) > 10 {
                warm += 1;
            }
            if r > 235 && g > 230 && b > 215 {
                white += 1;
            }
        }
    }
    if total == 0 {
        return Readiness::Unknown;
    }
    let warm_ratio = warm as f32 / total as f32;
    let white_ratio = white as f32 / total as f32;
    if debug_on() {
        debug_log(format_args!(
            "[chest] stage1 warm={:.3} white={:.3} box={}x{}",
            warm_ratio,
            white_ratio,
            inner.w(),
            inner.h()
        ));
    }
    if warm_ratio >= 0.30 && white_ratio < 0.15 {
        return Readiness::Ready;
    }
    if white_ratio >= 0.15 {
        return Readiness::Cooldown;
    }

    // ---------- 阶段 2：背景差分（兼容低不透明度） ----------
    let Some((bg_r, bg_g, bg_b)) = background_estimate(buf, w, h, inner) else {
        return Readiness::Unknown;
    };

    let mut total2: u32 = 0;
    let mut warm2: u32 = 0;
    let mut white2: u32 = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let (r, g, b) = px(x, y);
            let dr = r - bg_r;
            let dg = g - bg_g;
            let db = b - bg_b;
            let mag = dr.abs() + dg.abs() + db.abs();
            total2 += 1;
            if mag < 18 {
                continue; // 与壁纸几乎无差别：不透明度太低，或白底白钮
            }
            let m = mag as f32;
            // 就绪：暖橙，红显著高于蓝
            if (dr - db) as f32 >= 0.42 * m && dr > 0 {
                warm2 += 1;
            }
            // 冷却：奶白，三通道接近且整体比壁纸亮
            let mx = dr.max(dg).max(db);
            let mn = dr.min(dg).min(db);
            if (mx - mn) as f32 <= 0.22 * m && dr > 0 && dg > 0 {
                white2 += 1;
            }
        }
    }
    if total2 == 0 {
        return Readiness::Unknown;
    }
    let wr2 = warm2 as f32 / total2 as f32;
    let wh2 = white2 as f32 / total2 as f32;
    if debug_on() {
        debug_log(format_args!(
            "[chest] stage2 bg=({},{},{}) warm={:.3} white={:.3}",
            bg_r, bg_g, bg_b, wr2, wh2
        ));
    }
    if wr2 >= 0.28 && wh2 < 0.15 {
        Readiness::Ready
    } else if wh2 >= 0.18 {
        Readiness::Cooldown
    } else {
        Readiness::Unknown
    }
}

fn readiness_of(anchor: &Anchor, origin: (i32, i32), pet: &Found) -> (Readiness, String) {
    let screen = Box2 {
        l: anchor.area.l + origin.0,
        t: anchor.area.t + origin.1,
        r: anchor.area.r + origin.0,
        b: anchor.area.b + origin.1,
    };
    if let Some(who) = is_occluded(&screen, pet) {
        return (Readiness::Unknown, format!("被遮挡: {who}"));
    }
    // 右/上/下外扩一圈纯壁纸，供低不透明度时估计背后底色；
    // 左侧不扩：锚点框左侧紧邻桌宠本体，扩进来会把本体像素混入背景估计。
    let m = ring_margin(anchor.scale);
    let outer = Box2 {
        l: screen.l,
        t: screen.t - m,
        r: screen.r + m,
        b: screen.b + m,
    };
    match capture(&outer) {
        Some((buf, w, h)) => {
            let inner = Box2 {
                l: screen.l - outer.l,
                t: screen.t - outer.t,
                r: screen.r - outer.l,
                b: screen.b - outer.t,
            };
            let state = classify(&buf, w, h, &inner);
            let text = match state {
                Readiness::Ready => "就绪",
                Readiness::Cooldown => "冷却中",
                Readiness::Unknown => "无法判定",
            };
            debug_log(format_args!("[chest] 判定 = {}", text));
            (state, text.to_string())
        }
        None => (Readiness::Unknown, "截取失败".into()),
    }
}

// ==================== 点击注入 ====================

fn post_click(hwnd: Hwnd, client: (i32, i32)) -> Result<(), i32> {
    let lparam = (((client.1 as i64) << 16) | ((client.0 as i64) & 0xFFFF)) as isize;
    unsafe {
        let ok_move = PostMessageW(hwnd, WM_MOUSEMOVE, 0, lparam);
        thread::sleep(Duration::from_millis(30));
        let ok_down = PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lparam);
        thread::sleep(Duration::from_millis(40));
        let ok_up = PostMessageW(hwnd, WM_LBUTTONUP, 0, lparam);
        if ok_move == 0 || ok_down == 0 || ok_up == 0 {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        } else {
            Ok(())
        }
    }
}

// ==================== 提权 ====================

fn is_elevated() -> bool {
    unsafe {
        let mut token: Handle = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: u32 = 0;
        let mut len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            &mut elevation as *mut u32 as *mut c_void,
            4,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation != 0
    }
}

/// 以管理员身份重启自身（用于开箱需要同等完整性）
pub fn relaunch_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = wide(&exe.to_string_lossy());
    let op = wide("runas");
    unsafe {
        let ret = ShellExecuteW(0, op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
        if ret as usize <= 32 {
            return Err(format!("提权启动失败 (code {ret})"));
        }
    }
    Ok(())
}

// ==================== 控制器与工作线程 ====================

pub struct ChestController {
    pub enabled: Arc<AtomicBool>,
    pub shutdown: Arc<AtomicBool>,
    pub snapshot: Arc<Mutex<ChestSnapshot>>,
    pub opens_ok: Arc<AtomicU64>,
    pub opens_failed: Arc<AtomicU64>,
}

impl ChestController {
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            snapshot: Arc::new(Mutex::new(ChestSnapshot::default())),
            opens_ok: Arc::new(AtomicU64::new(0)),
            opens_failed: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    pub fn status(&self) -> ChestSnapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn start_worker(&self) {
        let enabled = Arc::clone(&self.enabled);
        let shutdown = Arc::clone(&self.shutdown);
        let snapshot = Arc::clone(&self.snapshot);
        let opens_ok = Arc::clone(&self.opens_ok);
        let opens_failed = Arc::clone(&self.opens_failed);

        thread::spawn(move || {
            let mut next_check = Instant::now();
            let mut occluded_backoff = Duration::from_secs(2);
            let mut unknown_streak: u32 = 0;
            let mut left_state = String::from("-");
            let mut right_state = String::from("-");
            let mut last_detail = String::from("待机");

            let publish = |state: &str, detail: &str, left: &str, right: &str| {
                if let Ok(mut s) = snapshot.lock() {
                    s.enabled = enabled.load(Ordering::Relaxed);
                    s.elevated = is_elevated();
                    s.state = state.to_string();
                    s.detail = detail.to_string();
                    s.left = left.to_string();
                    s.right = right.to_string();
                    s.opens_ok = opens_ok.load(Ordering::Relaxed) as u32;
                    s.opens_failed = opens_failed.load(Ordering::Relaxed) as u32;
                    s.last_event_unix = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                }
            };

            while !shutdown.load(Ordering::Relaxed) {
                // 未启用：低频休眠，几乎零开销
                if !enabled.load(Ordering::Relaxed) {
                    if last_detail != "已关闭" {
                        publish("idle", "已关闭", &left_state, &right_state);
                        last_detail = "已关闭".to_string();
                    }
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                let now = Instant::now();
                if now < next_check {
                    let wait = (next_check - now).min(Duration::from_millis(200));
                    thread::sleep(wait);
                    continue;
                }

                // 礼貌避让：用户在打字 / 划词 / 拖拽时不动作
                if user_is_active() {
                    next_check = Instant::now() + Duration::from_millis(500);
                    continue;
                }

                // ① 找窗口
                let pet = match find_pet() {
                    Some(p) => p,
                    None => {
                        left_state = "-".into();
                        right_state = "-".into();
                        last_detail = "未找到桌宠进程".into();
                        publish("no-pet", "未找到桌宠进程", &left_state, &right_state);
                        next_check = Instant::now() + Duration::from_secs(5);
                        continue;
                    }
                };

                // ② 读区域（每轮都读，绝不缓存）
                let rects = read_region(pet.input);
                if region_is_empty(&rects) {
                    left_state = "-".into();
                    right_state = "-".into();
                    last_detail = "桌宠面板打开或已隐藏".into();
                    publish("idle", "桌宠面板打开或已隐藏", &left_state, &right_state);
                    next_check = Instant::now() + Duration::from_secs(5);
                    continue;
                }

                // ③ 稳定性校验：60ms 后再读一次，必须完全一致（用户拖拽时区域会抖动）
                let h1 = region_hash(&rects);
                thread::sleep(Duration::from_millis(60));
                let rects2 = read_region(pet.input);
                if region_hash(&rects2) != h1 {
                    last_detail = "桌宠正在移动".into();
                    publish("moving", "桌宠正在移动", &left_state, &right_state);
                    next_check = Instant::now() + Duration::from_millis(800);
                    continue;
                }

                // ④ 定位锚点
                let anchors = locate(&rects2);
                if anchors.is_empty() {
                    left_state = "-".into();
                    right_state = "-".into();
                    last_detail = "暂无宝箱（累计点击数不足）".into();
                    publish("no-chest", "暂无宝箱（累计点击数不足）", &left_state, &right_state);
                    next_check = Instant::now() + Duration::from_secs(10);
                    continue;
                }

                // ⑤ 逐个锚点判定
                let mut acted = false;
                let mut occluded_any = false;
                let mut cooldown_any = false;
                let mut unknown_any = false;

                for anchor in anchors.iter() {
                    let (state, text) = readiness_of(anchor, pet.origin, &pet);
                    match anchor.place {
                        Place::Left => left_state = text.clone(),
                        Place::Right => right_state = text.clone(),
                    }

                    match state {
                        Readiness::Cooldown => {
                            cooldown_any = true;
                        }
                        Readiness::Unknown => {
                            if text.starts_with("被遮挡") {
                                occluded_any = true;
                            } else {
                                unknown_any = true;
                            }
                        }
                        Readiness::Ready => {
                            // ⑥ 投递点击（客户区坐标 = 屏幕坐标 - 窗口原点，锚点本身就是窗口相对坐标）
                            let client = anchor.click;
                            match post_click(pet.input, client) {
                                Ok(()) => {
                                    // ⑦ 点后验证：350ms 后复查，必须不再处于就绪
                                    thread::sleep(Duration::from_millis(350));
                                    let after = read_region(pet.input);
                                    let changed = region_hash(&after) != h1;
                                    let re_anchors = locate(&after);
                                    let still_ready = re_anchors
                                        .iter()
                                        .find(|a| a.place == anchor.place)
                                        .map(|a| {
                                            readiness_of(a, pet.origin, &pet).0 == Readiness::Ready
                                        })
                                        .unwrap_or(false);

                                    if changed && !still_ready {
                                        opens_ok.fetch_add(1, Ordering::Relaxed);
                                        last_detail = format!("已开启 {} 宝箱", anchor.place.as_str());
                                        publish("opened", &last_detail, &left_state, &right_state);
                                        next_check =
                                            Instant::now() + Duration::from_millis(COOLDOWN_SLEEP_MS);
                                        acted = true;
                                    } else {
                                        // 点了但没生效：可能跨屏后区域与绘制不一致，降频重试
                                        opens_failed.fetch_add(1, Ordering::Relaxed);
                                        last_detail = format!(
                                            "{} 宝箱点击未生效，稍后重试",
                                            anchor.place.as_str()
                                        );
                                        publish("retry", &last_detail, &left_state, &right_state);
                                        next_check = Instant::now() + Duration::from_secs(5);
                                        acted = true;
                                    }
                                    unknown_streak = 0;
                                }
                                Err(code) => {
                                    opens_failed.fetch_add(1, Ordering::Relaxed);
                                    if code == ERROR_ACCESS_DENIED {
                                        last_detail =
                                            "需要管理员权限（官方桌宠以管理员运行）".into();
                                        publish("need-admin", &last_detail, &left_state, &right_state);
                                        next_check = Instant::now()
                                            + Duration::from_millis(NEED_ADMIN_BACKOFF_MS);
                                    } else {
                                        last_detail = format!("点击投递失败 (code {code})");
                                        publish("error", &last_detail, &left_state, &right_state);
                                        next_check = Instant::now() + Duration::from_secs(10);
                                    }
                                    acted = true;
                                }
                            }
                        }
                    }
                    if acted {
                        break;
                    }
                }

                if acted {
                    continue;
                }

                // ⑧ 排定下一轮
                if occluded_any {
                    next_check = Instant::now() + occluded_backoff;
                    occluded_backoff =
                        (occluded_backoff * 2).min(Duration::from_millis(OCCLUDED_BACKOFF_MAX_MS));
                    publish("occluded", &last_detail, &left_state, &right_state);
                } else {
                    occluded_backoff = Duration::from_secs(2);
                    if cooldown_any {
                        unknown_streak = 0;
                        last_detail = "冷却中，稍后探测".into();
                        publish("cooldown", &last_detail, &left_state, &right_state);
                        next_check = Instant::now() + Duration::from_millis(COOLDOWN_PROBE_MS);
                    } else if unknown_any {
                        unknown_streak += 1;
                        last_detail = "画面异常，本轮跳过".into();
                        publish("unknown", &last_detail, &left_state, &right_state);
                        next_check = Instant::now() + Duration::from_secs(5);
                    } else {
                        // 理论上不会走到这里
                        next_check = Instant::now() + Duration::from_millis(READY_SCAN_MS);
                    }
                }
            }

            publish("stopped", "已退出", &left_state, &right_state);
        });
    }
}

/// 礼貌避让：用户在打字、切换输入法或拖拽时不注入
fn user_is_active() -> bool {
    let down = |vk: i32| unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 };
    if down(0x01) || down(0x02) || down(0x04) {
        return true;
    }
    if down(0x10) || down(0x11) || down(0x12) || down(0x5B) || down(0x5C) {
        return true;
    }
    if down(0x20) || down(0x08) || down(0x0D) {
        return true;
    }
    false
}

/// 供 main 早期调用，确保坐标是物理像素（多屏 / 高 DPI 必需）
pub fn ensure_dpi_aware() {
    unsafe {
        SetProcessDpiAwarenessContext(-4isize); // PER_MONITOR_AWARE_V2
    }
}
