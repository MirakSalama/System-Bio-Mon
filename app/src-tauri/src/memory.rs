use tauri::WebviewWindow;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
};
use windows_core::Interface;

pub fn set_idle(window: &WebviewWindow, idle: bool) {
    let level = if idle {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    let _ = window.with_webview(move |webview| {
        // Tauri runs this on the UI thread that owns the controller. Older
        // WebView2 runtimes can keep using their default memory policy.
        unsafe {
            if let Ok(view) = webview
                .controller()
                .CoreWebView2()
                .and_then(|v| v.cast::<ICoreWebView2_19>())
            {
                let _ = view.SetMemoryUsageTargetLevel(level);
            }
        }
    });
}
