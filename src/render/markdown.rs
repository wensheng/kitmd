use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use comrak::{
    Arena, Options,
    nodes::{AstNode, ListType, NodeLink, NodeValue, TableAlignment},
    parse_document,
};
use image::RgbaImage;

use crate::render::{
    image_renderer::{
        Canvas, RenderTheme, RenderedImage, TextBlockOptions, TextRenderer, TextSpan, TextStyle,
        ThemeMode, resize_to_width, rgba,
    },
    mermaid::{MermaidRenderOptions, rasterize_svg, render_error_block, render_mermaid_image},
};

#[derive(Debug, Clone)]
pub struct MarkdownRenderOptions {
    pub width_px: u32,
    pub strip_height_px: u32,
    pub theme: ThemeMode,
    pub zoom: f32,
}

impl MarkdownRenderOptions {
    pub fn new(width_px: u32, theme: ThemeMode) -> Self {
        Self {
            width_px: width_px.max(240),
            strip_height_px: 1800,
            theme,
            zoom: 2.0,
        }
    }

    pub fn with_zoom(mut self, zoom: f32) -> Self {
        self.zoom = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
enum ParagraphUnit {
    Text(TextSpan),
    Image { url: String, alt: String },
}

pub fn render_markdown_document(
    input: &str,
    base_dir: Option<&Path>,
    options: &MarkdownRenderOptions,
) -> Result<Vec<RenderedImage>> {
    let (blocks, width, background, strip_height) =
        render_markdown_blocks(input, base_dir, options)?;
    blocks_to_strips(blocks, width, background, strip_height)
}

pub fn render_markdown_document_image(
    input: &str,
    base_dir: Option<&Path>,
    options: &MarkdownRenderOptions,
) -> Result<RgbaImage> {
    let (blocks, width, background, _) = render_markdown_blocks(input, base_dir, options)?;
    Ok(blocks_to_image(blocks, width, background))
}

fn render_markdown_blocks(
    input: &str,
    base_dir: Option<&Path>,
    options: &MarkdownRenderOptions,
) -> Result<(Vec<RgbaImage>, u32, image::Rgba<u8>, u32)> {
    let arena = Arena::new();
    let root = parse_document(&arena, input, &comrak_options());
    let theme = RenderTheme::for_mode(options.theme);
    let mut renderer =
        MarkdownRenderer::new(options.clone(), theme, base_dir.map(Path::to_path_buf));
    let mut blocks = Vec::new();
    for child in root.children() {
        renderer.render_block(child, &mut blocks)?;
    }
    if blocks.is_empty() {
        blocks.push(spacer(options.width_px, 1, renderer.theme.background));
    }
    Ok((
        blocks,
        options.width_px,
        renderer.theme.background,
        options.strip_height_px.max(200),
    ))
}

fn comrak_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options
}

struct MarkdownRenderer {
    options: MarkdownRenderOptions,
    theme: RenderTheme,
    base_dir: Option<PathBuf>,
    text: TextRenderer,
}

impl MarkdownRenderer {
    fn new(options: MarkdownRenderOptions, theme: RenderTheme, base_dir: Option<PathBuf>) -> Self {
        Self {
            options,
            theme,
            base_dir,
            text: TextRenderer::new(),
        }
    }

    fn content_width(&self) -> u32 {
        self.options.width_px.saturating_sub(48).max(120)
    }

