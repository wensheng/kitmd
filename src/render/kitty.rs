use std::io::Write;

use anyhow::Result;
use base64::Engine;

use crate::render::image_renderer::RenderedImage;

const KITTY_CHUNK_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, Default)]
pub struct PlacementOptions {
    pub width_cols: Option<u16>,
}

impl PlacementOptions {
    pub fn scaled_to_width(width_cols: u16) -> Self {
        Self {
            width_cols: Some(width_cols),
        }
    }

    pub fn natural_size() -> Self {
        Self { width_cols: None }
    }
}

pub fn print_image<W: Write>(
    writer: &mut W,
    image: RenderedImage,
    placement: PlacementOptions,
) -> Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(&image.png);
    let mut chunks = encoded.as_bytes().chunks(KITTY_CHUNK_BYTES).peekable();
    let mut first = true;

    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(writer, "\x1b_Ga=T,f=100,t=d,q=2")?;
            if let Some(cols) = placement.width_cols.filter(|cols| *cols > 0) {
                let rows = rows_for_columns(cols, image.width, image.height);
                write!(writer, ",c={cols},r={rows}")?;
            }
            write!(writer, ",m={more};")?;
            first = false;
        } else {
            write!(writer, "\x1b_Gq=2,m={more};")?;
        }
        writer.write_all(chunk)?;
        write!(writer, "\x1b\\")?;
    }

    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn rows_for_columns(columns: u16, width: u32, height: u32) -> u16 {
    if width == 0 || height == 0 {
        return 1;
    }
    let rows = (columns as f32 * height as f32 / width as f32 / 2.0).ceil();
    rows.clamp(1.0, u16::MAX as f32) as u16
}

#[cfg(test)]
mod tests {
    use image::RgbaImage;

    use super::*;
    use crate::render::image_renderer::{RenderedImage, rgba};

    #[test]
    fn rows_scale_from_image_aspect() {
        assert_eq!(rows_for_columns(80, 800, 400), 20);
        assert_eq!(rows_for_columns(80, 400, 800), 80);
    }

    #[test]
    fn emits_kitty_png_escape_sequence() {
        let img = RgbaImage::from_pixel(4, 4, rgba(255, 0, 0, 255));
        let rendered = RenderedImage::from_rgba(&img).unwrap();
        let mut out = Vec::new();
        print_image(&mut out, rendered, PlacementOptions::scaled_to_width(8)).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\u{1b}_Ga=T"));
        assert!(text.contains("q=2"));
        assert!(text.contains("f=100"));
        assert!(text.contains("c=8"));
    }

    #[test]
    fn natural_size_does_not_force_terminal_width() {
        let img = RgbaImage::from_pixel(4, 4, rgba(255, 0, 0, 255));
        let rendered = RenderedImage::from_rgba(&img).unwrap();
        let mut out = Vec::new();
        print_image(&mut out, rendered, PlacementOptions::natural_size()).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("q=2"));
        assert!(!text.contains(",c="));
        assert!(!text.contains(",r="));
    }

    #[test]
    fn every_chunk_suppresses_terminal_responses() {
        let rendered = RenderedImage {
            width: 32,
            height: 32,
            png: vec![7; 10_000],
        };
        let mut out = Vec::new();
        print_image(&mut out, rendered, PlacementOptions::natural_size()).unwrap();
        let text = String::from_utf8_lossy(&out);
        let chunks: Vec<&str> = text.split("\u{1b}_G").skip(1).collect();
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.contains("q=2")));
    }
}
