//! 核心挂机引擎：纯后台静默虚拟按键与全场景礼貌避让
//!
//! 1. 采用 F21~F24 (0x84~0x87) 顶级幻影键，不打出乱码，不冲突快捷键；
//! 2. 毫秒级侦测用户物理操作 (打字、Shift 切输入法、鼠标拖拽/划词)，实时礼貌避让；
//! 3. 绝对 0 鼠标位移干涉，原生物理手感；
//! 4. 支持全局热键自定义 (默认 F6，支持动态切换)。

use rand::Rng;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::session;

// ---------------- Win32 FFI ----------------
const KEYEVENTF_KEYUP: u32 = 0x0002;
const SPI_GETMOUSEVANISH: u32 = 0x1020;
const SPI_SETMOUSEVANISH: u32 = 0x1021;
const SPIF_UPDATEINIFILE: u32 = 0x01;
const SPIF_SENDCHANGE: u32 = 0x02;

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vKey: i32) -> i16;
    fn keybd_event(bVk: u8, bScan: u8, dwFlags: u32, dwExtraInfo: usize);
    fn SystemParametersInfoW(
        uiAction: u32,
        uiParam: u32,
        pvParam: *mut c_void,
        fWinIni: u32,
    ) -> i32;
}

#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

// 幻影键 (F21 ~ F24)
const PHANTOM_KEYS: [u8; 4] = [0x84, 0x85, 0x86, 0x87];

// 默认快捷键: F6 (0x75)
pub const DEFAULT_HOTKEY_VK: i32 = 0x75;

// 需避让的物理按键：鼠标按键、修饰键、字母数字及标点
fn is_key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// 检测用户当前是否在使用物理键盘或鼠标按键
fn is_user_actively_operating() -> bool {
    // 1. 鼠标按键: 左键(0x01)、右键(0x02)、中键(0x04)
    if is_key_down(0x01) || is_key_down(0x02) || is_key_down(0x04) {
        return true;
    }

    // 2. 修饰键: Shift(0x10), Ctrl(0x11), Alt(0x12), LWin(0x5B), RWin(0x5C)
    if is_key_down(0x10) || is_key_down(0x11) || is_key_down(0x12) || is_key_down(0x5B) || is_key_down(0x5C) {
        return true;
    }

    // 3. 常用打字键: A-Z (0x41..=0x5A)
    for vk in 0x41..=0x5A {
        if is_key_down(vk) {
            return true;
        }
    }

    // 4. 数字键: 0-9 (0x30..=0x39)
    for vk in 0x30..=0x39 {
        if is_key_down(vk) {
            return true;
        }
    }

    // 5. 空格(0x20)、退格(0x08)、回车(0x0D)
    if is_key_down(0x20) || is_key_down(0x08) || is_key_down(0x0D) {
        return true;
    }

    false
}

/// 速度档位定义
#[derive(Debug, Clone, Copy)]
pub struct SpeedConfig {
    pub min_interval_ms: u64,
    pub max_interval_ms: u64,
    pub min_hold_ms: u64,
    pub max_hold_ms: u64,
}

pub fn get_speed_config(mode: u8) -> SpeedConfig {
    match mode {
        1 => SpeedConfig {
            min_interval_ms: 35,
            max_interval_ms: 55,
            min_hold_ms: 15,
            max_hold_ms: 25,
        }, // 快速档 (~20~25 次/s)
        2 => SpeedConfig {
            min_interval_ms: 18,
            max_interval_ms: 26,
            min_hold_ms: 10,
            max_hold_ms: 18,
        }, // 极速狂暴 (~40~50 次/s)
        _ => SpeedConfig {
            min_interval_ms: 70,
            max_interval_ms: 125,
            min_hold_ms: 16,
            max_hold_ms: 32,
        }, // 休闲拟真 (~8~12 次/s)
    }
}

pub struct ClickerController {
    pub running: Arc<AtomicBool>,
    pub speed_mode: Arc<AtomicU8>,
    pub total_sent: Arc<AtomicU64>,
    pub hotkey_vk: Arc<AtomicI32>,
    pub shutdown: Arc<AtomicBool>,
}

