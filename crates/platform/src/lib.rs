mod capture;
mod hotkeys;
mod ocr;
mod secrets;
mod speech;
mod translate;

pub use capture::{
    capture_region, cleanup_captures, open_screen_recording_settings, screen_recording_granted,
};
pub use hotkeys::{Hotkey, register_all};
pub use ocr::VisionOcr;
pub use secrets::{delete_secret, get_secret, has_secret, is_secret_field, set_secret};
pub use speech::{MacSpeechRecognizer, SpeechRecognitionEvent};
pub use translate::{HttpTranslator, language_code};
