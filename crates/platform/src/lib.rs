mod capture;
mod hotkeys;
mod ocr;
mod speech;
mod translate;

pub use capture::{
    capture_region, cleanup_captures, open_screen_recording_settings, screen_recording_granted,
};
pub use hotkeys::{Hotkey, register_all};
pub use ocr::VisionOcr;
pub use speech::{MacSpeechRecognizer, SpeechRecognitionEvent};
pub use translate::{HttpTranslator, language_code};
