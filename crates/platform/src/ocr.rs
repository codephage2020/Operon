use async_trait::async_trait;
use operon_core::*;

pub struct VisionOcr;

#[cfg(target_os = "macos")]
#[async_trait]
impl CapabilityExecutor for VisionOcr {
    async fn execute(
        &self,
        input: Payload,
        _: ExecutionContext,
    ) -> Result<Payload, CapabilityError> {
        let Payload::Image { path, .. } = input else {
            return Err(CapabilityError::InvalidInput(
                "Vision OCR 仅接受图片".into(),
            ));
        };
        tokio::task::spawn_blocking(move || recognize(&path))
            .await
            .map_err(|error| CapabilityError::Failed(error.to_string()))?
    }
}

#[cfg(target_os = "macos")]
fn recognize(path: &std::path::Path) -> Result<Payload, CapabilityError> {
    use objc2::{AnyThread, rc::Retained};
    use objc2_core_foundation::CFURL;
    use objc2_foundation::{NSArray, NSString};
    use objc2_image_io::CGImageSource;
    use objc2_vision::{
        VNImageBasedRequest, VNImageRequestHandler, VNRecognizeTextRequest,
        VNRecognizedTextObservation, VNRequest, VNRequestTextRecognitionLevel,
    };
    unsafe {
        let bytes = path.to_string_lossy().as_bytes().to_vec();
        let url = CFURL::from_file_system_representation(
            None,
            bytes.as_ptr(),
            bytes.len().try_into().unwrap(),
            false,
        )
        .ok_or_else(|| CapabilityError::InvalidInput("无法读取截图路径".into()))?;
        let source = CGImageSource::with_url(&url, None)
            .ok_or_else(|| CapabilityError::InvalidInput("无法读取截图".into()))?;
        let image = source
            .image_at_index(0, None)
            .ok_or_else(|| CapabilityError::InvalidInput("无法解码截图".into()))?;
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        request.setAutomaticallyDetectsLanguage(true);
        request.setRecognitionLanguages(&NSArray::from_retained_slice(&[
            NSString::from_str("zh-Hans"),
            NSString::from_str("en-US"),
        ]));
        let handler = VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            &image,
            &objc2_foundation::NSDictionary::new(),
        );
        let based: Retained<VNImageBasedRequest> = Retained::into_super(request);
        let generic: Retained<VNRequest> = Retained::into_super(based);
        handler
            .performRequests_error(&NSArray::from_retained_slice(&[generic.clone()]))
            .map_err(|error| {
                CapabilityError::Failed(format!("OCR 失败：{}", error.localizedDescription()))
            })?;
        let request = generic
            .downcast::<VNRecognizeTextRequest>()
            .map_err(|_| CapabilityError::Failed("OCR 响应类型无效".into()))?;
        let observations = request
            .results()
            .ok_or_else(|| CapabilityError::Failed("OCR 未返回结果".into()))?;
        let mut blocks = Vec::new();
        for item in observations.iter() {
            if let Some(observation) = item.downcast_ref::<VNRecognizedTextObservation>() {
                if let Some(candidate) = observation.topCandidates(1).iter().next() {
                    let bounds = observation.boundingBox();
                    blocks.push(TextBlock {
                        text: candidate.string().to_string(),
                        confidence: Some(candidate.confidence()),
                        bounds: Some(Rect {
                            x: bounds.origin.x,
                            y: bounds.origin.y,
                            width: bounds.size.width,
                            height: bounds.size.height,
                        }),
                    });
                }
            }
        }
        let text = blocks
            .iter()
            .map(|block| block.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        if text.trim().is_empty() {
            return Err(CapabilityError::Failed(
                "没有识别到文字，请重新框选区域".into(),
            ));
        }
        Ok(Payload::Text(TextData {
            text,
            language: None,
            blocks,
        }))
    }
}

#[cfg(not(target_os = "macos"))]
#[async_trait]
impl CapabilityExecutor for VisionOcr {
    async fn execute(&self, _: Payload, _: ExecutionContext) -> Result<Payload, CapabilityError> {
        Err(CapabilityError::Unavailable(
            "Vision OCR 仅在 macOS 可用".into(),
        ))
    }
}
