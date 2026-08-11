//! 终端 QR 渲染 + 可选 PNG 保存。
//!
//! 对标 `cc-connect/cmd/cc-connect/feishu.go:685-709`（`tryPrintTerminalQRCode` +
//! `saveQRCodeImage`）。用 `qrcode` crate 生成 modules，自己拼 ANSI 字符串。

use std::path::Path;

use qrcode::{Color, EcLevel, QrCode};

use super::error::SetupError;

/// QR 内容渲染抽象。生产用 `AnsiQrRenderer`；测试可注入 fake。
pub trait QrRenderer: Send + Sync {
    /// 把 `content` 渲染到终端 stdout。
    fn render_terminal(&self, content: &str) -> Result<(), SetupError>;

    /// 把 `content` 渲染成 PNG 存到 `path`。
    fn render_png(&self, content: &str, path: &Path) -> Result<(), SetupError>;
}

/// 生产实现：用 `qrcode` crate 解码 + 自己拼 ANSI 字符串。
#[derive(Debug, Clone, Default)]
pub struct AnsiQrRenderer;

impl QrRenderer for AnsiQrRenderer {
    fn render_terminal(&self, content: &str) -> Result<(), SetupError> {
        let qr = build_qr(content)?;
        let width = qr.width();
        let modules = qr.to_colors();
        let quiet = 4;

        let black = "██";
        let white = "  ";

        let mut out = String::new();
        out.push('\n');
        for _ in 0..(quiet * 2 + width) {
            out.push_str(white);
        }
        out.push('\n');

        for y in 0..width {
            for _ in 0..quiet {
                out.push_str(white);
            }
            for x in 0..width {
                if modules[y * width + x] == Color::Dark {
                    out.push_str(black);
                } else {
                    out.push_str(white);
                }
            }
            for _ in 0..quiet {
                out.push_str(white);
            }
            out.push('\n');
        }

        for _ in 0..(quiet * 2 + width) {
            out.push_str(white);
        }
        out.push('\n');

        print!("{out}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        Ok(())
    }

    fn render_png(&self, content: &str, path: &Path) -> Result<(), SetupError> {
        let qr = build_qr(content)?;
        write_minimal_png(&qr, path)?;
        Ok(())
    }
}

fn build_qr(content: &str) -> Result<QrCode, SetupError> {
    QrCode::with_error_correction_level(content, EcLevel::M)
        .map_err(|e| SetupError::Http(format!("encode QR: {e}")))
}

/// 写一个 1-bit PNG（每像素 1 模块，黑白）。无外部 image 依赖。
/// 模块 true（暗）→ 像素 0；false（亮）→ 像素 255。
fn write_minimal_png(qr: &QrCode, path: &Path) -> Result<(), SetupError> {
    use std::io::Write;

    let width = qr.width() as u32;
    let modules = qr.to_colors();
    let quiet = 4u32;
    let img_w = width + quiet * 2;
    let scale = 8u32;
    let png_w = img_w * scale;
    let png_h = png_w;

    let mut pixels: Vec<u8> = Vec::with_capacity((png_w * png_h) as usize);
    for py in 0..png_h {
        let gy = py / scale;
        let module_y = gy.saturating_sub(quiet) as usize;
        for px in 0..png_w {
            let gx = px / scale;
            let module_x = gx.saturating_sub(quiet) as usize;
            let bright = if (module_x as u32) < width && (module_y as u32) < width {
                let dark = modules[module_y * width as usize + module_x] == Color::Dark;
                if dark {
                    0u8
                } else {
                    255u8
                }
            } else {
                255u8
            };
            pixels.push(bright);
            pixels.push(bright);
            pixels.push(bright);
        }
    }

    let raw = rgb_to_png_bytes(&pixels, png_w, png_h);
    let mut file = std::fs::File::create(path)
        .map_err(|e| SetupError::WriteConfig(format!("create png file: {e}")))?;
    file.write_all(&raw)
        .map_err(|e| SetupError::WriteConfig(format!("write png: {e}")))?;
    Ok(())
}

/// 最简 RGB PNG encoder：用 uncompressed DEFLATE（stored blocks）规避 flate2 依赖。
/// PNG 头部 + IHDR + IDAT（zlib stored）+ IEND。
fn rgb_to_png_bytes(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature

    // IHDR
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(2); // color type = RGB
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    append_chunk(&mut out, *b"IHDR", &ihdr);

    // IDAT: zlib-wrapped uncompressed DEFLATE
    let row_byte_count = (width as usize) * 3;
    let mut raw = Vec::with_capacity((row_byte_count + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0u8); // filter type 0 (None)
        let row_start = y * row_byte_count;
        let row_end = row_start + row_byte_count;
        raw.extend_from_slice(&rgb[row_start..row_end]);
    }

    let zlib = zlib_stored_deflate(&raw);
    append_chunk(&mut out, *b"IDAT", &zlib);

    append_chunk(&mut out, *b"IEND", &[]);
    out
}

fn append_chunk(out: &mut Vec<u8>, chunk_type: [u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&chunk_type);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(&chunk_type);
    crc_input.extend_from_slice(data);
    let crc = crc32(&crc_input);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFFFFFF
}

fn zlib_stored_deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF
    out.push(0x01); // FLG (no preset dictionary, compression level 0)

    // DEFLATE stored blocks
    let mut i = 0;
    while i < data.len() {
        let remaining = data.len() - i;
        let block_len = remaining.min(0xFFFF);
        let is_last = if i + block_len >= data.len() {
            1u8
        } else {
            0u8
        };
        out.push(is_last); // BFINAL + BTYPE=00 (stored)
        out.push((block_len & 0xFF) as u8);
        out.push(((block_len >> 8) & 0xFF) as u8);
        let nlen = !block_len as u16;
        out.push((nlen & 0xFF) as u8);
        out.push(((nlen >> 8) & 0xFF) as u8);
        out.extend_from_slice(&data[i..i + block_len]);
        i += block_len;
    }

    // Adler-32 checksum
    let adler = adler32(data);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    const MOD: u32 = 65521;
    for byte in data {
        a = (a + *byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn renderer_outputs_some_black_modules_for_short_url() {
        let renderer = AnsiQrRenderer;
        let url = "https://example.com/qr?dc=abc";
        let qr = build_qr(url).unwrap();
        let width = qr.width();
        assert!(width > 0);

        let modules = qr.to_colors();
        let mut dark_count = 0;
        for v in &modules {
            if *v == Color::Dark {
                dark_count += 1;
            }
        }
        assert!(dark_count > 0);

        // render_terminal 跑通不 panic
        let _ = renderer.render_terminal(url);
    }

    #[test]
    fn png_writer_produces_valid_signature() {
        let renderer = AnsiQrRenderer;
        let dir = tempdir().unwrap();
        let path = dir.path().join("qr.png");

        renderer
            .render_png("https://example.com/qr?dc=abc", &path)
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        // PNG signature
        assert_eq!(&bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        // IHDR chunk type at offset 12-16
        assert_eq!(&bytes[12..16], b"IHDR");
        // IEND chunk at the end
        assert!(bytes.windows(4).any(|w| w == b"IEND"));
    }

    #[test]
    fn crc32_known_value() {
        // RFC 2086 reference: crc32("123456789") == 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn adler32_known_value() {
        // RFC 1950 reference: adler32("Wikipedia") == 0x11E60398
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
    }
}
