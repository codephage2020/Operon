use operon_core::{CapabilityError, SegmentUpdate};

#[derive(Clone, Debug)]
pub enum SpeechRecognitionEvent {
    Update(SegmentUpdate),
    Error(String),
    Finished,
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::RcBlock;
    use objc2::{AnyThread, rc::Retained, runtime::Bool};
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    use objc2_avf_audio::{AVAudioEngine, AVAudioInputNode};
    use objc2_foundation::NSLocale;
    use objc2_speech::{
        SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
        SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus,
    };
    use operon_core::SpeechStatus;
    use std::sync::{Arc, Mutex};

    struct Inner {
        engine: Retained<AVAudioEngine>,
        input: Retained<AVAudioInputNode>,
        request: Arc<Mutex<Option<Retained<SFSpeechAudioBufferRecognitionRequest>>>>,
        task: Option<Retained<SFSpeechRecognitionTask>>,
        recognizer: Option<Retained<SFSpeechRecognizer>>,
    }
    unsafe impl Send for Inner {}
    unsafe impl Sync for Inner {}

    pub struct MacSpeechRecognizer {
        inner: Option<Inner>,
    }

    impl MacSpeechRecognizer {
        pub async fn start(
            locale: &str,
            sender: tokio::sync::mpsc::UnboundedSender<SpeechRecognitionEvent>,
        ) -> Result<Self, CapabilityError> {
            request_speech_permission().await?;
            request_microphone_permission().await?;
            unsafe {
                let recognizer = SFSpeechRecognizer::initWithLocale(
                    SFSpeechRecognizer::alloc(),
                    &NSLocale::localeWithLocaleIdentifier(&objc2_foundation::NSString::from_str(
                        locale,
                    )),
                )
                .ok_or_else(|| {
                    CapabilityError::Unavailable(format!("系统不支持语音识别语言 {locale}"))
                })?;
                if !recognizer.isAvailable() {
                    return Err(CapabilityError::Unavailable(
                        "系统语音识别当前不可用，请检查语言与网络".into(),
                    ));
                }
                let request = Arc::new(Mutex::new(Some(
                    SFSpeechAudioBufferRecognitionRequest::new(),
                )));
                let request_ref = request.lock().expect("request lock");
                let request_object = request_ref.as_ref().expect("new request").clone();
                drop(request_ref);
                request_object.setShouldReportPartialResults(true);
                request_object.setAddsPunctuation(true);
                if recognizer.supportsOnDeviceRecognition() {
                    request_object.setRequiresOnDeviceRecognition(true);
                }
                let engine = AVAudioEngine::new();
                let input = engine.inputNode();
                let format = input.outputFormatForBus(0);
                let callback_request = request.clone();
                let tap = RcBlock::new(
                    move |buffer: std::ptr::NonNull<objc2_avf_audio::AVAudioPCMBuffer>, _: std::ptr::NonNull<objc2_avf_audio::AVAudioTime>| {
                        if let Some(request) =
                            callback_request.lock().expect("request lock").as_ref()
                        {
                            request.appendAudioPCMBuffer(buffer.as_ref());
                        }
                    },
                );
                input.installTapOnBus_bufferSize_format_block(
                    0,
                    4096,
                    Some(&format),
                    RcBlock::into_raw(tap).cast(),
                );
                engine.prepare();
                if let Err(error) = engine.startAndReturnError() {
                    input.removeTapOnBus(0);
                    return Err(CapabilityError::Unavailable(format!(
                        "麦克风启动失败：{}",
                        error.localizedDescription()
                    )));
                }
                let segment_id = uuid::Uuid::new_v4().to_string();
                let result_sender = sender.clone();
                let handler = RcBlock::new(
                    move |result: *mut SFSpeechRecognitionResult,
                          error: *mut objc2_foundation::NSError| {
                        if let Some(result) = result.as_ref() {
                            let text = result.bestTranscription().formattedString().to_string();
                            let status = if result.isFinal() {
                                SpeechStatus::Final
                            } else {
                                SpeechStatus::Interim
                            };
                            let _ =
                                result_sender.send(SpeechRecognitionEvent::Update(SegmentUpdate {
                                    id: segment_id.clone(),
                                    text,
                                    status,
                                }));
                            if result.isFinal() {
                                let _ = result_sender.send(SpeechRecognitionEvent::Finished);
                            }
                        }
                        if let Some(error) = error.as_ref() {
                            let _ = result_sender.send(SpeechRecognitionEvent::Error(
                                error.localizedDescription().to_string(),
                            ));
                        }
                    },
                );
                let task =
                    recognizer.recognitionTaskWithRequest_resultHandler(&request_object, &*handler);
                Ok(Self {
                    inner: Some(Inner {
                        engine,
                        input,
                        request,
                        task: Some(task),
                        recognizer: Some(recognizer),
                    }),
                })
            }
        }

