//! Local QR encoding and bounded decoding for short pairing invitations.

use crate::pairing;
use anyhow::{bail, ensure, Result};
use image::{GenericImageView, ImageFormat, ImageReader, Limits};
use qrcode::{Color, QrCode};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Cursor, Read},
    path::Path,
};
use url::{Host, Url};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SIDE: u32 = 8192;
const MAX_PIXELS: u32 = 16_777_216;
const MAX_PAYLOAD: usize = 512;
const MAX_CANDIDATES: usize = 16;
const MODULE_PIXELS: usize = 8;
const QUIET_MODULES: usize = 4;

/// Row-major QR modules without a quiet zone. `true` means dark.
pub struct QrMatrix {
    pub width: usize,
    pub modules: Vec<bool>,
}

fn valid_short_invite(invitation: &str) -> bool {
    if invitation.is_empty()
        || invitation.len() > MAX_PAYLOAD
        || !invitation.is_ascii()
        || invitation.trim() != invitation
        || invitation
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || invitation.starts_with("wb1_")
    {
        return false;
    }
    let Ok((endpoint, _)) = pairing::parse_short_invite(invitation) else {
        return false;
    };
    let Ok(url) = Url::parse(&endpoint) else {
        return false;
    };
    if url.scheme() != "https" || url.path() != "/" {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(ip)) => !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast(),
        Some(Host::Ipv6(ip)) => !ip.is_unspecified() && !ip.is_multicast(),
        _ => false,
    }
}

pub fn matrix(invitation: &str) -> Result<QrMatrix> {
    ensure!(valid_short_invite(invitation), "Invalid short invitation");
    let code =
        QrCode::new(invitation.as_bytes()).map_err(|_| anyhow::anyhow!("QR generation failed"))?;
    let width = code.width();
    let modules = code
        .to_colors()
        .into_iter()
        .map(|color| color == Color::Dark)
        .collect();
    Ok(QrMatrix { width, modules })
}

/// Render an eight-pixel module PNG with a four-module white border.
pub fn render_png(invitation: &str) -> Result<Vec<u8>> {
    let code = matrix(invitation)?;
    let side_modules = code
        .width
        .checked_add(QUIET_MODULES * 2)
        .ok_or_else(|| anyhow::anyhow!("QR size overflow"))?;
    let side = side_modules
        .checked_mul(MODULE_PIXELS)
        .ok_or_else(|| anyhow::anyhow!("QR size overflow"))?;
    let len = side
        .checked_mul(side)
        .ok_or_else(|| anyhow::anyhow!("QR size overflow"))?;
    let mut pixels = vec![255u8; len];
    for (index, dark) in code.modules.iter().enumerate() {
        if !dark {
            continue;
        }
        let module_y = index / code.width + QUIET_MODULES;
        let module_x = index % code.width + QUIET_MODULES;
        for y in module_y * MODULE_PIXELS..(module_y + 1) * MODULE_PIXELS {
            let row = &mut pixels
                [y * side + module_x * MODULE_PIXELS..y * side + (module_x + 1) * MODULE_PIXELS];
            row.fill(0);
        }
    }
    let mut result = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut result, side as u32, side as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    Ok(result)
}

fn local_regular_file(path: &Path) -> Result<File> {
    ensure!(path.is_absolute(), "Image path must be absolute and local");
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let local_disk = matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)));
        ensure!(local_disk, "Image path must be absolute and local");
        let resolved =
            fs::canonicalize(path).map_err(|_| anyhow::anyhow!("Image file unavailable"))?;
        let resolved_disk = matches!(resolved.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
        ensure!(resolved_disk, "Image path must be absolute and local");
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("Image file unavailable"))?;
    ensure!(
        metadata.file_type().is_file(),
        "Image must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_FILE_BYTES,
        "Image file exceeds size limit"
    );
    let file = File::open(path).map_err(|_| anyhow::anyhow!("Image file unavailable"))?;
    let opened = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("Image file unavailable"))?;
    ensure!(opened.is_file(), "Image must be a regular file");
    ensure!(
        opened.len() <= MAX_FILE_BYTES,
        "Image file exceeds size limit"
    );
    Ok(file)
}

