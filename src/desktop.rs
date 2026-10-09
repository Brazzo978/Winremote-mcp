use crate::protocol::{
    ScreenshotRequest, ScreenshotResult, MAX_SCREENSHOT_BYTES, MAX_SCREENSHOT_PIXELS,
};
use anyhow::{ensure, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn capture(request: &ScreenshotRequest) -> Result<ScreenshotResult> {
    ensure!(
        (320..=3840).contains(&request.max_width),
        "max_width must be between 320 and 3840"
    );
    #[cfg(windows)]
    {
        windows::capture(request)
    }
    #[cfg(not(windows))]
    {
        let _ = request;
        anyhow::bail!("desktop screenshots are supported only on Windows")
    }
}

fn scaled_dimensions(raw_width: u32, raw_height: u32, max_width: u32) -> Result<(u32, u32)> {
    ensure!(
        raw_width > 0 && raw_height > 0,
        "invalid virtual desktop dimensions"
    );
    ensure!(
        raw_width <= 32767 && raw_height <= 32767,
        "virtual desktop dimensions exceed GDI limits"
    );
    ensure!(
        (raw_width as u64) * (raw_height as u64) <= 256_000_000,
        "virtual desktop exceeds capture safety bound"
    );
    ensure!(
        (320..=3840).contains(&max_width),
        "max_width must be between 320 and 3840"
    );
    let mut width = raw_width.min(max_width);
    loop {
        let height = (((raw_height as u64) * (width as u64) + (raw_width as u64 / 2))
            / (raw_width as u64))
            .max(1) as u32;
        if (width as u64) * (height as u64) <= MAX_SCREENSHOT_PIXELS {
            return Ok((width, height));
        }
        width = (width * 3 / 4).max(1);
    }
}

fn bgra_to_rgb(input: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(input.len() / 4 * 3);
    for pixel in input.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    rgb
}

struct LimitedBytes(Vec<u8>);
impl std::io::Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SCREENSHOT_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("PNG exceeds 2 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut output = LimitedBytes(Vec::new());
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Balanced);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgb)?;
    writer.finish()?;
    Ok(output.0)
}