        pub async fn stop(&self) {
            if let Some(inner) = &self.inner {
                unsafe {
                    inner.engine.stop();
                    inner.input.removeTapOnBus(0);
                    if let Some(request) = inner.request.lock().expect("request lock").as_ref() {
                        request.endAudio();
                    }
                }
            }
        }
    }

    impl Drop for MacSpeechRecognizer {
        fn drop(&mut self) {
            if let Some(mut inner) = self.inner.take() {
                unsafe {
                    inner.engine.stop();
                    inner.input.removeTapOnBus(0);
                    if let Some(request) = inner.request.lock().expect("request lock").take() {
                        request.endAudio();
                    }
                    if let Some(task) = inner.task.take() {
                        task.cancel();
                    }
                    inner.recognizer.take();
                }
            }
        }
    }

    async fn request_speech_permission() -> Result<(), CapabilityError> {
        unsafe {
            if SFSpeechRecognizer::authorizationStatus()
                == SFSpeechRecognizerAuthorizationStatus::Authorized
            {
                return Ok(());
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let block = RcBlock::new(move |status| {
                if let Some(sender) = sender.lock().expect("permission lock").take() {
                    let _ = sender.send(status);
                }
            });
            SFSpeechRecognizer::requestAuthorization(&*block);
            match receiver
                .await
                .map_err(|_| CapabilityError::Unavailable("无法取得语音识别权限".into()))?
            {
                SFSpeechRecognizerAuthorizationStatus::Authorized => Ok(()),
                _ => Err(CapabilityError::Unavailable(
                    "请在系统设置中允许语音识别权限".into(),
                )),
            }
        }
    }

    async fn request_microphone_permission() -> Result<(), CapabilityError> {
        unsafe {
            let media = AVMediaTypeAudio
                .ok_or_else(|| CapabilityError::Unavailable("系统麦克风接口不可用".into()))?;
            if AVCaptureDevice::authorizationStatusForMediaType(media)
                == objc2_av_foundation::AVAuthorizationStatus::Authorized
            {
                return Ok(());
            }
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let sender = Arc::new(Mutex::new(Some(sender)));
            let block = RcBlock::new(move |granted: Bool| {
                if let Some(sender) = sender.lock().expect("permission lock").take() {
                    let _ = sender.send(granted.as_bool());
                }
            });
            AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &*block);
            if receiver.await.unwrap_or(false) {
                Ok(())
            } else {
                Err(CapabilityError::Unavailable(
                    "请在系统设置中允许麦克风权限".into(),
                ))
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::MacSpeechRecognizer;

#[cfg(not(target_os = "macos"))]
pub struct MacSpeechRecognizer;
#[cfg(not(target_os = "macos"))]
impl MacSpeechRecognizer {
    pub async fn start(
        _: &str,
        _: tokio::sync::mpsc::UnboundedSender<SpeechRecognitionEvent>,
    ) -> Result<Self, CapabilityError> {
        Err(CapabilityError::Unavailable("仅 macOS 支持".into()))
    }
    pub async fn stop(&self) {}
}