fn decode_bytes(bytes: &[u8]) -> Result<String> {
    let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageFormat::Png
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ImageFormat::Jpeg
    } else {
        bail!("Unsupported image format");
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut header = ImageReader::with_format(Cursor::new(bytes), format);
    header.limits(limits.clone());
    let (width, height) = header
        .into_dimensions()
        .map_err(|_| anyhow::anyhow!("Invalid image"))?;
    ensure!(
        width > 0 && height > 0 && width <= MAX_SIDE && height <= MAX_SIDE,
        "Image dimensions exceed limit"
    );
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| anyhow::anyhow!("Image dimensions exceed limit"))?;
    ensure!(pixels <= MAX_PIXELS, "Image dimensions exceed limit");
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| anyhow::anyhow!("Invalid image"))?;
    ensure!(
        image.dimensions() == (width, height),
        "Invalid image dimensions"
    );
    let mut grayscale = Vec::new();
    grayscale
        .try_reserve_exact(pixels as usize)
        .map_err(|_| anyhow::anyhow!("Image allocation failed"))?;
    for (_, _, color) in image.pixels() {
        let [r, g, b, a] = color.0;
        let luminance = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000;
        let composited = (luminance * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255;
        grayscale.push(composited as u8);
    }
    let mut found = HashSet::new();
    let mut candidates = 0usize;
    for invert in [false, true] {
        if invert {
            for value in &mut grayscale {
                *value = 255 - *value;
            }
        }
        let mut decoder = quircs::Quirc::default();
        for candidate in decoder.identify(width as usize, height as usize, &grayscale) {
            candidates += 1;
            ensure!(
                candidates <= MAX_CANDIDATES,
                "Image contains too many QR candidates"
            );
            let Ok(code) = candidate else {
                continue;
            };
            let Ok(data) = code.decode() else {
                continue;
            };
            if data.payload.len() > MAX_PAYLOAD {
                continue;
            }
            let Ok(text) = std::str::from_utf8(&data.payload) else {
                continue;
            };
            if valid_short_invite(text) {
                found.insert(text.to_owned());
            }
            ensure!(
                found.len() <= 1,
                "Image contains multiple distinct invitations"
            );
        }
    }
    found
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No valid invitation QR found"))
}

