use async_trait::async_trait;
use operon_core::*;
use serde::Deserialize;
use std::process::Stdio;

pub struct HttpTranslator {
    pub endpoint: String,
}

impl Default for HttpTranslator {
    fn default() -> Self {
        Self {
            endpoint: "https://api.mymemory.translated.net/get".into(),
        }
    }
}

#[derive(Deserialize)]
struct TranslationResponse {
    #[serde(rename = "responseData")]
    response_data: TranslationData,
    #[serde(rename = "responseStatus")]
    status: serde_json::Value,
}
#[derive(Deserialize)]
struct TranslationData {
    #[serde(rename = "translatedText")]
    translated_text: String,
}

#[async_trait]
impl CapabilityExecutor for HttpTranslator {
    async fn execute(
        &self,
        input: Payload,
        context: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        let Payload::Text(mut data) = input else {
            return Err(CapabilityError::InvalidInput("翻译器仅接受文本".into()));
        };
        let target = context
            .config
            .get("target_language")
            .map(String::as_str)
            .unwrap_or("zh-CN");
        let source = data.language.as_deref().unwrap_or("auto");
        if source != "auto" && language_code(source) == language_code(target) {
            return Ok(Payload::Text(data));
        }
        if context.cancellation.is_cancelled() {
            return Err(CapabilityError::Cancelled);
        }
        let mut arguments = vec![
            "--silent".into(),
            "--show-error".into(),
            "--fail".into(),
            "--max-time".into(),
            "40".into(),
            "--get".into(),
            self.endpoint.clone(),
            "--data-urlencode".into(),
            format!("q={}", data.text),
            "--data-urlencode".into(),
            format!(
                "langpair={}|{}",
                language_code(source),
                language_code(target)
            ),
        ];
        // Built-in services keep their key in the Keychain too, so a key typed
        // in the UI is used without ever entering the flow file.
        let stored_key = context
            .secrets
            .as_ref()
            .and_then(|resolver| resolver.resolve("translate", "api_key"));
        if let Some(key) = context.config.get("api_key").or(stored_key.as_ref()) {
            arguments.extend(["--data-urlencode".into(), format!("key={key}")]);
        }
        let output = tokio::process::Command::new("/usr/bin/curl")
            .args(arguments)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|error| CapabilityError::Failed(format!("无法调用在线翻译：{error}")))?;
        if !output.status.success() {
            return Err(CapabilityError::Unavailable(format!(
                "在线翻译不可用：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let response: TranslationResponse = serde_json::from_slice(&output.stdout)
            .map_err(|error| CapabilityError::Failed(format!("翻译服务响应无效：{error}")))?;
        if response.status != serde_json::json!(200) {
            return Err(CapabilityError::Unavailable(
                "翻译服务拒绝了请求，请稍后重试或检查配置".into(),
            ));
        }
        if response.response_data.translated_text.trim().is_empty() {
            return Err(CapabilityError::Failed("翻译服务返回了空译文".into()));
        }
        data.language = Some(target.to_string());
        data.text = response.response_data.translated_text;
        Ok(Payload::Text(data))
    }
}

pub fn language_code(language: &str) -> &str {    match language {
        "auto" => "Autodetect",
        "zh" | "zh-CN" | "zh-Hans" => "zh-CN",
        "en" | "en-US" => "en",
        "ja" | "ja-JP" => "ja",
        "ko" | "ko-KR" => "ko",
        "fr" | "fr-FR" => "fr",
        "de" | "de-DE" => "de",
        other => other,
    }
}