    fn render_block<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        blocks: &mut Vec<RgbaImage>,
    ) -> Result<()> {
        match &node.data.borrow().value {
            NodeValue::Heading(heading) => {
                let mut units = Vec::new();
                let mut style = TextStyle {
                    bold: true,
                    ..TextStyle::default()
                };
                collect_inline_children(node, &mut style, &mut units);
                let mut spans = text_units_only(units);
                for span in &mut spans {
                    span.style.bold = true;
                }
                let font_size = match heading.level {
                    1 => 34.0,
                    2 => 28.0,
                    3 => 23.0,
                    4 => 20.0,
                    _ => 18.0,
                };
                blocks.push(self.text_block(spans, font_size, font_size * 1.28, 22, 10));
            }
            NodeValue::Paragraph => {
                let mut units = Vec::new();
                collect_inline_children(node, &mut TextStyle::default(), &mut units);
                self.render_paragraph_units(units, blocks)?;
            }
            NodeValue::CodeBlock(code) => {
                if code.info.trim().eq_ignore_ascii_case("mermaid") {
                    blocks.push(self.render_mermaid_block(&code.literal));
                } else {
                    blocks.push(self.code_block(&code.literal, code.info.trim()));
                }
            }
            NodeValue::ThematicBreak => blocks.push(self.rule_block()),
            NodeValue::Table(table) => blocks.push(self.table_block(node, &table.alignments)?),
            NodeValue::BlockQuote => blocks.push(self.blockquote_block(node)),
            NodeValue::List(list) => self.render_list(node, *list, blocks)?,
            NodeValue::HtmlBlock(html) => {
                blocks.push(self.code_block(&html.literal, "html"));
            }
            NodeValue::Document => {
                for child in node.children() {
                    self.render_block(child, blocks)?;
                }
            }
            _ => {
                for child in node.children() {
                    self.render_block(child, blocks)?;
                }
            }
        }
        Ok(())
    }

    fn render_paragraph_units(
        &mut self,
        units: Vec<ParagraphUnit>,
        blocks: &mut Vec<RgbaImage>,
    ) -> Result<()> {
        let mut pending_text = Vec::new();
        for unit in units {
            match unit {
                ParagraphUnit::Text(span) => pending_text.push(span),
                ParagraphUnit::Image { url, alt } => {
                    if !pending_text.is_empty() {
                        blocks.push(self.paragraph_block(std::mem::take(&mut pending_text)));
                    }
                    blocks.push(self.image_block(&url, &alt));
                }
            }
        }
        if !pending_text.is_empty() {
            blocks.push(self.paragraph_block(pending_text));
        }
        Ok(())
    }

