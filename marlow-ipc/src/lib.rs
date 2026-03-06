use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

// ─── Request: agent → compositor ───

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    ListWindows,
    GetWindowInfo { window_id: u64 },
    FocusWindow { window_id: u64 },
    SendKey { window_id: u64, key: u32, pressed: bool },
    SendText { window_id: u64, text: String },
    SendClick { window_id: u64, x: f64, y: f64, button: u32 },
    SendHotkey { window_id: u64, modifiers: Vec<String>, key: String },
    RequestScreenshot { window_id: Option<u64> },
    MoveToShadow { window_id: u64 },
    MoveToUser { window_id: u64 },
    Subscribe { events: Vec<String> },
    Ping,
}

// ─── Response: compositor → agent ───

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum Response {
    #[serde(rename = "ok")]
    Ok { data: serde_json::Value },
    #[serde(rename = "error")]
    Error { message: String },
}

// ─── Event: compositor → agent (push) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    WindowCreated { window_id: u64, title: String, app_id: String },
    WindowDestroyed { window_id: u64 },
    WindowFocused { window_id: u64, title: String },
    WindowMoved { window_id: u64, x: i32, y: i32, width: i32, height: i32 },
    UserInputDetected { input_type: String },
    Pong,
}

// ─── Window info ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_id: u64,
    pub title: String,
    pub app_id: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub focused: bool,
}

// ─── Framing: u32 LE length prefix + MessagePack payload ───

/// Write a message: u32 LE length + MessagePack payload.
pub fn write_message<W: Write, T: Serialize>(stream: &mut W, msg: &T) -> io::Result<()> {
    let payload = rmp_serde::to_vec_named(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()
}

/// Read a message: u32 LE length + MessagePack payload.
pub fn read_message<R: Read, T: for<'de> Deserialize<'de>>(stream: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Message too large: {len} bytes"),
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    rmp_serde::from_slice(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_request() {
        let req = Request::Ping;
        let mut buf = Vec::new();
        write_message(&mut buf, &req).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded: Request = read_message(&mut cursor).unwrap();
        assert!(matches!(decoded, Request::Ping));
    }

    #[test]
    fn roundtrip_response() {
        let resp = Response::Ok {
            data: serde_json::json!({"windows": []}),
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded: Response = read_message(&mut cursor).unwrap();
        assert!(matches!(decoded, Response::Ok { .. }));
    }
}
