use operon_core::{CapabilityError, Payload};
use std::{
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn temporary_capture_directory() -> PathBuf {
    std::env::temp_dir().join("operon")
}

/// Whether macOS already granted screen recording access. Denied calls would
/// produce an empty image or a silent failure, so the app checks before asking
/// the user to frame a region.
pub fn screen_recording_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_core_graphics::CGPreflightScreenCaptureAccess()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub async fn capture_region() -> Result<Payload, CapabilityError> {
    if !screen_recording_granted() {
        return Err(CapabilityError::Unavailable(
            "还没有屏幕录制权限，无法截图。请在系统设置中授予权限后重新检查。".into(),
        ));
    }
    let directory = temporary_capture_directory();
    std::fs::create_dir_all(&directory)
        .map_err(|error| CapabilityError::Failed(error.to_string()))?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("capture-{id}.png"));
    let result = tokio::task::spawn_blocking({
        let path = path.clone();
        move || {
            Command::new("/usr/sbin/screencapture")
                .arg("-i")
                .arg("-x")
                .arg(&path)
                .status()
        }
    })
    .await
    .map_err(|error| CapabilityError::Failed(error.to_string()))?;
    match result {
        Ok(status) if status.success() && path.is_file() => Ok(Payload::Image {
            path,
            format: "png".into(),
        }),
        Ok(_) => Err(CapabilityError::Cancelled),
        Err(error) => Err(CapabilityError::Unavailable(format!(
            "无法启动系统截图：{error}"
        ))),
    }
}

pub fn open_screen_recording_settings() -> Result<(), CapabilityError> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .status()
        .map_err(|error| CapabilityError::Failed(format!("无法打开系统设置：{error}")))?;
    Ok(())
}

/// Remove temporary screenshot files left by finished runs. Safe to delete all
/// captures because a translation retry reuses the in-memory OCR text and a task
/// retry re-captures, so no terminal run keeps its image alive.
pub fn cleanup_captures() {
    let directory = temporary_capture_directory();
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().is_some_and(|extension| extension == "png") {
            let _ = std::fs::remove_file(path);
        }
    }
}