    fn render_list<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        list: comrak::nodes::NodeList,
        blocks: &mut Vec<RgbaImage>,
    ) -> Result<()> {
        let mut ordinal = list.start.max(1);
        for item in node.children() {
            let marker = match list.list_type {
                ListType::Bullet => "- ".to_string(),
                ListType::Ordered => {
                    let marker = format!("{ordinal}. ");
                    ordinal += 1;
                    marker
                }
            };
            let mut spans = vec![TextSpan {
                text: marker,
                style: TextStyle {
                    bold: true,
                    ..TextStyle::default()
                },
            }];
            spans.extend(collect_rich_text(item));
            blocks.push(self.indented_text_block(spans, 16.0, 22.0, 46));
        }
        Ok(())
    }

    fn render_mermaid_block(&mut self, source: &str) -> RgbaImage {
        match render_mermaid_image(
            source,
            &MermaidRenderOptions {
                theme: self.options.theme,
                zoom: self.options.zoom,
            },
        ) {
            Ok(image) => self.framed_image_block(resize_to_width(&image, self.content_width())),
            Err(error) => {
                render_error_block(source, &error, self.options.width_px, self.options.theme)
            }
        }
    }

    fn paragraph_block(&mut self, spans: Vec<TextSpan>) -> RgbaImage {
        self.text_block(spans, 17.0, 25.0, 24, 12)
    }

    fn text_block(
        &mut self,
        spans: Vec<TextSpan>,
        font_size: f32,
        line_height: f32,
        padding_x: u32,
        padding_y: u32,
    ) -> RgbaImage {
        self.text.render_text_block(
            &normalize_spans(spans),
            &TextBlockOptions {
                width: self.options.width_px,
                padding_x,
                padding_y,
                font_size,
                line_height,
                background: self.theme.background,
                default_color: self.theme.text,
                link_color: self.theme.link,
                code_color: self.theme.code_text,
                code_background: self.theme.code_bg,
            },
        )
    }

    fn indented_text_block(
        &mut self,
        spans: Vec<TextSpan>,
        font_size: f32,
        line_height: f32,
        padding_x: u32,
    ) -> RgbaImage {
        self.text.render_text_block(
            &normalize_spans(spans),
            &TextBlockOptions {
                width: self.options.width_px,
                padding_x,
                padding_y: 4,
                font_size,
                line_height,
                background: self.theme.background,
                default_color: self.theme.text,
                link_color: self.theme.link,
                code_color: self.theme.code_text,
                code_background: self.theme.code_bg,
            },
        )
    }

    fn code_block(&mut self, literal: &str, info: &str) -> RgbaImage {
        let text = if info.is_empty() {
            literal.to_string()
        } else {
            format!("{info}\n{literal}")
        };
        let spans = vec![TextSpan {
            text,
            style: TextStyle {
                code: true,
                ..TextStyle::default()
            },
        }];
        self.text.render_text_block(
            &spans,
            &TextBlockOptions {
                width: self.options.width_px,
                padding_x: 24,
                padding_y: 14,
                font_size: 15.0,
                line_height: 22.0,
                background: self.theme.code_bg,
                default_color: self.theme.text,
                link_color: self.theme.link,
                code_color: self.theme.code_text,
                code_background: self.theme.code_bg,
            },
        )
    }

    fn rule_block(&self) -> RgbaImage {
        let mut canvas = Canvas::new(self.options.width_px, 24, self.theme.background);
        canvas.fill_rect(24, 11, self.content_width(), 2, self.theme.border);
        canvas.into_image()
    }

    fn blockquote_block<'a>(&mut self, node: &'a AstNode<'a>) -> RgbaImage {
        let spans = collect_rich_text(node);
        let mut block = self.text.render_text_block(
            &normalize_spans(spans),
            &TextBlockOptions {
                width: self.options.width_px.saturating_sub(24),
                padding_x: 18,
                padding_y: 12,
                font_size: 16.0,
                line_height: 24.0,
                background: self.theme.blockquote_bg,
                default_color: self.theme.muted_text,
                link_color: self.theme.link,
                code_color: self.theme.code_text,
                code_background: self.theme.code_bg,
            },
        );
        let mut canvas = Canvas::new(
            self.options.width_px,
            block.height() + 8,
            self.theme.background,
        );
        canvas.fill_rect(24, 4, 4, block.height(), self.theme.blockquote_bar);
        canvas.overlay(30, 4, &block);
        block = canvas.into_image();
        block
    }

    fn table_block<'a>(
        &mut self,
        node: &'a AstNode<'a>,
        alignments: &[TableAlignment],
    ) -> Result<RgbaImage> {
        let rows = table_rows(node);
        if rows.is_empty() {
            return Ok(spacer(self.options.width_px, 1, self.theme.background));
        }

        let column_count = rows
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .max(alignments.len());
        if column_count == 0 {
            return Ok(spacer(self.options.width_px, 1, self.theme.background));
        }

        let available = self.content_width().saturating_sub(column_count as u32 + 1);
        let mut widths = measured_column_widths(&rows, column_count, available);
        let total_width: u32 = widths.iter().sum::<u32>() + column_count as u32 + 1;
        if total_width > self.content_width() {
            widths = vec![available / column_count as u32; column_count];
        }

        let mut rendered_rows = Vec::new();
        let mut table_height = 1u32;
        for row in &rows {
            let mut cells = Vec::new();
            let mut row_height = 0u32;
            for col in 0..column_count {
                let spans = row.get(col).cloned().unwrap_or_default();
                let cell = self.text.render_text_block(
                    &normalize_spans(spans),
                    &TextBlockOptions {
                        width: widths[col],
                        padding_x: 8,
                        padding_y: 7,
                        font_size: 14.0,
                        line_height: 20.0,
                        background: if rendered_rows.is_empty() {
                            self.theme.table_header_bg
                        } else {
                            self.theme.background
                        },
                        default_color: self.theme.text,
                        link_color: self.theme.link,
                        code_color: self.theme.code_text,
                        code_background: self.theme.code_bg,
                    },
                );
                row_height = row_height.max(cell.height());
                cells.push(cell);
            }
            table_height += row_height + 1;
            rendered_rows.push((cells, row_height));
        }

        let table_width: u32 = widths.iter().sum::<u32>() + column_count as u32 + 1;
        let left = 24i32;
        let mut canvas = Canvas::new(
            self.options.width_px,
            table_height + 16,
            self.theme.background,
        );
        let mut y = 8i32;
        canvas.fill_rect(left, y, table_width, table_height, self.theme.background);
        canvas.stroke_rect(left, y, table_width, table_height, self.theme.border);
        y += 1;
        for (row_idx, (cells, row_height)) in rendered_rows.iter().enumerate() {
            let mut x = left + 1;
            for (col_idx, cell) in cells.iter().enumerate() {
                let cell_bg = if row_idx == 0 {
                    self.theme.table_header_bg
                } else {
                    self.theme.background
                };
                canvas.fill_rect(x, y, widths[col_idx], *row_height, cell_bg);
                canvas.overlay(x, y, cell);
                x += widths[col_idx] as i32;
                canvas.fill_rect(x, y - 1, 1, *row_height + 2, self.theme.border);
                x += 1;
            }
            y += *row_height as i32;
            canvas.fill_rect(left, y, table_width, 1, self.theme.border);
            y += 1;
        }
        Ok(canvas.into_image())
    }

    fn image_block(&mut self, url: &str, alt: &str) -> RgbaImage {
        match load_markdown_image(url, self.base_dir.as_deref(), self.options.theme) {
            Ok(image) => self.framed_image_block(resize_to_width(&image, self.content_width())),
            Err(error) => self.placeholder_block(alt, &error.to_string()),
        }
    }

    fn framed_image_block(&self, image: RgbaImage) -> RgbaImage {
        let mut canvas = Canvas::new(
            self.options.width_px,
            image.height().saturating_add(18),
            self.theme.background,
        );
        let x = 24 + (self.content_width().saturating_sub(image.width()) / 2) as i32;
        canvas.overlay(x, 9, &image);
        canvas.into_image()
    }

    fn placeholder_block(&mut self, alt: &str, message: &str) -> RgbaImage {
        let text = vec![
            TextSpan {
                text: if alt.is_empty() {
                    "Image unavailable\n".to_string()
                } else {
                    format!("Image unavailable: {alt}\n")
                },
                style: TextStyle {
                    bold: true,
                    ..TextStyle::default()
                },
            },
            TextSpan {
                text: message.to_string(),
                style: TextStyle {
                    code: true,
                    ..TextStyle::default()
                },
            },
        ];
        self.text.render_text_block(
            &text,
            &TextBlockOptions {
                width: self.options.width_px,
                padding_x: 24,
                padding_y: 12,
                font_size: 15.0,
                line_height: 21.0,
                background: self.theme.error_bg,
                default_color: self.theme.error_text,
                link_color: self.theme.link,
                code_color: self.theme.error_text,
                code_background: rgba(0, 0, 0, 20),
            },
        )
    }
}

