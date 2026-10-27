//! UIA（UI Automation）无障碍树探测 —— 直接读官方 WebView 里的 DOM。
//!
//! 为什么值得这么做（实测确认）：
//! - 官方桌宠的宝箱挂件在 DOM 里是**真实元素**：
//!   - 冷却态 `<div class="reward-countdown" role="img" aria-label="13:34">`；
//!   - 就绪态 `<button class="reward-btn">`；
//!   - 本体容器 `<div class="pet-dock-content">`（200×211，属于「自己的猫」）。
//! - 因此 UIA 能一次性给出**状态 + 精确屏幕矩形 + 剩余时间**，
//!   与「窗口命中区域是否被别的桌宠合并」「不透明度」「缩放」全部无关。
//! - 实测 `FindAll` 约 25~37 ms，桌宠内存无变化（DOM 仅十几个节点）。
//!
//! 代价与约束（务必遵守）：
//! - 查询是在**桌宠 UI 线程**上执行的，必须低频；调用方要按剩余时间睡到快就绪再探；
//! - 跨完整性级别会被 UIPI 拦，所以本程序必须和官方一样以管理员运行；
//! - 只读，不注入、不 Hook、不改官方进程任何内存。
//!
//! COM 部分说明：CLSID/IID 与 `CoCreateInstance` 走自声明 FFI，
//! 只用 `windows` crate 提供 `IUIAutomation` / `IUIAutomationElement` 的虚表定义，
//! 这样对 crate 版本变化最不敏感。

use std::ffi::c_void;

use windows::core::Interface;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::Accessibility::{IUIAutomation, IUIAutomationElement};

/// DOM 类名（官方前端硬编码，见 AGENTS.md 2.3）
pub const CLASS_COUNTDOWN: &str = "reward-countdown";
pub const CLASS_READY: &str = "reward-btn";
pub const CLASS_DOCK: &str = "pet-dock-content";

#[repr(C)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

/// CLSID_CUIAutomation = {FF48DBA4-60EF-4201-AA87-54103EEF594E}
const CLSID_CUIAUTOMATION: Guid = Guid {
    d1: 0xFF48_DBA4,
    d2: 0x60EF,
    d3: 0x4201,
    d4: [0xAA, 0x87, 0x54, 0x10, 0x3E, 0xEF, 0x59, 0x4E],
};

/// IID_IUIAutomation = {30CBE57D-D9D0-452A-AB13-7AC5AC4825EE}
const IID_IUI_AUTOMATION: Guid = Guid {
    d1: 0x30CB_E57D,
    d2: 0xD9D0,
    d3: 0x452A,
    d4: [0xAB, 0x13, 0x7A, 0xC5, 0xAC, 0x48, 0x25, 0xEE],
};

const COINIT_MULTITHREADED: u32 = 0x0;
const CLSCTX_INPROC_SERVER: u32 = 0x1;
/// 单个界面元素树最多下探的节点数（桌宠 DOM 只有十几到几十个节点）
const NODE_BUDGET: u32 = 400;
const MAX_DEPTH: u32 = 10;

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(pv: *const c_void, coinit: u32) -> i32;
    fn CoCreateInstance(
        rclsid: *const Guid,
        punkouter: *mut c_void,
        ctx: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn center(&self) -> (i32, i32) {
        ((self.l + self.r) / 2, (self.t + self.b) / 2)
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    /// true = 就绪（`reward-btn`），false = 冷却（`reward-countdown`）
    pub ready: bool,
    /// 屏幕坐标（物理像素）
    pub rect: Rect,
    /// 冷却剩余秒数（从 aria-label 的 "mm:ss" 解析）
    pub remain_secs: Option<u32>,
}

impl Item {
    pub fn center(&self) -> (i32, i32) {
        self.rect.center()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// 本体容器 `pet-dock-content` 的屏幕矩形（用于区分左右宝箱、反推缩放）
    pub dock: Option<Rect>,
    pub items: Vec<Item>,
}

/// 一次探测会话。持有 `IUIAutomation` 实例，需在**同一线程**内复用。
pub struct Uia {
    automation: IUIAutomation,
}

impl Uia {
    pub fn new() -> Option<Self> {
        unsafe {
            // MTA：S_OK / S_FALSE（已初始化）/ RPC_E_CHANGED_MODE 都不致命
            let _ = CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED);

            let mut raw: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_CUIAUTOMATION,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IUI_AUTOMATION,
                &mut raw,
            );
            if hr < 0 || raw.is_null() {
                return None;
            }
            Some(Self {
                automation: IUIAutomation::from_raw(raw),
            })
        }
    }

    /// 探测指定窗口（传视觉层 `Tauri Window/Wuwa Tappo` 的 HWND）。
    /// 返回 `None` 表示 UIA 调用失败（调用方应降级到区域+像素路径）。
    pub fn probe(&self, hwnd: isize) -> Option<Report> {
        unsafe {
            let root: IUIAutomationElement =
                self.automation.ElementFromHandle(HWND(hwnd as _)).ok()?;
            let mut rep = Report::default();
            let mut budget = NODE_BUDGET;
            walk(&root, 0, &mut budget, &mut rep);
            Some(rep)
        }
    }
}

fn walk(el: &IUIAutomationElement, depth: u32, budget: &mut u32, rep: &mut Report) {
    if *budget == 0 || depth > MAX_DEPTH {
        return;
    }
    *budget -= 1;

    let class = unsafe { el.Get_CurrentClassName() }
        .map(|b| b.to_string())
        .unwrap_or_default();

    if class == CLASS_DOCK {
        if let Ok(r) = unsafe { el.Get_CurrentBoundingRectangle() } {
            rep.dock = Some(to_rect(r));
        }
    } else if class == CLASS_COUNTDOWN || class == CLASS_READY {
        if let Ok(r) = unsafe { el.Get_CurrentBoundingRectangle() } {
            let ready = class == CLASS_READY;
            let remain_secs = if ready {
                None
            } else {
                unsafe { el.Get_CurrentName() }
                    .map(|b| b.to_string())
                    .ok()
                    .and_then(|s| parse_mmss(&s))
            };
            rep.items.push(Item {
                ready,
                rect: to_rect(r),
                remain_secs,
            });
        }
    }

    let mut child = unsafe { el.Get_FirstChild() }.ok();
    while let Some(c) = child {
        if *budget == 0 {
            return;
        }
        walk(&c, depth + 1, budget, rep);
        child = unsafe { c.Get_NextSibling() }.ok();
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        l: r.left,
        t: r.top,
        r: r.right,
        b: r.bottom,
    }
}

/// 解析 "13:34" -> 814 秒
fn parse_mmss(s: &str) -> Option<u32> {
    let s = s.trim();
    let (m, sec) = s.split_once(':')?;
    let m: u32 = m.trim().parse().ok()?;
    let sec: u32 = sec.trim().parse().ok()?;
    if m > 24 * 60 {
        return None;
    }
    Some(m * 60 + sec)
}

/// 把秒数格式化成 "mm:ss"
pub fn fmt_mmss(total: u32) -> String {
    format!("{:02}:{:02}", total / 60, total % 60)
}
