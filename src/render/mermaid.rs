use anyhow::{Context, Result, anyhow};
use fontdb::Database;
use image::{Rgba, RgbaImage};
use once_cell::sync::Lazy;
use resvg::{
    tiny_skia::{Pixmap, Transform},
    usvg::{Options, Tree},
};
use std::sync::Arc;

use crate::{
    mermaid_engine,
    render::image_renderer::{RenderTheme, RenderedImage, ThemeMode, rgba},
};

#[derive(Debug, Clone)]
pub struct MermaidRenderOptions {
    pub theme: ThemeMode,
    pub zoom: f32,
}

static SVG_FONTDB: Lazy<Arc<Database>> = Lazy::new(|| {
    let mut database = Database::new();
    database.load_system_fonts();
    Arc::new(database)
});

impl Default for MermaidRenderOptions {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            zoom: 2.0,
        }
    }
}

pub fn render_mermaid_png(source: &str, options: &MermaidRenderOptions) -> Result<RenderedImage> {
    let image = render_mermaid_image(source, options)?;
    RenderedImage::from_rgba_owned(image)
}

pub fn render_mermaid_image(source: &str, options: &MermaidRenderOptions) -> Result<RgbaImage> {
    let svg = render_mermaid_svg(source)?;
    let theme = RenderTheme::for_mode(options.theme);
    rasterize_svg_with_zoom(&svg, theme.background, options.zoom)
}

pub fn render_mermaid_svg(source: &str) -> Result<String> {
    let preprocessed = preprocess_mermaid(source);
    std::panic::catch_unwind(|| mermaid_engine::render(&preprocessed))
        .map_err(|_| anyhow!("Mermaid renderer panicked"))?
        .map_err(|e| anyhow!("Mermaid render error: {e}"))
}

pub fn rasterize_svg(svg_text: &str, background: Rgba<u8>) -> Result<RgbaImage> {
    rasterize_svg_with_zoom(svg_text, background, 1.0)
}

pub fn rasterize_svg_with_zoom(
    svg_text: &str,
    background: Rgba<u8>,
    zoom: f32,
) -> Result<RgbaImage> {
    let mut opt = Options::default();
    opt.fontdb = SVG_FONTDB.clone();

    let tree = Tree::from_str(svg_text, &opt).context("failed to parse SVG")?;
    let zoom = normalized_zoom(zoom);
    let size = tree.size();
    let width = (size.width() * zoom).ceil().max(1.0) as u32;
    let height = (size.height() * zoom).ceil().max(1.0) as u32;
    let mut pixmap =
        Pixmap::new(width, height).ok_or_else(|| anyhow!("failed to allocate SVG pixmap"))?;
    pixmap.fill(resvg::tiny_skia::Color::from_rgba8(
        background[0],
        background[1],
        background[2],
        background[3],
    ));
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, Transform::from_scale(zoom, zoom), &mut pixmap_mut);

    let data = pixmap.take_demultiplied();
    RgbaImage::from_raw(width, height, data)
        .ok_or_else(|| anyhow!("failed to construct SVG raster image"))
}

fn normalized_zoom(zoom: f32) -> f32 {
    if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    }
}

fn preprocess_mermaid(source: &str) -> String {
    source
        .replace("<br/>", " ")
        .replace("<br>", " ")
        .replace("<br />", " ")
}

pub fn render_error_block(
    source: &str,
    error: &anyhow::Error,
    width: u32,
    theme: ThemeMode,
) -> RgbaImage {
    use crate::render::image_renderer::{TextBlockOptions, TextRenderer, TextSpan, TextStyle};

    let colors = RenderTheme::for_mode(theme);
    let mut text = Vec::new();
    text.push(TextSpan {
        text: format!("Mermaid render failed: {error}\n\n"),
        style: TextStyle {
            bold: true,
            ..TextStyle::default()
        },
    });
    text.push(TextSpan {
        text: source.to_string(),
        style: TextStyle {
            code: true,
            ..TextStyle::default()
        },
    });
    TextRenderer::new().render_text_block(
        &text,
        &TextBlockOptions {
            width,
            padding_x: 14,
            padding_y: 12,
            font_size: 15.0,
            line_height: 21.0,
            background: colors.error_bg,
            default_color: colors.error_text,
            link_color: colors.link,
            code_color: colors.error_text,
            code_background: rgba(0, 0, 0, 20),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_flowchart_to_svg() {
        let svg = render_mermaid_svg("flowchart LR\nA-->B").unwrap();
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn renders_class_stereotype_members_to_svg() {
        let svg = render_mermaid_svg(
            "classDiagram\nclass Backend {\n    <<trait>>\n    -markdown: String\n    +run()\n}\n",
        )
        .unwrap();
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn renders_sequence_to_png() {
        let rendered = render_mermaid_png(
            "sequenceDiagram\nAlice->>Bob: Hello",
            &MermaidRenderOptions::default(),
        )
        .unwrap();
        assert!(rendered.width > 0);
        assert!(rendered.height > 0);
        assert!(!rendered.png.is_empty());
    }

    #[test]
    fn zoom_changes_rendered_png_dimensions() {
        let source = "flowchart LR\nA-->B";
        let one = render_mermaid_png(
            source,
            &MermaidRenderOptions {
                theme: ThemeMode::Dark,
                zoom: 1.0,
            },
        )
        .unwrap();
        let two = render_mermaid_png(
            source,
            &MermaidRenderOptions {
                theme: ThemeMode::Dark,
                zoom: 2.0,
            },
        )
        .unwrap();
        assert!(two.width >= one.width.saturating_mul(2).saturating_sub(1));
        assert!(two.height >= one.height.saturating_mul(2).saturating_sub(1));
    }
}