fn collect_inline_children<'a>(
    parent: &'a AstNode<'a>,
    style: &mut TextStyle,
    units: &mut Vec<ParagraphUnit>,
) {
    for child in parent.children() {
        match &child.data.borrow().value {
            NodeValue::HtmlInline(tag) if tag.eq_ignore_ascii_case("<small>") => {
                style.scale *= 0.78;
            }
            NodeValue::HtmlInline(tag) if tag.eq_ignore_ascii_case("</small>") => {
                style.scale /= 0.78;
            }
            NodeValue::HtmlInline(tag) if tag.eq_ignore_ascii_case("<big>") => {
                style.scale *= 1.25;
            }
            NodeValue::HtmlInline(tag) if tag.eq_ignore_ascii_case("</big>") => {
                style.scale /= 1.25;
            }
            _ => collect_inline_node(child, style, units),
        }
    }
}

fn collect_inline_node<'a>(
    node: &'a AstNode<'a>,
    style: &TextStyle,
    units: &mut Vec<ParagraphUnit>,
) {
    match &node.data.borrow().value {
        NodeValue::Text(text) => units.push(ParagraphUnit::Text(TextSpan {
            text: text.clone().into_owned(),
            style: style.clone(),
        })),
        NodeValue::Code(code) => {
            let mut style = style.clone();
            style.code = true;
            units.push(ParagraphUnit::Text(TextSpan {
                text: code.literal.clone(),
                style,
            }));
        }
        NodeValue::SoftBreak => units.push(ParagraphUnit::Text(TextSpan {
            text: " ".to_string(),
            style: style.clone(),
        })),
        NodeValue::LineBreak => units.push(ParagraphUnit::Text(TextSpan {
            text: "\n".to_string(),
            style: style.clone(),
        })),
        NodeValue::Strong => {
            let mut next = style.clone();
            next.bold = true;
            collect_inline_children(node, &mut next, units);
        }
        NodeValue::Emph => {
            let mut next = style.clone();
            next.italic = true;
            collect_inline_children(node, &mut next, units);
        }
        NodeValue::Strikethrough => {
            let mut next = style.clone();
            next.strike = true;
            collect_inline_children(node, &mut next, units);
        }
        NodeValue::Link(_) => {
            let mut next = style.clone();
            next.link = true;
            collect_inline_children(node, &mut next, units);
        }
        NodeValue::Image(link) => units.push(ParagraphUnit::Image {
            url: link.url.clone(),
            alt: plain_text(node),
        }),
        NodeValue::HtmlInline(tag) => units.push(ParagraphUnit::Text(TextSpan {
            text: tag.clone(),
            style: style.clone(),
        })),
        NodeValue::TaskItem(done) => units.push(ParagraphUnit::Text(TextSpan {
            text: if done.symbol.is_some() {
                "[x] "
            } else {
                "[ ] "
            }
            .to_string(),
            style: style.clone(),
        })),
        _ => {
            let mut next = style.clone();
            collect_inline_children(node, &mut next, units);
        }
    }
}