impl ClickerController {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            speed_mode: Arc::new(AtomicU8::new(0)),
            total_sent: Arc::new(AtomicU64::new(0)),
            hotkey_vk: Arc::new(AtomicI32::new(DEFAULT_HOTKEY_VK)),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start_worker(&self) {
        let running = Arc::clone(&self.running);
        let speed_mode = Arc::clone(&self.speed_mode);
        let total_sent = Arc::clone(&self.total_sent);
        let hotkey_vk = Arc::clone(&self.hotkey_vk);
        let shutdown = Arc::clone(&self.shutdown);

        thread::spawn(move || {
            // 提升定时器精度为 1ms
            unsafe { timeBeginPeriod(1) };

            // 记录并暂时关闭系统的“打字时隐藏鼠标光标”
            let mut original_vanish: i32 = 0;
            unsafe {
                SystemParametersInfoW(
                    SPI_GETMOUSEVANISH,
                    0,
                    &mut original_vanish as *mut i32 as *mut c_void,
                    0,
                );
                if original_vanish != 0 {
                    SystemParametersInfoW(
                        SPI_SETMOUSEVANISH,
                        0,
                        std::ptr::null_mut(),
                        SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                    );
                }
            }

            let mut rng = rand::thread_rng();
            let mut last_hotkey_down = false;
            let mut last_user_active_time = Instant::now() - Duration::from_secs(1);
            // 锁屏/息屏检测按 500ms 节流，避免每次都去开输入桌面
            let mut last_session_check = Instant::now() - Duration::from_secs(10);

            while !shutdown.load(Ordering::Relaxed) {
                // 1. 全局可自定义热键监听
                let current_vk = hotkey_vk.load(Ordering::Relaxed);
                let cur_hotkey_down = if current_vk > 0 { is_key_down(current_vk) } else { false };

                if cur_hotkey_down && !last_hotkey_down {
                    let prev = running.fetch_xor(true, Ordering::SeqCst);
                    let new_state = !prev;
                    log_state_change(new_state);
                    thread::sleep(Duration::from_millis(200)); // 防抖
                }
                last_hotkey_down = cur_hotkey_down;

                // 2. 检查是否处于运行状态
                if running.load(Ordering::Relaxed) {
                    let now = Instant::now();

                    // 锁屏 / 显示器已关闭：必须完全停手。
                    // keybd_event 对系统而言是真实输入，会把黑掉的屏幕重新点亮到锁屏界面，
                    // 而锁屏期间桌宠本来也不计数，发键纯属副作用。
                    if now.duration_since(last_session_check) >= Duration::from_millis(500) {
                        last_session_check = now;
                        if session::should_pause() {
                            thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                    }

                    // 智能避让：用户当前正在打字、按 Shift 切输入法或按住鼠标划词
                    if is_user_actively_operating() {
                        last_user_active_time = now;
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }

                    // 离开物理操作后的礼貌冷却期 (180ms)
                    if now.duration_since(last_user_active_time) < Duration::from_millis(180) {
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }

                    // 3. 随机选择一个 F21~F24 幻影键
                    let vk = PHANTOM_KEYS[rng.gen_range(0..PHANTOM_KEYS.len())];
                    let cfg = get_speed_config(speed_mode.load(Ordering::Relaxed));

                    // 4. 模拟按下与释放
                    let hold_ms = rng.gen_range(cfg.min_hold_ms..=cfg.max_hold_ms);
                    unsafe {
                        keybd_event(vk, 0, 0, 0);
                    }
                    thread::sleep(Duration::from_millis(hold_ms));
                    unsafe {
                        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
                    }

                    total_sent.fetch_add(1, Ordering::Relaxed);

                    // 5. 拟真随机间隔与微停顿
                    // 3% 几率模拟真实人手的短暂微小停顿 (250ms ~ 400ms)
                    if rng.gen_bool(0.03) {
                        let pause = rng.gen_range(250..=400);
                        thread::sleep(Duration::from_millis(pause));
                    } else {
                        let interval = rng.gen_range(cfg.min_interval_ms..=cfg.max_interval_ms);
                        thread::sleep(Duration::from_millis(interval));
                    }
                } else {
                    // 暂停状态下休眠 40ms，避免占用 CPU
                    thread::sleep(Duration::from_millis(40));
                }
            }

            // 退出清理：恢复光标设置与定时器
            unsafe {
                if original_vanish != 0 {
                    SystemParametersInfoW(
                        SPI_SETMOUSEVANISH,
                        0,
                        1 as *mut c_void,
                        SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                    );
                }
                timeEndPeriod(1);
            }
        });
    }
}

fn log_state_change(running: bool) {
    if running {
        println!("[AutoTappo] ▶ 挂机已开启");
    } else {
        println!("[AutoTappo] ⏸ 挂机已暂停");
    }
}