#[cfg(windows)]
pub(crate) mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        mem::{size_of, zeroed},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GetLastError, HANDLE},
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
            ReleaseDC, SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt, BITMAPINFO,
            BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
        },
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            StationsAndDesktops::{
                CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop,
                DESKTOP_READOBJECTS, HDESK, UOI_NAME,
            },
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::{
            HiDpi::{
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            WindowsAndMessaging::{
                GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN,
            },
        },
    };

    pub(crate) struct DpiGuard(DPI_AWARENESS_CONTEXT);
    impl DpiGuard {
        pub(crate) fn enter() -> Result<Self> {
            let old =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            ensure!(
                !old.is_null(),
                "per-monitor DPI awareness is unavailable; cannot capture physical desktop pixels"
            );
            Ok(Self(old))
        }
    }
    impl Drop for DpiGuard {
        fn drop(&mut self) {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
    struct Desktop(HDESK);
    impl Drop for Desktop {
        fn drop(&mut self) {
            unsafe {
                CloseDesktop(self.0);
            }
        }
    }
    struct ScreenDc(HDC);
    impl Drop for ScreenDc {
        fn drop(&mut self) {
            unsafe {
                ReleaseDC(ptr::null_mut(), self.0);
            }
        }
    }
    struct MemoryDc(HDC);
    impl Drop for MemoryDc {
        fn drop(&mut self) {
            unsafe {
                DeleteDC(self.0);
            }
        }
    }
    struct Bitmap(HBITMAP);
    impl Drop for Bitmap {
        fn drop(&mut self) {
            unsafe {
                DeleteObject(self.0);
            }
        }
    }
    struct Selection {
        dc: HDC,
        old: HGDIOBJ,
    }
    impl Drop for Selection {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.old);
            }
        }
    }

    fn desktop_name(handle: HANDLE) -> Result<String> {
        let mut needed = 0u32;
        unsafe {
            GetUserObjectInformationW(handle, UOI_NAME, ptr::null_mut(), 0, &mut needed);
        }
        ensure!(
            (2..=1024).contains(&needed) && needed.is_multiple_of(2),
            "unable to read desktop name"
        );
        let mut name = vec![0u16; needed as usize / 2];
        if unsafe {
            GetUserObjectInformationW(
                handle,
                UOI_NAME,
                name.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            anyhow::bail!("unable to read desktop name: Windows error {}", unsafe {
                GetLastError()
            });
        }
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        Ok(String::from_utf16_lossy(&name[..end]))
    }
    pub(crate) fn ensure_interactive_desktop() -> Result<()> {
        let mut session = 0u32;
        if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 {
            anyhow::bail!("cannot determine Windows session: error {}", unsafe {
                GetLastError()
            });
        }
        ensure!(
            session != 0,
            "screenshots require an interactive user session; Session 0 is unavailable"
        );
        let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
        ensure!(
            !input.is_null(),
            "input desktop unavailable; unlock the Windows session and dismiss any UAC prompt"
        );
        let input = Desktop(input);
        let input_name = desktop_name(input.0)?;
        let thread = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        ensure!(!thread.is_null(), "current thread desktop unavailable");
        let thread_name = desktop_name(thread)?;
        ensure!(input_name.eq_ignore_ascii_case("Default") && thread_name.eq_ignore_ascii_case("Default"), "screen capture requires the unlocked Default desktop; input={input_name}, thread={thread_name}");
        Ok(())
    }
    fn bitmap_rgb(
        screen: HDC,
        origin_x: i32,
        origin_y: i32,
        raw_width: u32,
        raw_height: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let dc = unsafe { CreateCompatibleDC(screen) };
        ensure!(
            !dc.is_null(),
            "CreateCompatibleDC failed: Windows error {}",
            unsafe { GetLastError() }
        );
        let dc = MemoryDc(dc);
        let mut info: BITMAPINFO = unsafe { zeroed() };
        info.bmiHeader.biSize =
            size_of::<windows_sys::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width as i32;
        info.bmiHeader.biHeight = -(height as i32);
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut c_void = ptr::null_mut();
        let bitmap = unsafe {
            CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0)
        };
        ensure!(
            !bitmap.is_null() && !bits.is_null(),
            "CreateDIBSection failed: Windows error {}",
            unsafe { GetLastError() }
        );
        let bitmap = Bitmap(bitmap);
        let old = unsafe { SelectObject(dc.0, bitmap.0) };
        ensure!(
            !old.is_null() && old as isize != -1,
            "SelectObject failed: Windows error {}",
            unsafe { GetLastError() }
        );
        let _selection = Selection { dc: dc.0, old };
        ensure!(
            unsafe { SetStretchBltMode(dc.0, HALFTONE) } != 0,
            "SetStretchBltMode failed: Windows error {}",
            unsafe { GetLastError() }
        );
        unsafe {
            SetBrushOrgEx(dc.0, 0, 0, ptr::null_mut());
        }
        let ok = unsafe {
            StretchBlt(
                dc.0,
                0,
                0,
                width as i32,
                height as i32,
                screen,
                origin_x,
                origin_y,
                raw_width as i32,
                raw_height as i32,
                SRCCOPY | CAPTUREBLT,
            )
        };
        ensure!(ok != 0, "StretchBlt failed: Windows error {}", unsafe {
            GetLastError()
        });
        ensure!(
            unsafe { GdiFlush() } != 0,
            "GdiFlush failed: Windows error {}",
            unsafe { GetLastError() }
        );
        let byte_len = (width as usize) * (height as usize) * 4;
        let bgra = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), byte_len) };
        Ok(bgra_to_rgb(bgra))
    }
    pub fn capture(request: &ScreenshotRequest) -> Result<ScreenshotResult> {
        let _dpi = DpiGuard::enter()?;
        ensure_interactive_desktop()?;
        let x = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let y = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let raw_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let raw_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        ensure!(
            raw_width > 0 && raw_height > 0,
            "virtual desktop is unavailable"
        );
        let raw_width = raw_width as u32;
        let raw_height = raw_height as u32;
        let (mut width, mut height) = scaled_dimensions(raw_width, raw_height, request.max_width)?;
        let screen = unsafe { GetDC(ptr::null_mut()) };
        ensure!(
            !screen.is_null(),
            "GetDC failed: Windows error {}",
            unsafe { GetLastError() }
        );
        let screen = ScreenDc(screen);
        let png = loop {
            let rgb = bitmap_rgb(screen.0, x, y, raw_width, raw_height, width, height)?;
            match encode_png(&rgb, width, height) {
                Ok(png) => break png,
                Err(err) if err.to_string().contains("PNG exceeds 2 MiB") && width > 1 => {
                    width = (width * 3 / 4).max(1);
                    height = (((raw_height as u64) * (width as u64) + (raw_width as u64 / 2))
                        / (raw_width as u64))
                        .max(1) as u32;
                }
                Err(err) => return Err(err),
            }
        };
        ensure_interactive_desktop()?;
        let captured_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        Ok(ScreenshotResult {
            mime_type: "image/png".into(),
            content_base64: STANDARD.encode(png),
            width,
            height,
            desktop_x: x,
            desktop_y: y,
            desktop_width: raw_width,
            desktop_height: raw_height,
            captured_unix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_preserves_aspect_ratio_and_bounds_pixels() {
        let (width, height) = scaled_dimensions(5760, 2160, 1920).unwrap();
        assert_eq!((width, height), (1920, 720));
        let (width, height) = scaled_dimensions(800, 1200, 1920).unwrap();
        assert_eq!((width, height), (800, 1200));
        let (width, height) = scaled_dimensions(1000, 32767, 3840).unwrap();
        assert!(width < 1000);
        assert!((width as u64) * (height as u64) <= MAX_SCREENSHOT_PIXELS);
        assert!(scaled_dimensions(0, 1080, 1920).is_err());
        assert!(scaled_dimensions(100, 100, 100).is_err());
    }
    #[test]
    fn bgra_channel_order_png_roundtrip() {
        let rgb = bgra_to_rgb(&[3, 2, 1, 255, 30, 20, 10, 255]);
        assert_eq!(rgb, [1, 2, 3, 10, 20, 30]);
        let png = encode_png(&rgb, 2, 1).unwrap();
        assert!(png.len() <= MAX_SCREENSHOT_BYTES);
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let mut reader = decoder.read_info().unwrap();
        let mut output = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut output).unwrap();
        assert_eq!(&output[..info.buffer_size()], &rgb);
    }
}