fn collect_rich_text<'a>(node: &'a AstNode<'a>) -> Vec<TextSpan> {
    let mut units = Vec::new();
    collect_inline_children(node, &mut TextStyle::default(), &mut units);
    text_units_only(units)
}

fn text_units_only(units: Vec<ParagraphUnit>) -> Vec<TextSpan> {
    units
        .into_iter()
        .map(|unit| match unit {
            ParagraphUnit::Text(span) => span,
            ParagraphUnit::Image { alt, .. } => TextSpan {
                text: if alt.is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[image: {alt}]")
                },
                style: TextStyle {
                    italic: true,
                    ..TextStyle::default()
                },
            },
        })
        .collect()
}

fn normalize_spans(spans: Vec<TextSpan>) -> Vec<TextSpan> {
    if spans.is_empty() {
        return vec![TextSpan::plain(" ")];
    }
    spans
}

fn plain_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    plain_text_into(node, &mut out);
    out
}

fn plain_text_into<'a>(node: &'a AstNode<'a>, out: &mut String) {
    match &node.data.borrow().value {
        NodeValue::Text(text) => out.push_str(text),
        NodeValue::Code(code) => out.push_str(&code.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        NodeValue::Image(_) => {
            for child in node.children() {
                plain_text_into(child, out);
            }
        }
        _ => {
            for child in node.children() {
                plain_text_into(child, out);
            }
        }
    }
}

