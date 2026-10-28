//! 锁屏 / 息屏检测 —— 让连点器在这些状态下彻底停手。
//!
//! ## 为什么需要这个模块
//!
//! `keybd_event` 注入的 F21~F24 对 Windows 内核来说是**真实的键盘输入**：
//! 它会重置系统的「最后输入时间」，也就是显示器空闲计时器。后果是
//! **息屏之后只要我们再发一次键，显示器就会重新点亮**（亮到锁屏输密码界面）。
//!
//! 而官方桌宠在锁屏 / 息屏时本来就不计数（`WH_KEYBOARD_LL` 收不到安全桌面上的输入，
//! 且桌面不可见时它的会话也是挂起的），所以我们不停手只会白亮屏幕、毫无收益。
//!
//! ## 两种判定
//!
//! 1. **显示器是否关闭**：注册 `GUID_CONSOLE_DISPLAY_STATE` 的电源设置通知
//!    （`PowerSettingRegisterNotification` + `DEVICE_NOTIFY_CALLBACK`，纯回调，不需要窗口消息循环），
//!    回调里把状态写进 `AtomicU32`（0=关闭、1=开启、2=变暗），零轮询开销；
//! 2. **是否锁屏**：`OpenInputDesktop` 打开当前输入桌面，锁屏时输入桌面是 `Winlogon`，
//!    普通进程（哪怕管理员）也会被 `ERROR_ACCESS_DENIED` 拒绝 → 判定为已锁定。
//!    每次约微秒级，不需要缓存。
//!
//! 任一成立即 `should_pause() == true`，调用方 sleep 一小会儿再检查。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct DeviceNotifySubscribeParameters {
    callback: Option<unsafe extern "system" fn(*const c_void, u32, *const c_void) -> u32>,
    context: *mut c_void,
}

/// `DEVICE_NOTIFY_CALLBACK`
const DEVICE_NOTIFY_CALLBACK: u32 = 2;
/// `PBT_POWERSETTINGCHANGE`
const PBT_POWERSETTINGCHANGE: u32 = 0x8013;
/// `DESKTOP_READOBJECTS`
const DESKTOP_READOBJECTS: u32 = 0x0001;

// GUID_CONSOLE_DISPLAY_STATE = {6FE69556-704A-47A0-8F24-C28D936FDA47}
const GUID_CONSOLE_DISPLAY_STATE: Guid = Guid {
    data1: 0x6FE6_9556,
    data2: 0x704A,
    data3: 0x47A0,
    data4: [0x8F, 0x24, 0xC2, 0x8D, 0x93, 0x6F, 0xDA, 0x47],
};

#[link(name = "powrprof")]
extern "system" {
    fn PowerSettingRegisterNotification(
        setting_guid: *const Guid,
        flags: u32,
        recipient: *const c_void,
        registration_handle: *mut *mut c_void,
    ) -> u32;
}

#[link(name = "user32")]
extern "system" {
    fn OpenInputDesktop(dwFlags: u32, fInherit: i32, dwDesiredAccess: u32) -> *mut c_void;
    fn CloseDesktop(hDesktop: *mut c_void) -> i32;
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(hWnd: *mut c_void, lpdwProcessId: *mut u32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> *mut c_void;
    fn CloseHandle(hObject: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(
        hProcess: *mut c_void,
        dwFlags: u32,
        lpExeName: *mut u16,
        lpdwSize: *mut u32,
    ) -> i32;
}

/// `PROCESS_QUERY_LIMITED_INFORMATION`
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// 0 = 显示器关闭，1 = 开启，2 = 变暗。默认按「开启」处理，避免注册失败时误暂停。
static DISPLAY_STATE: AtomicU32 = AtomicU32::new(1);
static INITED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_power_setting(
    _context: *const c_void,
    event_type: u32,
    setting: *const c_void,
) -> u32 {
    if event_type == PBT_POWERSETTINGCHANGE && !setting.is_null() {
        // POWERBROADCAST_SETTING { GUID PowerSetting; DWORD DataLength; UCHAR Data[1]; }
        // GUID 占 16 字节、DataLength 占 4 字节，Data 从偏移 20 开始。
        let data = (setting as *const u8).add(20) as *const u32;
        let state = std::ptr::read_unaligned(data);
        DISPLAY_STATE.store(state, Ordering::Relaxed);
    }
    0
}

/// 注册电源设置通知。只需调用一次，重复调用安全（幂等）。
pub fn init() {
    if INITED.swap(true, Ordering::SeqCst) {
        return;
    }
    let params = Box::into_raw(Box::new(DeviceNotifySubscribeParameters {
        callback: Some(on_power_setting),
        context: std::ptr::null_mut(),
    }));
    let mut handle: *mut c_void = std::ptr::null_mut();
    unsafe {
        // 返回值 0 = ERROR_SUCCESS。注册失败也不致命：display_off() 恒为 false，
        // 退化成只有「锁屏检测」在起作用。
        let _ = PowerSettingRegisterNotification(
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_CALLBACK,
            params as *const c_void,
            &mut handle,
        );
    }
}

/// 显示器是否已关闭（息屏）。
pub fn display_off() -> bool {
    DISPLAY_STATE.load(Ordering::Relaxed) == 0
}

/// 当前工作站是否已锁定（锁屏界面）。
///
/// 锁屏后输入桌面变成 `Winlogon`，只有 SYSTEM 能打开；普通进程（含管理员）
/// 拿到的都是 `ERROR_ACCESS_DENIED`，据此判定。
///
/// **为什么还要二次确认**：万一某些机器上 `OpenInputDesktop` 因别的原因失败，
/// 单靠它会把「正常使用中」误判成锁屏 → 连点器永久停摆。所以再查一次前台窗口
/// 的宿主进程，只有确实是 `LogonUI.exe` / `LockApp.exe`（或压根拿不到前台窗口）
/// 才认定为锁屏。
pub fn desktop_locked() -> bool {
    if input_desktop_accessible() {
        return false;
    }
    foreground_is_lock_screen()
}

fn input_desktop_accessible() -> bool {
    unsafe {
        let desk = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desk.is_null() {
            return false;
        }
        CloseDesktop(desk);
        true
    }
}

/// 前台窗口是否属于锁屏界面（LogonUI / LockApp），拿不到任何信息时按锁屏处理。
fn foreground_is_lock_screen() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return true;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(fg, &mut pid);
        if pid == 0 {
            return true;
        }
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if proc.is_null() {
            return true;
        }
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(proc, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(proc);
        if ok == 0 {
            return true;
        }
        let exe = String::from_utf16_lossy(&buf[..size as usize]).to_ascii_lowercase();
        exe.contains("logonui") || exe.contains("lockapp")
    }
}

/// 是否应当暂停发送按键：息屏或锁屏任一成立。
pub fn should_pause() -> bool {
    display_off() || desktop_locked()
}

/// 给诊断日志用的一行描述。
pub fn describe() -> &'static str {
    if display_off() {
        "显示器已关闭"
    } else if desktop_locked() {
        "已锁屏"
    } else {
        "正常"
    }
}
