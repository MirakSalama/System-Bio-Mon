use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL},
    System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::{
        Shell::{SHQueryUserNotificationState, QUNS_RUNNING_D3D_FULL_SCREEN},
        WindowsAndMessaging::{
            FindWindowExW, GetClassNameW, GetForegroundWindow, GetWindowRect,
            GetWindowThreadProcessId, IsIconic,
        },
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameTarget {
    pub pid: u32,
    pub name: String,
}

#[derive(Default)]
pub struct Detector {
    // A numeric identity only. HWND is system-owned and never dereferenced.
    window: usize,
    pid: u32,
    candidate: Option<GameTarget>,
    confirmed: bool,
    exclusive: Option<(Instant, bool)>,
}

impl Detector {
    pub fn foreground(&mut self) -> Option<GameTarget> {
        let frame = unsafe { GetForegroundWindow() };
        if frame.is_null() || unsafe { IsIconic(frame) } != 0 {
            *self = Self::default();
            return None;
        }
        let class = window_class(frame);
        // Older Store games have a shell-owned frame; their CoreWindow owns the presents.
        let window = if class == "ApplicationFrameWindow" {
            unsafe {
                FindWindowExW(
                    frame,
                    std::ptr::null_mut(),
                    windows_sys::w!("Windows.UI.Core.CoreWindow"),
                    std::ptr::null(),
                )
            }
        } else {
            frame
        };
        let mut pid = 0;
        if !window.is_null() {
            unsafe { GetWindowThreadProcessId(window, &mut pid) };
        }
        if pid == 0 || pid == std::process::id() {
            *self = Self::default();
            return None;
        }
        if (window as usize, pid) != (self.window, self.pid) {
            self.window = window as usize;
            self.pid = pid;
            self.exclusive = None;
            let app = process_path(pid).and_then(|path| classify(&path, &class));
            self.confirmed = app.as_ref().is_some_and(|(_, confirmed)| *confirmed);
            self.candidate = app.map(|(name, _)| GameTarget { pid, name });
        }
        let candidate = self.candidate.as_ref()?;
        if !self.confirmed {
            // The generic "busy/fullscreen" shell state also covers presentations and video.
            // Only the exclusive Direct3D state can qualify an otherwise unknown executable.
            let (checked, active) = self
                .exclusive
                .get_or_insert_with(|| (Instant::now(), exclusive_3d(frame)));
            if checked.elapsed() >= Duration::from_secs(1) {
                *active = exclusive_3d(frame);
                *checked = Instant::now();
            }
            if !*active {
                return None;
            }
        }
        Some(candidate.clone())
    }
}

fn window_class(window: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(window, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn process_path(pid: u32) -> Option<PathBuf> {
    let proc = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if proc.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    let ok =
        unsafe { QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) };
    unsafe { CloseHandle(proc) };
    (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buf[..len as usize])))
}

fn exclusive_3d(window: HWND) -> bool {
    let mut bounds = RECT::default();
    let mut monitor = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let display = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        if display.is_null()
            || GetMonitorInfoW(display, &mut monitor) == 0
            || GetWindowRect(window, &mut bounds) == 0
        {
            return false;
        }
        let screen = monitor.rcMonitor;
        if bounds.left > screen.left
            || bounds.top > screen.top
            || bounds.right < screen.right
            || bounds.bottom < screen.bottom
        {
            return false;
        }
        let mut state = 0;
        SHQueryUserNotificationState(&mut state) >= 0 && state == QUNS_RUNNING_D3D_FULL_SCREEN
    }
}

