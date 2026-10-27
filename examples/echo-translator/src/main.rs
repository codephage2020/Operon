use operon_core::*;
use tokio::io::{self, AsyncBufReadExt};

#[tokio::main]
async fn main() {
    let mut lines = io::BufReader::new(io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<PluginRequest>(&line) {
            Ok(request)
                if request.protocol == PROTOCOL_VERSION && request.capability_id != "translate" =>
            {
                PluginResponse {
                    protocol: PROTOCOL_VERSION,
                    id: request.id,
                    output: None,
                    error: Some(format!("未知能力：{}", request.capability_id)),
                }
            }
            Ok(request) if request.protocol == PROTOCOL_VERSION => {
                let result = match request.input {
                    Payload::Text(mut data) => {
                        data.text = format!("[演示] {}", data.text);
                        PluginResponse {
                            protocol: PROTOCOL_VERSION,
                            id: request.id,
                            output: Some(Payload::Text(data)),
                            error: None,
                        }
                    }
                    _ => PluginResponse {
                        protocol: PROTOCOL_VERSION,
                        id: request.id,
                        output: None,
                        error: Some("仅接受文本".into()),
                    },
                };
                result
            }
            Ok(request) => PluginResponse {
                protocol: request.protocol,
                id: request.id,
                output: None,
                error: Some("协议不兼容".into()),
            },
            Err(error) => PluginResponse {
                protocol: PROTOCOL_VERSION,
                id: String::new(),
                output: None,
                error: Some(error.to_string()),
            },
        };
        println!(
            "{}",
            serde_json::to_string(&response).expect("serialize response")
        );
    }
}
