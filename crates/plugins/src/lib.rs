use async_trait::async_trait;
use operon_core::*;
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};

pub struct PluginProcess {
    manifest: PluginManifest,
    root: PathBuf,
    capability_id: Option<String>,
    response_timeout: Duration,
}

impl PluginProcess {
    pub fn new(manifest: PluginManifest, root: PathBuf) -> Self {
        Self {
            manifest,
            root,
            capability_id: None,
            response_timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_capability_id(mut self, capability_id: impl Into<String>) -> Self {
        self.capability_id = Some(capability_id.into());
        self
    }

    pub fn with_response_timeout(mut self, timeout: Duration) -> Self {
        self.response_timeout = timeout;
        self
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn capability_id(&self, context: &ExecutionContext) -> String {
        context
            .config
            .get("capability_id")
            .cloned()
            .or_else(|| self.capability_id.clone())
            .or_else(|| {
                self.manifest
                    .capabilities
                    .first()
                    .map(|item| item.id.clone())
            })
            .unwrap_or_default()
    }

    async fn child(&self) -> Result<Child, CapabilityError> {
        if self.manifest.protocol != PROTOCOL_VERSION {
            return Err(CapabilityError::Unavailable(format!(
                "插件协议 {} 与宿主 {} 不兼容",
                self.manifest.protocol, PROTOCOL_VERSION
            )));
        }
        let path = executable_path(&self.root, &self.manifest.executable)?;
        Command::new(path)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| CapabilityError::Unavailable(format!("插件启动失败：{error}")))
    }

    async fn execute_child(
        &self,
        child: &mut Child,
        request: &PluginRequest,
        context: &ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CapabilityError::Failed("无法连接插件输入".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CapabilityError::Failed("无法连接插件输出".into()))?;
        let message = serde_json::to_string(request)
            .map_err(|error| CapabilityError::Failed(error.to_string()))?
            + "\n";
        let write = async {
            stdin
                .write_all(message.as_bytes())
                .await
                .map_err(io_error)?;
            stdin.shutdown().await.map_err(io_error)
        };
        tokio::select! {
            _ = cancelled(context.cancellation.clone()) => return Err(CapabilityError::Cancelled),
            result = write => result?,
        }
        let mut lines = BufReader::new(stdout).lines();
        let line = tokio::select! {
            _ = cancelled(context.cancellation.clone()) => return Err(CapabilityError::Cancelled),
            result = tokio::time::timeout(self.response_timeout, lines.next_line()) => result.map_err(|_| CapabilityError::Failed("插件无响应".into()))?.map_err(io_error)?.ok_or_else(|| CapabilityError::Failed("插件未返回结果".into()))?
        };
        let response: PluginResponse = serde_json::from_str(&line)
            .map_err(|error| CapabilityError::Failed(format!("插件协议错误：{error}")))?;
        if response.protocol != PROTOCOL_VERSION || response.id != request.id {
            return Err(CapabilityError::Failed("插件响应标识或版本不匹配".into()));
        }
        if let Some(error) = response.error {
            return Err(CapabilityError::Failed(error));
        }
        response
            .output
            .ok_or_else(|| CapabilityError::Failed("插件缺少输出".into()))
    }

    async fn reap(&self, child: &mut Child, success: bool) {
        if !success {
            let _ = child.kill().await;
        }
        let _ = child.wait().await;
    }
}

#[async_trait]
impl CapabilityExecutor for PluginProcess {
    async fn execute(
        &self,
        input: Payload,
        context: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        if context.cancellation.is_cancelled() {
            return Err(CapabilityError::Cancelled);
        }
        let request = PluginRequest {
            protocol: PROTOCOL_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            capability_id: self.capability_id(&context),
            input,
            config: context.config.clone(),
        };
        let mut child = self.child().await?;
        let result = self.execute_child(&mut child, &request, &context).await;
        self.reap(&mut child, result.is_ok()).await;
        result
    }
}

async fn cancelled(token: CancellationToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
fn io_error(error: std::io::Error) -> CapabilityError {
    CapabilityError::Failed(format!("插件通信失败：{error}"))
}

fn executable_path(root: &Path, value: &str) -> Result<PathBuf, CapabilityError> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(CapabilityError::Unavailable(
            "插件可执行路径必须是包内相对路径".into(),
        ));
    }
    let path = root.join(relative);
    if !path.is_file() {
        return Err(CapabilityError::Unavailable("插件可执行文件不存在".into()));
    }
    Ok(path)
}

#[derive(Clone)]
pub struct PluginManager {
    root: PathBuf,
    plugins: Arc<Mutex<Vec<InstalledPlugin>>>,
}

#[derive(Clone)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub enabled: bool,
}