/// Distinguish the game from its store, editors, crash reporters and launchers.
/// Return only a short label, never a path, window title or command line.
fn classify(path: &Path, class: &str) -> Option<(String, bool)> {
    let stem = path.file_stem()?.to_str()?;
    let exe = stem.to_ascii_lowercase();
    if matches!(
        exe.as_str(),
        "chrome"
            | "msedge"
            | "firefox"
            | "brave"
            | "opera"
            | "vivaldi"
            | "msedgewebview2"
            | "explorer"
            | "applicationframehost"
            | "dwm"
            | "steam"
            | "steamwebhelper"
            | "eadesktop"
            | "galaxyclient"
            | "upc"
            | "gamebar"
            | "xboxpcapp"
            | "discord"
            | "obs32"
            | "obs64"
            | "blender"
            | "wallpaper32"
            | "wallpaper64"
            | "vtube studio"
            | "powerpnt"
            | "vlc"
            | "mpv"
            | "mpc-hc64"
            | "code"
            | "devenv"
            | "unity"
            | "ue4editor"
            | "gpu dual monitor"
            | "gpu-dual-monitor"
            | "system bio mon"
            | "system-bio-mon"
    ) || exe.starts_with("unrealeditor")
        || exe.starts_with("godot_v")
        || [
            "launcher",
            "crashreport",
            "crashpad",
            "anticheat",
            "updater",
            "unins",
        ]
        .iter()
        .any(|part| exe.contains(part))
    {
        return None;
    }

    let dirs: Vec<_> = path.parent()?.iter().filter_map(|s| s.to_str()).collect();
    let mut title = None;
    for (i, &dir) in dirs.iter().enumerate() {
        let prior = i
            .checked_sub(1)
            .and_then(|j| dirs.get(j))
            .copied()
            .unwrap_or("");
        if (dir.eq_ignore_ascii_case("common") && prior.eq_ignore_ascii_case("steamapps"))
            || (dir.eq_ignore_ascii_case("games")
                && (prior.eq_ignore_ascii_case("GOG Galaxy")
                    || prior.eq_ignore_ascii_case("Ubisoft Game Launcher")))
            || ["Epic Games", "GOG Games", "XboxGames", "EA Games"]
                .iter()
                .any(|s| dir.eq_ignore_ascii_case(s))
        {
            title = dirs.get(i + 1).copied();
            break;
        }
    }
    if title.is_some_and(|dir| {
        [
            "Launcher",
            "Steamworks Shared",
            "SteamVR",
            "Wallpaper Engine",
            "Blender",
        ]
        .iter()
        .any(|s| dir.eq_ignore_ascii_case(s))
            || dir.starts_with("UE_")
    }) {
        return None;
    }
    let java_game = matches!(exe.as_str(), "java" | "javaw") && matches!(class, "GLFW30" | "LWJGL");
    let engine = matches!(class, "UnityWndClass" | "UnrealWindow" | "Valve001")
        || (class == "Engine" && path.with_extension("pck").is_file());
    let known = matches!(exe.as_str(), "robloxplayerbeta" | "minecraft.windows");
    let is_game = title.is_some() || engine || java_game || known;
    if !is_game
        && (class.starts_with("Chrome_WidgetWin_")
            || class.starts_with("Mozilla")
            || class == "CabinetWClass")
    {
        return None;
    }
    let label = title.unwrap_or_else(|| stem.strip_suffix("-Win64-Shipping").unwrap_or(stem));
    let label: String = label
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() && *c == ' ' || "-_.'()".contains(*c))
        .take(64)
        .collect();
    Some((
        if label.trim().is_empty() {
            "Game".into()
        } else {
            label.trim().into()
        },
        is_game,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_windowed_library_games_and_engines() {
        let expected = Some(("Orbital".into(), true));
        assert_eq!(
            classify(
                Path::new(r"D:\Steam\SteamApps\Common\Orbital\bin\client.exe"),
                "SDL_app"
            ),
            expected
        );
        assert_eq!(
            classify(Path::new(r"D:\Standalone\Orbital.exe"), "UnityWndClass"),
            expected
        );
        assert_eq!(
            classify(
                Path::new(r"D:\Portable\Orbital-Win64-Shipping.exe"),
                "UnrealWindow"
            ),
            expected
        );
        assert_eq!(
            classify(
                Path::new(r"D:\XboxGames\Orbital\Content\Play.exe"),
                "ApplicationFrameWindow"
            ),
            expected
        );
    }

    #[test]
    fn browsers_launchers_and_editors_do_not_trigger_capture() {
        for (path, class) in [
            (r"D:\Portable\chrome.exe", "Chrome_WidgetWin_1"),
            (
                r"D:\Epic Games\Launcher\Engine\EpicGamesLauncher.exe",
                "UnrealWindow",
            ),
            (
                r"D:\Epic Games\UE_5.5\Engine\UnrealEditor.exe",
                "UnrealWindow",
            ),
            (
                r"D:\Steam\steamapps\common\Something\CrashReportClient.exe",
                "UnrealWindow",
            ),
            (r"D:\Tools\custom.exe", "Chrome_WidgetWin_1"),
        ] {
            assert_eq!(classify(Path::new(path), class), None);
        }
        assert_eq!(
            classify(Path::new(r"C:\Other\Demo.exe"), "SDL_app"),
            Some(("Demo".into(), false))
        );
    }
}