fn table_rows<'a>(node: &'a AstNode<'a>) -> Vec<Vec<Vec<TextSpan>>> {
    let mut rows = Vec::new();
    for row in node.children() {
        if !matches!(row.data.borrow().value, NodeValue::TableRow(_)) {
            continue;
        }
        let mut cells = Vec::new();
        for cell in row.children() {
            cells.push(collect_rich_text(cell));
        }
        rows.push(cells);
    }
    rows
}

fn measured_column_widths(rows: &[Vec<Vec<TextSpan>>], columns: usize, available: u32) -> Vec<u32> {
    let min_col = 72u32;
    let max_col = (available / columns as u32).max(min_col);
    let mut widths = vec![min_col; columns];
    for row in rows {
        for (idx, cell) in row.iter().enumerate().take(columns) {
            let text_len = cell
                .iter()
                .map(|span| span.text.chars().count())
                .sum::<usize>();
            let measured = (text_len as u32).saturating_mul(8).saturating_add(24);
            widths[idx] = widths[idx].max(measured.min(max_col));
        }
    }
    let total = widths.iter().sum::<u32>();
    if total > available {
        vec![(available / columns as u32).max(1); columns]
    } else {
        widths
    }
}

pub fn resolve_image_path(url: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Err(anyhow!("remote images are not supported"));
    }
    let path_part = url.split(['#', '?']).next().unwrap_or(url);
    let path = Path::new(path_part);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(base_dir.unwrap_or_else(|| Path::new(".")).join(path))
    }
}

pub fn load_markdown_image(
    url: &str,
    base_dir: Option<&Path>,
    theme: ThemeMode,
) -> Result<RgbaImage> {
    if url.starts_with("data:image/") {
        return load_data_uri_image(url, theme);
    }
    let path = resolve_image_path(url, base_dir)?;
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        let svg = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read SVG image {}", path.display()))?;
        return rasterize_svg(&svg, RenderTheme::for_mode(theme).background);
    }
    Ok(image::open(&path)
        .with_context(|| format!("failed to open image {}", path.display()))?
        .to_rgba8())
}

