//! System-wide hotkeys via Carbon RegisterEventHotKey.
//!
//! Carbon delivers hotkey events on the main event loop, which GPUI pumps, so
//! the callback stays non-blocking and hands work to a channel the UI reads.

use std::os::raw::{c_int, c_uint, c_void};
use std::sync::mpsc::{self, Sender};

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

type EventRef = *mut c_void;
type OSStatus = c_int;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
struct HotKeyID {
    creator: u32,
    id: u32,
}

unsafe extern "C" {
    fn GetEventDispatcherTarget() -> EventRef;
    fn InstallEventHandler(
        target: EventRef,
        handler: unsafe extern "C" fn(EventRef, EventRef, *mut c_void) -> OSStatus,
        num_types: c_int,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: c_uint,
        modifiers: c_uint,
        hot_id: HotKeyID,
        signature: u32,
        options: c_uint,
        out_ref: *mut EventRef,
    ) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        param_name: u32,
        type_code: u32,
        desired_type: *mut u32,
        buffer_size: c_uint,
        actual_size: *mut c_uint,
        out_data: *mut c_void,
    ) -> OSStatus;
}

const SIGNATURE: u32 = u32::from_le_bytes(*b"Oprn");
const EVENT_CLASS_HOT_KEY: u32 = u32::from_le_bytes(*b"khot");
const K_EVENT_HOT_KEY_PRESSED: u32 = 0;
const EVENT_PARAM_HOT_KEY: u32 = u32::from_le_bytes(*b"hKey");
const HOT_KEY_ID_TYPE: u32 = u32::from_le_bytes(*b"hKID");
const EVENT_NOT_HANDLED: OSStatus = -9874;

// Carbon modifier mask bits.
const MOD_OPTION: u32 = 0x0800;
const MOD_CONTROL: u32 = 0x1000;

// kVK_* virtual keycodes from the Carbon SDK headers.
const KEY_T: u32 = 0x11;
const KEY_O: u32 = 0x1F;
const KEY_S: u32 = 0x12;
const KEY_R: u32 = 0x10;
const KEY_ESC: u32 = 0x35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    ScreenshotTranslate,
    ScreenshotOcr,
    TranslateText,
    StartSpeech,
    Cancel,
}

impl Hotkey {
    pub fn label(self) -> &'static str {
        match self {
            Self::ScreenshotTranslate => "⌃⌥T",
            Self::ScreenshotOcr => "⌃⌥O",
            Self::TranslateText => "⌃⌥S",
            Self::StartSpeech => "⌃⌥R",
            Self::Cancel => "⌃⌥⎋",
        }
    }

    fn id(self) -> u32 {
        match self {
            Self::ScreenshotTranslate => 1,
            Self::ScreenshotOcr => 2,
            Self::TranslateText => 3,
            Self::StartSpeech => 4,
            Self::Cancel => 5,
        }
    }

    fn from_id(id: u32) -> Option<Self> {
        match id {
            1 => Some(Self::ScreenshotTranslate),
            2 => Some(Self::ScreenshotOcr),
            3 => Some(Self::TranslateText),
            4 => Some(Self::StartSpeech),
            5 => Some(Self::Cancel),
            _ => None,
        }
    }

    fn binding(self) -> (u32, u32) {
        let modifiers = MOD_CONTROL | MOD_OPTION;
        match self {
            Self::ScreenshotTranslate => (KEY_T, modifiers),
            Self::ScreenshotOcr => (KEY_O, modifiers),
            Self::TranslateText => (KEY_S, modifiers),
            Self::StartSpeech => (KEY_R, modifiers),
            Self::Cancel => (KEY_ESC, modifiers),
        }
    }
}

pub const ALL: [Hotkey; 5] = [
    Hotkey::ScreenshotTranslate,
    Hotkey::ScreenshotOcr,
    Hotkey::TranslateText,
    Hotkey::StartSpeech,
    Hotkey::Cancel,
];

thread_local! {
    static SINK: std::cell::RefCell<Option<Sender<Hotkey>>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "C" fn handle(_: EventRef, event: EventRef, _: *mut c_void) -> OSStatus {
    let mut id = HotKeyID { creator: 0, id: 0 };
    let status = unsafe {
        GetEventParameter(
            event,
            EVENT_PARAM_HOT_KEY,
            HOT_KEY_ID_TYPE,
            std::ptr::null_mut(),
            std::mem::size_of::<HotKeyID>() as c_uint,
            std::ptr::null_mut(),
            &mut id as *mut _ as *mut c_void,
        )
    };
    if status != 0 {
        return status;
    }
    if id.creator != SIGNATURE {
        return EVENT_NOT_HANDLED;
    }
    let Some(hotkey) = Hotkey::from_id(id.id) else {
        return EVENT_NOT_HANDLED;
    };
    SINK.with(|sink| {
        if let Some(sender) = sink.borrow().as_ref() {
            let _ = sender.send(hotkey);
        }
    });
    0
}

/// Register the system-wide hotkeys and deliver presses to the returned
/// receiver, which the UI should drain on the main thread. Must run on the
/// main thread. Returns false when the app cannot own its key handler; a
/// combination taken by another app is reported in the returned list of the
/// hotkeys that failed.
pub fn register_all() -> (mpsc::Receiver<Hotkey>, Vec<Hotkey>) {
    let (sender, receiver) = mpsc::channel();
    SINK.with(|cell| *cell.borrow_mut() = Some(sender));
    let spec = EventTypeSpec {
        event_class: EVENT_CLASS_HOT_KEY,
        event_kind: K_EVENT_HOT_KEY_PRESSED,
    };
    let installed = unsafe {
        InstallEventHandler(
            GetEventDispatcherTarget(),
            handle,
            1,
            &spec,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
    };
    if !installed {
        return (receiver, ALL.to_vec());
    }
    let failed = ALL
        .into_iter()
        .filter(|hotkey| {
            let (code, modifiers) = hotkey.binding();
            unsafe {
                RegisterEventHotKey(
                    code,
                    modifiers,
                    HotKeyID {
                        creator: SIGNATURE,
                        id: hotkey.id(),
                    },
                    SIGNATURE,
                    0,
                    std::ptr::null_mut(),
                ) != 0
            }
        })
        .collect();
    (receiver, failed)
}