/// Decode one valid short invitation from a bounded local PNG or JPEG.
pub fn decode_file(path: &Path) -> Result<String> {
    let file = local_regular_file(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Image file unavailable"))?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "Image file exceeds size limit"
    );
    decode_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GrayImage, ImageBuffer, Luma, Rgba};
    use std::{fs, io::Cursor};

    const CODE: &str = "Ab7!kP2@qR9#xT4?";

    #[test]
    fn png_roundtrip_preserves_ports_ipv6_and_symbols() {
        for invitation in [
            format!("192.0.2.10:{CODE}"),
            format!("192.0.2.10:9443:{CODE}"),
            format!("[2001:db8::10]:9443:{CODE}"),
        ] {
            let png = render_png(&invitation).unwrap();
            assert_eq!(decode_bytes(&png).unwrap(), invitation);
        }
    }

    #[test]
    fn rejects_invalid_payload_and_file_format() {
        assert!(matrix(&format!("0.0.0.0:{CODE}")).is_err());
        assert!(matrix(&format!("192.0.2.10:{CODE}\n")).is_err());
        assert!(matrix("wb1_fake").is_err());
        assert!(decode_bytes(b"not an image").is_err());
        let mut truncated = render_png(&format!("192.0.2.10:{CODE}")).unwrap();
        truncated.truncate(32);
        assert!(decode_bytes(&truncated).is_err());
    }

    fn qr_image(invitation: &str) -> GrayImage {
        image::load_from_memory_with_format(&render_png(invitation).unwrap(), ImageFormat::Png)
            .unwrap()
            .into_luma8()
    }

    fn encoded(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn finds_code_inside_screenshot_and_rotated_jpeg() {
        let invitation = format!("[2001:db8::10]:9443:{CODE}");
        let qr = qr_image(&invitation);
        let mut screenshot =
            GrayImage::from_pixel(qr.width() + 160, qr.height() + 140, Luma([235]));
        image::imageops::replace(&mut screenshot, &qr, 80, 80);
        // A dark window border and title bar should not become QR candidates.
        for x in 0..screenshot.width() {
            screenshot.put_pixel(x, 0, Luma([20]));
        }
        let screenshot_png = encoded(
            DynamicImage::ImageLuma8(screenshot.clone()),
            ImageFormat::Png,
        );
        assert_eq!(decode_bytes(&screenshot_png).unwrap(), invitation);
        let rotated = image::imageops::rotate90(&screenshot);
        let jpeg = encoded(DynamicImage::ImageLuma8(rotated), ImageFormat::Jpeg);
        assert_eq!(decode_bytes(&jpeg).unwrap(), invitation);
    }

    #[test]
    fn reads_inverted_qr_and_composites_transparency_on_white() {
        let invitation = format!("192.0.2.10:{CODE}");
        let mut inverted = qr_image(&invitation);
        image::imageops::invert(&mut inverted);
        let png = encoded(DynamicImage::ImageLuma8(inverted), ImageFormat::Png);
        assert_eq!(decode_bytes(&png).unwrap(), invitation);

        let gray = qr_image(&invitation);
        let rgba = ImageBuffer::from_fn(gray.width(), gray.height(), |x, y| {
            if gray.get_pixel(x, y).0[0] == 0 {
                Rgba([0u8, 0, 0, 255])
            } else {
                Rgba([0u8, 0, 0, 0])
            }
        });
        let png = encoded(DynamicImage::ImageRgba8(rgba), ImageFormat::Png);
        assert_eq!(decode_bytes(&png).unwrap(), invitation);
    }

    #[test]
    fn ignores_unrelated_codes_and_rejects_two_invitations() {
        let first = format!("192.0.2.10:{CODE}");
        let second = format!("192.0.2.11:{CODE}");
        let qr1 = qr_image(&first);
        let qr2 = qr_image(&second);
        let unrelated = image::load_from_memory_with_format(
            &{
                let code = QrCode::new(b"https://example.invalid/irrelevant").unwrap();
                let width = code.width();
                let side = (width + 8) * 8;
                let mut pixels = GrayImage::from_pixel(side as u32, side as u32, Luma([255]));
                for (i, c) in code.to_colors().iter().enumerate() {
                    if *c == Color::Dark {
                        for y in 0..8 {
                            for x in 0..8 {
                                pixels.put_pixel(
                                    ((i % width + 4) * 8 + x) as u32,
                                    ((i / width + 4) * 8 + y) as u32,
                                    Luma([0]),
                                );
                            }
                        }
                    }
                }
                encoded(DynamicImage::ImageLuma8(pixels), ImageFormat::Png)
            },
            ImageFormat::Png,
        )
        .unwrap()
        .into_luma8();
        let height = qr1.height().max(qr2.height()).max(unrelated.height()) + 80;
        let mut canvas = GrayImage::from_pixel(
            qr1.width() + qr2.width() + unrelated.width() + 160,
            height,
            Luma([255]),
        );
        image::imageops::replace(&mut canvas, &qr1, 40, 40);
        image::imageops::replace(&mut canvas, &unrelated, (qr1.width() + 80) as i64, 40);
        let one = encoded(DynamicImage::ImageLuma8(canvas.clone()), ImageFormat::Png);
        assert_eq!(decode_bytes(&one).unwrap(), first);
        image::imageops::replace(
            &mut canvas,
            &qr2,
            (qr1.width() + unrelated.width() + 120) as i64,
            40,
        );
        let two = encoded(DynamicImage::ImageLuma8(canvas), ImageFormat::Png);
        assert!(decode_bytes(&two)
            .unwrap_err()
            .to_string()
            .contains("multiple distinct"));
    }

    #[test]
    fn file_entrypoint_rejects_relative_and_oversize_files() {
        assert!(decode_file(Path::new("relative.png")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("invite.data");
        let invitation = format!("192.0.2.10:{CODE}");
        fs::write(&valid, render_png(&invitation).unwrap()).unwrap();
        assert_eq!(decode_file(&valid).unwrap(), invitation);
        let path = dir.path().join("large.png");
        fs::write(&path, vec![0; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(decode_file(&path)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
    }
}

#[cfg(test)]
mod default_https_port_tests {
    #[test]
    fn qr_accepts_explicit_https_default_port() {
        let invitation = "192.0.2.10:443:Ab7!kP2@qR9#xT4?";
        let png = super::render_png(invitation).unwrap();
        assert_eq!(super::decode_bytes(&png).unwrap(), invitation);
    }
}
