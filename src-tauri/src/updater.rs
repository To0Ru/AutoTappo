//! 一键自更新：下载新版本便携版 exe → 备份旧 exe → 原地替换 → 拉起新进程。
//!
//! 完整流程（与用户约定的交互一致）：
//! 1. 新版本下载到**当前 exe 同目录**，文件名 `AutoTappo.exe.new`；
//! 2. 校验（PE 魔数 `MZ` + 最小体积），避免把 HTML 错误页当成程序；
//! 3. 当前 `AutoTappo.exe` 重命名为 `AutoTappo.exe.bak`（运行中的 exe 允许改名，不允许删除）；
//! 4. `AutoTappo.exe.new` 重命名为 `AutoTappo.exe`；
//! 5. 拉起新 exe，旧进程随即退出；
//! 6. **新进程启动时**删除 `AutoTappo.exe.bak`（旧进程退出前文件仍被占用，故带重试）。
//!
//! 任何一步失败都尽量回滚，保证「最坏情况是没更新成功」，绝不是「程序没了」。
//!
//! 下载刻意走 **WinHTTP**（系统自带）而不是引入第三方 HTTP 库：
//! - 零新增 crate，不增加 CI 构建风险；
//! - 自动复用系统代理配置与系统证书库（国内网络/公司代理下更可靠）；
//! - 不会像子进程那样闪出控制台窗口。

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(
        psz_agent_w: *const u16,
        dw_access_type: u32,
        psz_proxy_w: *const u16,
        psz_proxy_bypass_w: *const u16,
        dw_flags: u32,
    ) -> *mut c_void;
    fn WinHttpConnect(
        h_session: *mut c_void,
        pswz_server_name: *const u16,
        n_server_port: u16,
        dw_reserved: u32,
    ) -> *mut c_void;
    fn WinHttpOpenRequest(
        h_connect: *mut c_void,
        pwsz_verb: *const u16,
        pwsz_object_name: *const u16,
        pwsz_version: *const u16,
        pwsz_referrer: *const u16,
        ppwsz_accept_types: *const *const u16,
        dw_flags: u32,
    ) -> *mut c_void;
    fn WinHttpSetTimeouts(
        h_internet: *mut c_void,
        n_resolve_timeout: i32,
        n_connect_timeout: i32,
        n_send_timeout: i32,
        n_receive_timeout: i32,
    ) -> i32;
    fn WinHttpSetOption(
        h_internet: *mut c_void,
        dw_option: u32,
        lp_buffer: *mut c_void,
        dw_buffer_length: u32,
    ) -> i32;
    fn WinHttpSendRequest(
        h_request: *mut c_void,
        lpsz_headers: *const u16,
        dw_headers_length: u32,
        lp_optional: *const c_void,
        dw_optional_length: u32,
        dw_total_length: u32,
        dw_context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(h_request: *mut c_void, lp_reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(
        h_request: *mut c_void,
        dw_info_level: u32,
        pwsz_name: *const u16,
        lp_buffer: *mut c_void,
        lpdw_buffer_length: *mut u32,
        lpdw_index: *mut u32,
    ) -> i32;
    fn WinHttpReadData(
        h_request: *mut c_void,
        lp_buffer: *mut c_void,
        dw_number_of_bytes_to_read: u32,
        lpdw_number_of_bytes_read: *mut u32,
    ) -> i32;
    fn WinHttpCloseHandle(h_internet: *mut c_void) -> i32;
}

const WINHTTP_ACCESS_TYPE_DEFAULT_PROXY: u32 = 0;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_OPTION_REDIRECT_POLICY: u32 = 88;
const WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS: u32 = 2;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

const NEW_SUFFIX: &str = ".new";
const BAK_SUFFIX: &str = ".bak";

/// 便携版正常约 3.6 MB；低于这个体积一定不是完整程序（多半是错误页/限流页）
const MIN_EXE_SIZE: u64 = 300 * 1024;
/// 下载体积上限，防止异常来源把磁盘写满
const MAX_EXE_SIZE: u64 = 128 * 1024 * 1024;

/// 允许的下载来源（白名单，防止前端参数被篡改成任意 URL 下载）
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "codeload.github.com",
    "raw.githubusercontent.com",
    "gitee.com",
    "cdn.jsdelivr.net",
];

/// RAII：保证任何分支下句柄都会被释放（顺序必须从内到外，这里靠 drop 顺序天然满足）
struct WinHandle(*mut c_void);

impl Drop for WinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("获取当前程序路径失败：{e}"))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.display(), suffix))
}

fn log_path() -> PathBuf {
    std::env::temp_dir().join("autotappo-update.log")
}

/// 更新流程的诊断日志（GUI 子系统没有控制台，只能落盘）
fn log_line(msg: &str) {
    let p = log_path();
    if let Ok(meta) = fs::metadata(&p) {
        if meta.len() > 512 * 1024 {
            let _ = fs::remove_file(&p);
        }
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "[update] {msg}");
    }
}

fn host_allowed(host: &str) -> bool {
    ALLOWED_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

fn split_https(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or("只接受 https 下载地址")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "端口解析失败")?),
        None => (authority, 443u16),
    };
    if host.is_empty() {
        return Err("更新地址缺少主机名".into());
    }
    Ok((host.to_ascii_lowercase(), port, path.to_string()))
}

