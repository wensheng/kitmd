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
    render::{
        fonts,
        image_renderer::{RenderTheme, RenderedImage, ThemeMode, rgba},
    },
};

#[derive(Debug, Clone)]
pub struct MermaidRenderOptions {
    pub theme: ThemeMode,
    pub zoom: f32,
}

static SVG_FONTDB: Lazy<Arc<Database>> = Lazy::new(|| {
    let mut database = Database::new();
    database.load_system_fonts();
    fonts::load_bundled_fonts(&mut database);
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
    use crate::mermaid_engine::{ir::NodeShape, parser::parse_mermaid};

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

    #[test]
    fn parses_v11_flowchart_datastore_shape() {
        let parsed =
            parse_mermaid("flowchart LR\nA@{ shape: datastore, label: \"Datastore\" } --> B")
                .unwrap();
        let node = parsed.graph.nodes.get("A").unwrap();
        assert_eq!(node.label, "Datastore");
        assert_eq!(node.shape, NodeShape::Cylinder);
    }

    #[test]
    fn parses_v11_sequence_decimal_autonumber() {
        let parsed = parse_mermaid("sequenceDiagram\nautonumber 1.5 0.25\nA->>B: ping").unwrap();
        let autonumber = parsed.graph.sequence_autonumber.unwrap();
        assert_eq!(autonumber.start, 1.5);
        assert_eq!(autonumber.step, 0.25);
    }

    #[test]
    fn parses_class_namespace_syntax() {
        let parsed = parse_mermaid(
            "classDiagram\nnamespace Domain {\n  class Service\n  Service : +call()\n}",
        )
        .unwrap();
        assert!(parsed.graph.nodes.contains_key("Domain.Service"));
        assert!(
            parsed.graph.nodes["Domain.Service"]
                .label
                .contains("+call()")
        );
    }

    #[test]
    fn strips_outer_quotes_from_rendered_mermaid_labels() {
        let flow = parse_mermaid("flowchart LR\nA -->|\"Edge label\"| B").unwrap();
        assert_eq!(flow.graph.edges[0].label.as_deref(), Some("Edge label"));

        let sequence =
            parse_mermaid("sequenceDiagram\nA->>B: \"quoted ping\"\nNote over A,B: 'quoted note'")
                .unwrap();
        assert_eq!(
            sequence.graph.edges[0].label.as_deref(),
            Some("quoted ping")
        );
        assert_eq!(sequence.graph.sequence_notes[0].label, "quoted note");

        let state =
            parse_mermaid("stateDiagram-v2\nstate Idle\nnote right of Idle: \"waiting\"").unwrap();
        assert_eq!(state.graph.state_notes[0].label, "waiting");
    }

    #[test]
    fn parses_quadrant_unicode_labels_without_outer_quotes() {
        let parsed = parse_mermaid(
            "quadrantChart\n  title \"增长\"\n  x-axis \"低\" --> \"高\"\n  y-axis \"慢\" --> \"快\"\n  quadrant-1 \"优先\"\n  \"活动一\" : [0.2, 0.8]",
        )
        .unwrap();
        assert_eq!(parsed.graph.quadrant.title.as_deref(), Some("增长"));
        assert_eq!(parsed.graph.quadrant.x_axis_left.as_deref(), Some("低"));
        assert_eq!(parsed.graph.quadrant.y_axis_top.as_deref(), Some("快"));
        assert_eq!(
            parsed.graph.quadrant.quadrant_labels[0].as_deref(),
            Some("优先")
        );
        assert_eq!(parsed.graph.quadrant.points[0].label, "活动一");
    }
}
