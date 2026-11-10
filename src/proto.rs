//! Application messages carried inside the encrypted Noise channel.
use crate::transfer::Manifest;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 3;

/// Shared clipboard content.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind")]
pub enum Clip {
    Text { text: String },
    /// A PNG image, base64-encoded (screenshots, snips, copied pictures).
    Image { png: String },
    /// Files or folders copied in a file manager. Only the list travels here;
    /// the contents are pulled over parallel data connections when needed.
    Files { manifest: Manifest },
}

/// Mouse buttons on the wire.
pub const BTN_LEFT: u8 = 1;
pub const BTN_RIGHT: u8 = 2;
pub const BTN_MIDDLE: u8 = 3;
pub const BTN_BACK: u8 = 4;
pub const BTN_FORWARD: u8 = 5;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t")]
pub enum ToServer {
    Hello {
        name: String,
        width: i32,
        height: i32,
        os: String,
        version: u32,
    },
    Clipboard {
        text: String,
    },
    ClipboardImage {
        png: String,
    },
    Pong,
    /// Reply to `FilesGrab`: what was being dragged here (empty = not files).
    FilesGrabbed {
        files: Vec<String>,
    },
    /// First message on a file-transfer data connection (instead of Hello).
    DataHello {
        id: u64,
    },
    /// Files copied on this client (sent when the mouse leaves it).
    ClipboardFiles {
        manifest: Manifest,
    },
    /// Files dragged from this client and dropped on the server.
    DropOffer {
        manifest: Manifest,
    },
}

/// Coordinates are pixels in the client's own screen space (0..width, 0..height).
/// Key codes are Linux evdev codes (see keymap.rs). Wheel units are 1/120 of a notch.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t")]
pub enum ToClient {
    PairPending { code: String },
    Welcome { server_name: String },
    PairRejected,
    Enter { x: i32, y: i32 },
    Leave,
    Move { x: i32, y: i32 },
    Button { button: u8, down: bool },
    Wheel { dx: i32, dy: i32 },
    Key { code: u16, down: bool },
    Clipboard { text: String },
    ClipboardImage { png: String },
    Ping,
    /// A drag is being pushed off this screen: report which files it carries.
    FilesGrab,
    /// The drag was dropped on another screen: send these files to the server.
    FilesSend { paths: Vec<String> },
    /// Files copied on the server: pull them, then put them on the clipboard.
    ClipboardFiles { manifest: Manifest },
    /// Files dragged from the server and dropped here: pull them into place.
    DropOffer { manifest: Manifest },
    /// Open `count` data connections and serve transfer `id` from the outbox.
    OpenData { id: u64, count: u8 },
}
