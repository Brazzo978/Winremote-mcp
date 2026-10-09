use crate::protocol::{
    DesktopButton, DesktopClickRequest, DesktopInputResult, DesktopKeyRequest, DesktopPointRequest,
    DesktopScrollRequest, DesktopTypeRequest,
};
use anyhow::{bail, ensure, Result};

pub fn validate_point(_: &DesktopPointRequest) -> Result<()> {
    // Geometry is live and checked just before insertion.
    Ok(())
}
pub fn validate_click(r: &DesktopClickRequest) -> Result<()> {
    ensure!((1..=2).contains(&r.clicks), "clicks must be 1 or 2");
    Ok(())
}
pub fn validate_scroll(r: &DesktopScrollRequest) -> Result<()> {
    ensure!(
        r.vertical != 0 || r.horizontal != 0,
        "scroll must be nonzero"
    );
    ensure!(
        (-100..=100).contains(&r.vertical) && (-100..=100).contains(&r.horizontal),
        "scroll notches must be between -100 and 100"
    );
    Ok(())
}
pub fn validate_type(r: &DesktopTypeRequest) -> Result<()> {
    ensure!(
        (1..=4096).contains(&r.text.encode_utf16().count()),
        "text must contain 1 to 4096 UTF-16 code units"
    );
    ensure!(
        r.text
            .chars()
            .all(|c| c == '\r' || c == '\n' || c == '\t' || !c.is_control()),
        "text contains an unsupported control character"
    );
    ensure!(
        !r.text.replace("\r\n", "").contains('\r'),
        "bare carriage return is unsupported; use LF or CRLF"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeySpec {
    vk: u16,
    modifier: bool,
    extended: bool,
}
fn key_spec(raw: &str) -> Option<KeySpec> {
    let name = raw.trim().to_ascii_uppercase();
    let (vk, modifier, extended) = match name.as_str() {
        "CTRL" | "CONTROL" => (0x11, true, false),
        "ALT" | "OPTION" => (0x12, true, false),
        "SHIFT" => (0x10, true, false),
        "WIN" | "WINDOWS" | "META" | "CMD" => (0x5b, true, true),
        "ENTER" | "RETURN" => (0x0d, false, false),
        "TAB" => (0x09, false, false),
        "ESC" | "ESCAPE" => (0x1b, false, false),
        "SPACE" => (0x20, false, false),
        "BACKSPACE" | "BACK" => (0x08, false, false),
        "DELETE" | "DEL" => (0x2e, false, true),
        "INSERT" | "INS" => (0x2d, false, true),
        "HOME" => (0x24, false, true),
        "END" => (0x23, false, true),
        "PAGEUP" | "PGUP" => (0x21, false, true),
        "PAGEDOWN" | "PGDN" => (0x22, false, true),
        "UP" | "ARROWUP" => (0x26, false, true),
        "DOWN" | "ARROWDOWN" => (0x28, false, true),
        "LEFT" | "ARROWLEFT" => (0x25, false, true),
        "RIGHT" | "ARROWRIGHT" => (0x27, false, true),
        _ => {
            if name.len() == 1 {
                let byte = name.as_bytes()[0];
                if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
                    (byte as u16, false, false)
                } else {
                    return None;
                }
            } else {
                let n = name.strip_prefix('F')?.parse::<u8>().ok()?;
                if (1..=24).contains(&n) {
                    (0x6f + n as u16, false, false)
                } else {
                    return None;
                }
            }
        }
    };
    Some(KeySpec {
        vk,
        modifier,
        extended,
    })
}
fn key_plan(r: &DesktopKeyRequest) -> Result<Vec<KeySpec>> {
    ensure!(
        (1..=5).contains(&r.keys.len()),
        "keys must contain 1 to 5 names"
    );
    let mut plan = Vec::with_capacity(r.keys.len());
    for (i, name) in r.keys.iter().enumerate() {
        let k = key_spec(name).ok_or_else(|| anyhow::anyhow!("unsupported key: {name}"))?;
        if k.modifier {
            ensure!(
                i + 1 < r.keys.len(),
                "key chord needs one final nonmodifier key"
            );
            ensure!(
                !plan.iter().any(|p: &KeySpec| p.vk == k.vk),
                "duplicate modifier"
            );
        } else {
            ensure!(
                i + 1 == r.keys.len(),
                "key chord must end with exactly one nonmodifier key"
            );
        }
        plan.push(k);
    }
    ensure!(
        !plan.last().unwrap().modifier,
        "key chord needs one final nonmodifier key"
    );
    Ok(plan)
}
pub fn validate_key(r: &DesktopKeyRequest) -> Result<()> {
    key_plan(r).map(|_| ())
}

fn absolute_axis(pixel: i32, origin: i32, extent: i32) -> Result<i32> {
    ensure!(extent > 0, "virtual desktop has invalid dimensions");
    let relative = i64::from(pixel) - i64::from(origin);
    ensure!(
        relative >= 0 && relative < i64::from(extent),
        "point is outside the virtual desktop"
    );
    // SendInput maps 0..65535 into 65536 slots; aim at pixel centers.
    Ok((((2 * relative + 1) * 32768 / i64::from(extent)).min(65535)) as i32)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextUnit {
    Key(u16),
    Unicode(u16),
}
fn text_units(text: &str) -> Vec<TextUnit> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next();
                out.push(TextUnit::Key(13));
            }
            '\n' => out.push(TextUnit::Key(13)),
            '\t' => out.push(TextUnit::Key(9)),
            _ => out.extend(
                c.encode_utf16(&mut [0; 2])
                    .iter()
                    .copied()
                    .map(TextUnit::Unicode),
            ),
        }
    }
    out
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::desktop::windows::{ensure_interactive_desktop, DpiGuard};
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{GetLastError, POINT},
        Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
                KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
                MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
                MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
                MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
                MOUSEEVENTF_WHEEL, MOUSEINPUT,
            },
            WindowsAndMessaging::{
                GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN,
            },
        },
    };

    fn reject_held_input() -> Result<()> {
        // Held modifiers alter keystrokes; held buttons can turn movement into a drag.
        for (name, vk) in [
            ("left shift", 0xa0),
            ("right shift", 0xa1),
            ("left control", 0xa2),
            ("right control", 0xa3),
            ("left alt", 0xa4),
            ("right alt", 0xa5),
            ("left Windows", 0x5b),
            ("right Windows", 0x5c),
            ("left mouse", 1),
            ("right mouse", 2),
            ("middle mouse", 4),
            ("X mouse 1", 5),
            ("X mouse 2", 6),
        ] {
            ensure!(
                unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 == 0,
                "{name} is held; release it before desktop input"
            );
        }
        Ok(())
    }
    fn ready() -> Result<DpiGuard> {
        let dpi = DpiGuard::enter()?;
        ensure_interactive_desktop()?;
        reject_held_input()?;
        Ok(dpi)
    }
    fn mouse(flags: u32, dx: i32, dy: i32, data: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    fn keyboard(vk: u16, unicode: bool, extended: bool, up: bool) -> INPUT {
        let mut flags = 0;
        if unicode {
            flags |= KEYEVENTF_UNICODE;
        }
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: if unicode { 0 } else { vk },
                    wScan: if unicode { vk } else { 0 },
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    #[derive(Clone, Copy)]
    enum Event {
        Neutral(INPUT),
        Down(INPUT, INPUT),
        Up(INPUT),
    }
    impl Event {
        fn input(self) -> INPUT {
            match self {
                Self::Neutral(x) | Self::Down(x, _) | Self::Up(x) => x,
            }
        }
    }
    fn send(events: &[Event]) -> Result<u32> {
        ensure!(!events.is_empty(), "empty input batch");
        let _dpi = ready()?;
        for event in events {
            if let Event::Down(input, _) = event {
                if input.r#type == INPUT_KEYBOARD {
                    let key = unsafe { input.Anonymous.ki };
                    if key.dwFlags & KEYEVENTF_UNICODE == 0 {
                        ensure!(
                            unsafe { GetAsyncKeyState(i32::from(key.wVk)) } as u16 & 0x8000 == 0,
                            "requested key is already held; release it before desktop input"
                        );
                    }
                }
            }
        }
        let inputs: Vec<INPUT> = events.iter().copied().map(Event::input).collect();
        let inserted = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if inserted != inputs.len() as u32 {
            let error = unsafe { GetLastError() };
            // Release only downs acknowledged by this request, in reverse order.
            let mut pending = Vec::new();
            for event in events.iter().take(inserted as usize) {
                match *event {
                    Event::Down(_, up) => pending.push(up),
                    Event::Up(_) => {
                        pending.pop();
                    }
                    Event::Neutral(_) => {}
                }
            }
            if !pending.is_empty() && ensure_interactive_desktop().is_ok() {
                pending.reverse();
                unsafe {
                    SendInput(
                        pending.len() as u32,
                        pending.as_ptr(),
                        size_of::<INPUT>() as i32,
                    );
                }
            }
            bail!("SendInput inserted {inserted}/{} events (Windows error {error}; UIPI may also block input)", inputs.len());
        }
        Ok(inserted)
    }
    fn point_event(x: i32, y: i32) -> Result<Event> {
        let origin_x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let origin_y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        let dx = absolute_axis(x, origin_x, width)?;
        let dy = absolute_axis(y, origin_y, height)?;
        ensure!(
            !unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL) }.is_null(),
            "point falls outside a connected monitor"
        );
        Ok(Event::Neutral(mouse(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            dx,
            dy,
            0,
        )))
    }
    fn pair(vk: u16, unicode: bool, extended: bool) -> [Event; 2] {
        let down = keyboard(vk, unicode, extended, false);
        let up = keyboard(vk, unicode, extended, true);
        [Event::Down(down, up), Event::Up(up)]
    }
    pub fn move_pointer(r: &DesktopPointRequest) -> Result<DesktopInputResult> {
        validate_point(r)?;
        let _dpi = ready()?;
        Ok(DesktopInputResult {
            sent_inputs: send(&[point_event(r.x, r.y)?])?,
        })
    }
    pub fn click(r: &DesktopClickRequest) -> Result<DesktopInputResult> {
        validate_click(r)?;
        let _dpi = ready()?;
        let mut events = vec![point_event(r.x, r.y)?];
        let (down_flag, up_flag) = match r.button {
            DesktopButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            DesktopButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            DesktopButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        };
        let down = mouse(down_flag, 0, 0, 0);
        let up = mouse(up_flag, 0, 0, 0);
        for _ in 0..r.clicks {
            events.extend([Event::Down(down, up), Event::Up(up)]);
        }
        Ok(DesktopInputResult {
            sent_inputs: send(&events)?,
        })
    }
    pub fn scroll(r: &DesktopScrollRequest) -> Result<DesktopInputResult> {
        validate_scroll(r)?;
        let mut events = Vec::with_capacity(2);
        if r.vertical != 0 {
            events.push(Event::Neutral(mouse(
                MOUSEEVENTF_WHEEL,
                0,
                0,
                r.vertical * 120,
            )));
        }
        if r.horizontal != 0 {
            events.push(Event::Neutral(mouse(
                MOUSEEVENTF_HWHEEL,
                0,
                0,
                r.horizontal * 120,
            )));
        }
        Ok(DesktopInputResult {
            sent_inputs: send(&events)?,
        })
    }
    pub fn type_text(r: &DesktopTypeRequest) -> Result<DesktopInputResult> {
        type_text_checked(r, || true)
    }
    pub fn type_text_checked(
        r: &DesktopTypeRequest,
        can_continue: impl Fn() -> bool,
    ) -> Result<DesktopInputResult> {
        validate_type(r)?;
        let mut total = 0;
        for chunk in text_units(&r.text).chunks(32) {
            ensure!(can_continue(), "desktop input session expired or stopped");
            let mut events = Vec::with_capacity(chunk.len() * 2);
            for unit in chunk {
                events.extend(match *unit {
                    TextUnit::Key(vk) => pair(vk, false, false),
                    TextUnit::Unicode(unit) => pair(unit, true, false),
                });
            }
            total += send(&events)?;
        }
        Ok(DesktopInputResult { sent_inputs: total })
    }
    pub fn key(r: &DesktopKeyRequest) -> Result<DesktopInputResult> {
        let keys = key_plan(r)?;
        let mut events = Vec::with_capacity(keys.len() * 2);
        for k in &keys {
            let down = keyboard(k.vk, false, k.extended, false);
            let up = keyboard(k.vk, false, k.extended, true);
            events.push(Event::Down(down, up));
        }
        for k in keys.iter().rev() {
            events.push(Event::Up(keyboard(k.vk, false, k.extended, true)));
        }
        Ok(DesktopInputResult {
            sent_inputs: send(&events)?,
        })
    }
}
#[cfg(windows)]
pub use windows::{click, key, move_pointer, scroll, type_text, type_text_checked};
#[cfg(not(windows))]
pub fn move_pointer(r: &DesktopPointRequest) -> Result<DesktopInputResult> {
    validate_point(r)?;
    bail!("desktop input is supported only on Windows")
}
#[cfg(not(windows))]
pub fn click(r: &DesktopClickRequest) -> Result<DesktopInputResult> {
    validate_click(r)?;
    bail!("desktop input is supported only on Windows")
}
#[cfg(not(windows))]
pub fn scroll(r: &DesktopScrollRequest) -> Result<DesktopInputResult> {
    validate_scroll(r)?;
    bail!("desktop input is supported only on Windows")
}
#[cfg(not(windows))]
pub fn type_text(r: &DesktopTypeRequest) -> Result<DesktopInputResult> {
    validate_type(r)?;
    bail!("desktop input is supported only on Windows")
}
#[cfg(not(windows))]
pub fn key(r: &DesktopKeyRequest) -> Result<DesktopInputResult> {
    validate_key(r)?;
    bail!("desktop input is supported only on Windows")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coordinates_map_pixel_centers_and_negative_origins() {
        assert_eq!(absolute_axis(-1920, -1920, 1920).unwrap(), 17);
        assert_eq!(absolute_axis(-1, -1920, 1920).unwrap(), 65518);
        assert_eq!(absolute_axis(0, 0, 1).unwrap(), 32768);
        assert!(absolute_axis(-1921, -1920, 1920).is_err());
        assert!(absolute_axis(0, -1920, 1920).is_err());
        assert!(absolute_axis(0, 0, 0).is_err());
    }
    #[test]
    fn text_checks_utf16_controls_and_line_endings() {
        let r = DesktopTypeRequest {
            text: "A😀\r\n\té".into(),
        };
        validate_type(&r).unwrap();
        assert_eq!(
            text_units(&r.text),
            vec![
                TextUnit::Unicode(65),
                TextUnit::Unicode(0xd83d),
                TextUnit::Unicode(0xde00),
                TextUnit::Key(13),
                TextUnit::Key(9),
                TextUnit::Unicode(233),
            ]
        );
        for text in ["", "\0", "\u{1b}", "\r", &"😀".repeat(2049)] {
            assert!(validate_type(&DesktopTypeRequest { text: text.into() }).is_err());
        }
        assert!(validate_type(&DesktopTypeRequest {
            text: "x".repeat(4096)
        })
        .is_ok());
    }
    #[test]
    fn key_chords_and_aliases() {
        assert_eq!(
            key_plan(&DesktopKeyRequest {
                keys: vec!["control".into(), "Alt".into(), "F24".into()]
            })
            .unwrap()
            .iter()
            .map(|k| k.vk)
            .collect::<Vec<_>>(),
            vec![0x11, 0x12, 0x87]
        );
        for keys in [
            vec![],
            vec!["CTRL"],
            vec!["A", "B"],
            vec!["CTRL", "CONTROL", "A"],
            vec!["A", "SHIFT"],
            vec!["F25"],
            vec![""],
            vec!["CTRL", "ALT", "SHIFT", "WIN", "A", "B"],
        ] {
            assert!(validate_key(&DesktopKeyRequest {
                keys: keys.into_iter().map(str::to_string).collect()
            })
            .is_err());
        }
    }
    #[test]
    fn click_and_scroll_bounds() {
        for clicks in [0, 3] {
            assert!(validate_click(&DesktopClickRequest {
                x: 0,
                y: 0,
                button: DesktopButton::Left,
                clicks
            })
            .is_err());
        }
        assert!(validate_click(&DesktopClickRequest {
            x: 0,
            y: 0,
            button: DesktopButton::Middle,
            clicks: 2
        })
        .is_ok());
        for (vertical, horizontal) in [(0, 0), (101, 0), (0, -101)] {
            assert!(validate_scroll(&DesktopScrollRequest {
                vertical,
                horizontal
            })
            .is_err());
        }
        assert!(validate_scroll(&DesktopScrollRequest {
            vertical: -100,
            horizontal: 100
        })
        .is_ok());
    }
}

#[cfg(not(windows))]
pub fn type_text_checked(
    r: &DesktopTypeRequest,
    _: impl Fn() -> bool,
) -> Result<DesktopInputResult> {
    type_text(r)
}
