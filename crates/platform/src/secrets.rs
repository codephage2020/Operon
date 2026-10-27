//! Secret storage backed by the macOS Keychain.
//!
//! PRD §9.2 requires keys to live outside flow files, logs and exports. Values
//! are stored as generic passwords keyed by plugin instance and config field,
//! and are only read when a step actually runs.

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const SERVICE: &str = "dev.operon.plugin";

    fn account(instance_id: &str, key: &str) -> String {
        format!("{instance_id}/{key}")
    }

    /// Store or replace a secret. Returns an error message suitable for the UI;
    /// the value itself is never included.
    pub fn set_secret(instance_id: &str, key: &str, value: &str) -> Result<(), String> {
        let account = account(instance_id, key);
        // Replace semantics: deleting first avoids duplicate items erroring out.
        let _ = delete_generic_password(SERVICE, &account);
        set_generic_password(SERVICE, &account, value.as_bytes())
            .map_err(|error| format!("无法写入钥匙串：{error}"))
    }

    pub fn get_secret(instance_id: &str, key: &str) -> Option<String> {
        let account = account(instance_id, key);
        get_generic_password(SERVICE, &account)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    pub fn delete_secret(instance_id: &str, key: &str) -> Result<(), String> {
        delete_generic_password(SERVICE, &account(instance_id, key))
            .map_err(|error| format!("无法删除钥匙串条目：{error}"))
    }

    /// Whether a secret exists, without loading it into memory.
    pub fn has_secret(instance_id: &str, key: &str) -> bool {
        get_secret(instance_id, key).is_some()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn set_secret(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("当前平台不支持钥匙串".into())
    }
    pub fn get_secret(_: &str, _: &str) -> Option<String> {
        None
    }
    pub fn delete_secret(_: &str, _: &str) -> Result<(), String> {
        Err("当前平台不支持钥匙串".into())
    }
    pub fn has_secret(_: &str, _: &str) -> bool {
        false
    }
}

pub use platform::{delete_secret, get_secret, has_secret, set_secret};

/// Config fields that must never be written into a flow file or log.
pub fn is_secret_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("key") || key.contains("token") || key.contains("secret") || key.contains("password")
}
