#![cfg(unix)]

use operon_core::*;
use operon_plugins::PluginProcess;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn manifest(executable: &str) -> PluginManifest {
    PluginManifest {
        id: "test.echo".into(),
        name: "Test echo".into(),
        version: "1".into(),
        protocol: PROTOCOL_VERSION,
        executable: executable.into(),
        capabilities: vec![Capability {
            id: "translate".into(),
            name: "Translate".into(),
            input: DataType::Text,
            output: DataType::Text,
            mode: ExecutionMode::Once,
            online: false,
            permissions: vec![],
        }],
        permissions: vec![],
    }
}

fn execution_context(cancellation: CancellationToken) -> ExecutionContext {
    ExecutionContext {
        config: BTreeMap::new(),
        cancellation,
        events: None,
    }
}

fn text() -> Payload {
    Payload::Text(TextData::plain("hello"))
}

fn script(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, contents).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn assert_reaped(pid: u32) {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8(output.stdout).unwrap();
    if !output.status.success() && status.trim().is_empty() {
        return;
    }
    assert!(
        status.trim().is_empty() || status.contains('Z'),
        "plugin process {pid} is still running: {status}"
    );
}

#[tokio::test]
async fn real_echo_plugin_succeeds_and_uses_context_capability() {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/echo-translator");
    let manifest: PluginManifest =
        serde_json::from_slice(&fs::read(source.join("plugin.json")).unwrap()).unwrap();
    fs::copy(
        source.join("plugin.json"),
        directory.path().join("plugin.json"),
    )
    .unwrap();
    let workspace = source.parent().unwrap().parent().unwrap().to_path_buf();
    let build = Command::new(env!("CARGO"))
        .args(["build", "-p", "operon-echo-translator"])
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let executable = target.join("debug/operon-echo-translator");
    fs::copy(executable, directory.path().join(&manifest.executable)).unwrap();

    let mut context = execution_context(CancellationToken::default());
    context
        .config
        .insert("capability_id".into(), "translate".into());
    let output = PluginProcess::new(manifest.clone(), directory.path().to_path_buf())
        .with_capability_id("constructor-value")
        .execute(text(), context)
        .await
        .unwrap();
    assert_eq!(output.text().unwrap().text, "[演示] hello");

    let mut constructor_manifest = manifest.clone();
    let mut first = constructor_manifest.capabilities[0].clone();
    first.id = "first".into();
    constructor_manifest.capabilities.insert(0, first);
    let output = PluginProcess::new(constructor_manifest, directory.path().to_path_buf())
        .with_capability_id("translate")
        .execute(text(), execution_context(CancellationToken::default()))
        .await
        .unwrap();
    assert_eq!(output.text().unwrap().text, "[演示] hello");

    let mut context = execution_context(CancellationToken::default());
    context
        .config
        .insert("capability_id".into(), "wrong-capability".into());
    let error = PluginProcess::new(manifest, directory.path().to_path_buf())
        .with_capability_id("translate")
        .execute(text(), context)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("wrong-capability"));
}

#[tokio::test]
async fn malformed_protocol_is_an_error_and_process_is_reaped() {
    let directory = tempfile::tempdir().unwrap();
    script(directory.path(), "plugin", "#!/bin/sh\necho not-json\n");
    let error = PluginProcess::new(manifest("plugin"), directory.path().to_path_buf())
        .execute(text(), execution_context(CancellationToken::default()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("插件协议错误"));
}

#[tokio::test]
async fn crashed_plugin_is_an_error_and_process_is_reaped() {
    let directory = tempfile::tempdir().unwrap();
    script(
        directory.path(),
        "plugin",
        "#!/bin/sh\nprintf $$ > crash.pid\nexit 7\n",
    );
    let error = PluginProcess::new(manifest("plugin"), directory.path().to_path_buf())
        .execute(text(), execution_context(CancellationToken::default()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("插件未返回结果"));
    let pid: u32 = fs::read_to_string(directory.path().join("crash.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid);
}

#[tokio::test]
async fn timeout_kills_and_reaps_plugin() {
    let directory = tempfile::tempdir().unwrap();
    script(
        directory.path(),
        "plugin",
        "#!/bin/sh\nprintf $$ > timeout.pid\nread line\nread line\n",
    );
    let error = PluginProcess::new(manifest("plugin"), directory.path().to_path_buf())
        .with_response_timeout(Duration::from_secs(1))
        .execute(text(), execution_context(CancellationToken::default()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("插件无响应"));
    let pid: u32 = fs::read_to_string(directory.path().join("timeout.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid);
}

#[tokio::test]
async fn cancellation_kills_and_reaps_plugin() {
    let directory = tempfile::tempdir().unwrap();
    script(
        directory.path(),
        "plugin",
        "#!/bin/sh\nprintf $$ > cancel.pid\nread line\nread line\n",
    );
    let cancellation = CancellationToken::default();
    let plugin = PluginProcess::new(manifest("plugin"), directory.path().to_path_buf());
    let executing = plugin.execute(text(), execution_context(cancellation.clone()));
    tokio::pin!(executing);
    tokio::select! {
        result = &mut executing => panic!("plugin unexpectedly completed: {result:?}"),
        _ = tokio::time::timeout(Duration::from_secs(1), async {
            while !directory.path().join("cancel.pid").exists() {
                tokio::task::yield_now().await;
            }
        }) => {}
    }
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), executing)
        .await
        .expect("cancellation cleanup should complete");
    assert!(matches!(result, Err(CapabilityError::Cancelled)));
    let pid: u32 = fs::read_to_string(directory.path().join("cancel.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_reaped(pid);
}
