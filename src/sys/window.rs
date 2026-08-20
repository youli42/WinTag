use anyhow::{Context, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

/// 窗口信息快照
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub process_name: String,
    #[allow(dead_code)]
    pub process_path: String,
    #[allow(dead_code)]
    pub pid: u32,
}

/// 获取当前活动窗口信息
pub fn get_foreground_window_info() -> Result<WindowInfo> {
    // SAFETY: GetForegroundWindow 是只读 API，无副作用
    let hwnd = unsafe { GetForegroundWindow() };

    let title = get_window_title(hwnd)?;
    let (pid, _tid) = get_window_process_id(hwnd);
    let process_name = get_process_name(pid)?;
    let process_path = get_process_path(pid)?;

    Ok(WindowInfo {
        hwnd: hwnd.0 as isize,
        title,
        process_name,
        process_path,
        pid,
    })
}

/// 获取窗口标题
fn get_window_title(hwnd: HWND) -> Result<String> {
    // SAFETY: GetWindowTextLengthW 是只读 API
    let len = unsafe { GetWindowTextLengthW(hwnd) } as usize;
    if len == 0 {
        return Ok(String::new());
    }

    let mut buf = vec![0u16; len + 1];
    // SAFETY: buf 已分配足够空间
    let actual = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    buf.truncate(actual);

    String::from_utf16(&buf).context("窗口标题包含无效 UTF-16")
}

/// 获取窗口所属进程 ID
fn get_window_process_id(hwnd: HWND) -> (u32, u32) {
    let mut pid: u32 = 0;
    // SAFETY: GetWindowThreadProcessId 是只读 API
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid, tid)
}

/// 获取进程名（如 "chrome.exe"）
fn get_process_name(pid: u32) -> Result<String> {
    if pid == 0 {
        return Ok(String::new());
    }

    // SAFETY: 以只读权限打开进程，只查询信息
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) }
        .context("无法打开进程")?;

    let mut buf = [0u16; 260];
    // SAFETY: buf 已分配足够空间，handle 是有效进程句柄
    let len = unsafe { GetModuleBaseNameW(handle, None, &mut buf) } as usize;

    // SAFETY: handle 是有效句柄
    unsafe { windows::Win32::Foundation::CloseHandle(handle) }?;

    if len == 0 {
        return Ok(String::new());
    }

    String::from_utf16(&buf[..len]).context("进程名包含无效 UTF-16")
}

/// 获取进程完整路径
fn get_process_path(pid: u32) -> Result<String> {
    if pid == 0 {
        return Ok(String::new());
    }

    // SAFETY: 以只读权限打开进程
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) }
        .context("无法打开进程")?;

    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    // SAFETY: buf 已分配足够空间
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .context("无法获取进程路径")?;

    // SAFETY: handle 是有效句柄
    unsafe { windows::Win32::Foundation::CloseHandle(handle) }?;

    String::from_utf16(&buf[..len as usize]).context("进程路径包含无效 UTF-16")
}

/// 获取当前进程 PID
#[allow(dead_code)]
pub fn get_current_pid() -> u32 {
    // SAFETY: GetCurrentProcessId 是只读 API
    unsafe { GetCurrentProcessId() }
}