fn load_data_uri_image(url: &str, theme: ThemeMode) -> Result<RgbaImage> {
    let (meta, data) = url
        .split_once(',')
        .ok_or_else(|| anyhow!("invalid data URI image"))?;
    if !meta.contains(";base64") {
        return Err(anyhow!("only base64 data URI images are supported"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("failed to decode data URI image")?;
    if meta.starts_with("data:image/svg+xml") {
        let svg = std::str::from_utf8(&bytes).context("SVG data URI is not UTF-8")?;
        return rasterize_svg(svg, RenderTheme::for_mode(theme).background);
    }
    Ok(image::load_from_memory(&bytes)
        .context("failed to decode data URI image")?
        .to_rgba8())
}

fn spacer(width: u32, height: u32, color: image::Rgba<u8>) -> RgbaImage {
    RgbaImage::from_pixel(width.max(1), height.max(1), color)
}

fn blocks_to_strips(
    blocks: Vec<RgbaImage>,
    width: u32,
    background: image::Rgba<u8>,
    strip_height: u32,
) -> Result<Vec<RenderedImage>> {
    let mut strips = Vec::new();
    let mut current = Canvas::new(width, strip_height, background);
    let mut y = 0u32;

    for block in blocks {
        let mut offset = 0u32;
        while offset < block.height() {
            if y >= strip_height {
                strips.push(RenderedImage::from_rgba_owned(current.into_image())?);
                current = Canvas::new(width, strip_height, background);
                y = 0;
            }
            let remaining = block.height() - offset;
            let available = strip_height - y;
            let take = remaining.min(available);
            let crop = image::imageops::crop_imm(&block, 0, offset, block.width(), take).to_image();
            current.overlay(0, y as i32, &crop);
            y += take;
            offset += take;
            if y >= strip_height {
                strips.push(RenderedImage::from_rgba_owned(current.into_image())?);
                current = Canvas::new(width, strip_height, background);
                y = 0;
            }
        }
    }

    if y > 0 || strips.is_empty() {
        let image =
            image::imageops::crop_imm(&current.into_image(), 0, 0, width, y.max(1)).to_image();
        strips.push(RenderedImage::from_rgba_owned(image)?);
    }
    Ok(strips)
}

fn blocks_to_image(blocks: Vec<RgbaImage>, width: u32, background: image::Rgba<u8>) -> RgbaImage {
    let height = blocks
        .iter()
        .map(RgbaImage::height)
        .fold(0u32, u32::saturating_add)
        .max(1);
    let mut canvas = Canvas::new(width, height, background);
    let mut y = 0u32;
    for block in blocks {
        canvas.overlay(0, y as i32, &block);
        y = y.saturating_add(block.height());
    }
    canvas.into_image()
}

#[allow(dead_code)]
fn _link_url(link: &NodeLink) -> &str {
    &link.url
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn opts() -> MarkdownRenderOptions {
        MarkdownRenderOptions {
            width_px: 420,
            strip_height_px: 500,
            theme: ThemeMode::Dark,
            zoom: 2.0,
        }
    }

    #[test]
    fn renders_basic_markdown_to_image() {
        let images =
            render_markdown_document("# Title\n\nHello **world**.", None, &opts()).unwrap();
        assert!(!images.is_empty());
        assert!(images[0].width > 0);
        assert!(images[0].height > 0);
    }

    #[test]
    fn parses_small_big_and_strike_as_inline_styles() {
        let arena = Arena::new();
        let root = parse_document(
            &arena,
            "A <small>tiny</small> <big>large</big> ~~gone~~",
            &comrak_options(),
        );
        let paragraph = root.first_child().unwrap();
        let spans = collect_rich_text(paragraph);
        assert!(
            spans
                .iter()
                .any(|span| span.text == "tiny" && span.style.scale < 1.0)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "large" && span.style.scale > 1.0)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "gone" && span.style.strike)
        );
    }

    #[test]
    fn table_renders_with_pixels() {
        let images =
            render_markdown_document("| A | B |\n|---|---|\n| c | d |\n", None, &opts()).unwrap();
        let rgba = images[0].to_rgba().unwrap();
        let bg = RenderTheme::for_mode(ThemeMode::Dark).background;
        assert!(rgba.pixels().any(|pixel| *pixel != bg));
    }

    #[test]
    fn resolves_relative_image_path() {
        let path = resolve_image_path("assets/a.png", Some(Path::new("/tmp/doc"))).unwrap();
        assert_eq!(path, Path::new("/tmp/doc/assets/a.png"));
    }

    #[test]
    fn loads_base64_data_uri_image() {
        let img = RgbaImage::from_pixel(1, 1, rgba(1, 2, 3, 255));
        let rendered = RenderedImage::from_rgba(&img).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rendered.png);
        let uri = format!("data:image/png;base64,{b64}");
        let loaded = load_markdown_image(&uri, None, ThemeMode::Dark).unwrap();
        assert_eq!(loaded.dimensions(), (1, 1));
    }

    #[test]
    fn loads_local_image() {
        let dir = std::env::temp_dir().join(format!("viewmd_image_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("one.png");
        let img = RgbaImage::from_pixel(2, 3, rgba(4, 5, 6, 255));
        img.save(&file).unwrap();
        let loaded = load_markdown_image("one.png", Some(&dir), ThemeMode::Dark).unwrap();
        assert_eq!(loaded.dimensions(), (2, 3));
        let _ = fs::remove_file(file);
        let _ = fs::remove_dir(dir);
    }
}