/// 用 WinHTTP 下载到 dest，返回字节数
fn download(url: &str, dest: &Path) -> Result<u64, String> {
    let (host, port, path) = split_https(url)?;

    let agent = wide("AutoTappo-Updater/1.0");
    let host_w = wide(&host);
    let path_w = wide(&path);
    let verb = wide("GET");

    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return Err(format!(
                "初始化 WinHTTP 失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        let session = WinHandle(session);
        WinHttpSetTimeouts(session.0, 20_000, 20_000, 30_000, 60_000);

        let connect = WinHttpConnect(session.0, host_w.as_ptr(), port, 0);
        if connect.is_null() {
            return Err(format!(
                "连接 {host} 失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        let connect = WinHandle(connect);

        let request = WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return Err(format!(
                "创建请求失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        let request = WinHandle(request);

        // 允许自动跟随重定向（GitHub Release 资产会 302 到 Azure Blob）
        let mut policy: u32 = WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS;
        WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_REDIRECT_POLICY,
            &mut policy as *mut u32 as *mut c_void,
            4,
        );

        if WinHttpSendRequest(
            request.0,
            std::ptr::null(),
            0,
            std::ptr::null(),
            0,
            0,
            0,
        ) == 0
        {
            return Err(format!(
                "发送请求失败（{}）",
                std::io::Error::last_os_error()
            ));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(format!(
                "接收响应失败（{}）",
                std::io::Error::last_os_error()
            ));
        }

        let mut status: u32 = 0;
        let mut status_len: u32 = 4;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut status_len,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err("读取 HTTP 状态码失败".into());
        }
        if status != 200 {
            return Err(format!("下载失败，服务器返回 HTTP {status}"));
        }

        let mut file = File::create(dest).map_err(|e| format!("创建临时文件失败：{e}"))?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut total: u64 = 0;

        loop {
            let mut got: u32 = 0;
            if WinHttpReadData(
                request.0,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut got,
            ) == 0
            {
                return Err(format!(
                    "读取下载数据失败（{}）",
                    std::io::Error::last_os_error()
                ));
            }
            if got == 0 {
                break;
            }
            total += got as u64;
            if total > MAX_EXE_SIZE {
                return Err("下载体积超过上限，已中止".into());
            }
            file.write_all(&buf[..got as usize])
                .map_err(|e| format!("写入临时文件失败：{e}"))?;
        }

        file.flush()
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        Ok(total)
    }
}

/// 下载并应用更新。成功返回时新进程已经启动，调用方应立即退出当前进程。
pub fn apply_from_url(url: &str) -> Result<(), String> {
    let url = url.trim().to_string();

    let host = split_https(&url).map(|(h, _, _)| h)?;
    if !host_allowed(&host) {
        return Err(format!("不允许的下载来源：{host}"));
    }

    let cur = current_exe()?;
    let new_path = sibling(&cur, NEW_SUFFIX);
    let bak_path = sibling(&cur, BAK_SUFFIX);
    log_line(&format!("开始更新，来源 {host}，目标 {}", cur.display()));

    let _ = fs::remove_file(&new_path);

    let size = download(&url, &new_path)?;
    log_line(&format!("下载完成，{size} 字节"));

    if size < MIN_EXE_SIZE {
        let _ = fs::remove_file(&new_path);
        return Err(format!(
            "下载文件只有 {size} 字节，明显不是完整程序（可能被限流或跳到了错误页面）"
        ));
    }

    // PE 校验：DOS 头前两字节必须是 MZ
    {
        let mut head = [0u8; 2];
        let mut f = File::open(&new_path).map_err(|e| format!("打开临时文件失败：{e}"))?;
        f.read_exact(&mut head)
            .map_err(|e| format!("读取临时文件失败：{e}"))?;
        if &head != b"MZ" {
            let _ = fs::remove_file(&new_path);
            return Err("下载的文件不是有效的 Windows 可执行文件".into());
        }
    }

    // 备份旧 exe（运行中的 exe 可以改名，不能删除）
    let _ = fs::remove_file(&bak_path);
    fs::rename(&cur, &bak_path)
        .map_err(|e| format!("备份当前程序失败（文件可能被占用）：{e}"))?;
    log_line(&format!("旧版本已备份为 {}", bak_path.display()));

    // 新版本就位
    if let Err(e) = fs::rename(&new_path, &cur) {
        let _ = fs::rename(&bak_path, &cur);
        return Err(format!("替换程序文件失败，已回滚：{e}"));
    }
    log_line("新版本已就位，准备启动");

    // 拉起新版本（同权限继承，不会再弹 UAC）
    if let Err(e) = std::process::Command::new(&cur).spawn() {
        let _ = fs::rename(&cur, &new_path);
        let _ = fs::rename(&bak_path, &cur);
        return Err(format!("启动新版本失败，已回滚：{e}"));
    }
    log_line("新版本已启动");

    Ok(())
}

/// 启动时调用：清理上次更新留下的旧版本备份与未完成的下载。
///
/// `.bak` 是**上一个版本**的 exe，此刻可能仍被尚未退出的老进程占用，
/// 所以放到后台线程里重试删除（最多 10 秒）。
pub fn cleanup_after_update() {
    let Ok(cur) = current_exe() else {
        return;
    };
    let bak_path = sibling(&cur, BAK_SUFFIX);
    let new_path = sibling(&cur, NEW_SUFFIX);

    // 未完成的下载直接删掉
    if new_path.exists() {
        let _ = fs::remove_file(&new_path);
        log_line("已清理未完成的下载文件");
    }

    if !bak_path.exists() {
        return;
    }

    let bak_for_thread = bak_path.clone();
    thread::spawn(move || {
        for i in 0..40u32 {
            if fs::remove_file(&bak_for_thread).is_ok() {
                log_line(&format!("已删除旧版本备份（第 {} 次尝试）", i + 1));
                return;
            }
            if !bak_for_thread.exists() {
                return;
            }
            thread::sleep(Duration::from_millis(250));
        }
        log_line("旧版本备份删除失败（文件一直被占用），下次启动会再试");
    });
}