impl PluginManager {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            plugins: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Load installed packages from the plugin directory so restarts keep
    /// previously installed plugins available.
    pub fn scan(&self) -> anyhow::Result<()> {
        let mut found = Vec::new();
        if self.root.is_dir() {
            for entry in std::fs::read_dir(&self.root)? {
                let directory = entry?.path();
                let manifest = directory.join("plugin.json");
                if !manifest.is_file() {
                    continue;
                }
                let manifest: PluginManifest = serde_json::from_slice(&std::fs::read(manifest)?)?;
                validate_manifest(&manifest)?;
                found.push(InstalledPlugin {
                    manifest,
                    path: directory,
                    enabled: true,
                });
            }
        }
        *self.plugins.lock().expect("plugin list lock") = found;
        Ok(())
    }

    pub fn list(&self) -> Vec<InstalledPlugin> {
        self.plugins.lock().expect("plugin list lock").clone()
    }

    pub fn install(&self, package: &Path) -> anyhow::Result<PluginInstance> {
        // Accept either the package directory or its plugin.json directly.
        let package = if package.is_dir() {
            package.to_path_buf()
        } else if package.file_name().is_some_and(|name| name == "plugin.json") {
            package
                .parent()
                .ok_or_else(|| anyhow::anyhow!("插件包路径无效"))?
                .to_path_buf()
        } else {
            anyhow::bail!("请选择包含 plugin.json 的插件包目录");
        };
        let manifest: PluginManifest =
            serde_json::from_slice(&std::fs::read(package.join("plugin.json"))?)?;
        validate_manifest(&manifest)?;
        executable_path(&package, &manifest.executable)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let destination = self.root.join(&manifest.id);
        if destination.exists() {
            anyhow::bail!("插件 {} 已安装", manifest.id);
        }
        std::fs::create_dir_all(&self.root)?;
        copy_directory(&package, &destination)?;
        let instance = instance(&manifest, true);
        self.plugins
            .lock()
            .expect("plugin list lock")
            .push(InstalledPlugin {
                manifest,
                path: destination,
                enabled: true,
            });
        Ok(instance)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> anyhow::Result<()> {
        let mut plugins = self.plugins.lock().expect("plugin list lock");
        let plugin = plugins
            .iter_mut()
            .find(|item| item.manifest.id == id)
            .ok_or_else(|| anyhow::anyhow!("插件未安装"))?;
        plugin.enabled = enabled;
        Ok(())
    }

    pub fn uninstall(&self, id: &str) -> anyhow::Result<()> {
        let mut plugins = self.plugins.lock().expect("plugin list lock");
        let index = plugins
            .iter()
            .position(|item| item.manifest.id == id)
            .ok_or_else(|| anyhow::anyhow!("插件未安装"))?;
        let plugin = plugins.remove(index);
        std::fs::remove_dir_all(plugin.path)?;
        Ok(())
    }

    /// Executors for every installed plugin, keyed by instance ID.
    pub fn executors(&self) -> Vec<(PluginInstance, Arc<dyn CapabilityExecutor>)> {
        self.plugins
            .lock()
            .expect("plugin list lock")
            .iter()
            .map(|plugin| {
                (
                    instance(&plugin.manifest, plugin.enabled),
                    Arc::new(PluginProcess::new(
                        plugin.manifest.clone(),
                        plugin.path.clone(),
                    ))
                        as Arc<dyn CapabilityExecutor>,
                )
            })
            .collect()
    }
}

fn instance(manifest: &PluginManifest, enabled: bool) -> PluginInstance {
    PluginInstance {
        id: format!("{}:default", manifest.id),
        plugin_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        enabled,
        capabilities: manifest.capabilities.clone(),
        config: Default::default(),
        endpoint: None,
    }
}

fn validate_manifest(manifest: &PluginManifest) -> anyhow::Result<()> {
    if manifest.protocol != PROTOCOL_VERSION {
        anyhow::bail!("不支持的插件协议 {}", manifest.protocol);
    }
    if manifest.id.is_empty()
        || !manifest
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        anyhow::bail!("插件 ID 无效");
    }
    if manifest.capabilities.is_empty() {
        anyhow::bail!("插件至少声明一个能力");
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == "plugin.json" || entry.file_type()?.is_symlink() {
            continue;
        }
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    std::fs::copy(source.join("plugin.json"), destination.join("plugin.json"))?;
    Ok(())
}
