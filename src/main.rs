mod mermaid_engine;
mod render;

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

use crate::render::{
    image_renderer::{RenderedImage, ThemeMode},
    kitty::{PlacementOptions, print_image},
    markdown::{MarkdownRenderOptions, render_markdown_document, render_markdown_document_image},
    mermaid::{MermaidRenderOptions, render_mermaid_png},
};

const DEFAULT_RENDER_ZOOM: f32 = 2.0;
const DEFAULT_ZOOM_MULTIPLIER: f32 = 1.0;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Render Markdown and Mermaid directly in Kitty-compatible terminals"
)]
struct Args {
    /// The file to view (.md, .markdown, .mkd, .mmd, .mermaid). Use '-' for stdin.
    file: Option<PathBuf>,

    /// Override input type detection.
    #[arg(long = "input-type", value_enum, default_value_t = InputType::Auto)]
    input_type: InputType,

    /// Layout width in terminal columns. Defaults to current terminal width.
    #[arg(long = "width-cols")]
    width_cols: Option<u16>,

    /// Raster theme.
    #[arg(long = "theme", value_enum, default_value_t = ThemeArg::Dark)]
    theme: ThemeArg,

    /// Mermaid zoom multiplier relative to the default size. 1.5 is 150% of default, 0.5 is half default.
    #[arg(short = 'z', long = "zoom", default_value_t = DEFAULT_ZOOM_MULTIPLIER, value_parser = parse_zoom)]
    zoom: f32,

    /// Save rendered output to a PNG file instead of printing Kitty graphics.
    #[arg(short = 'o', long = "output", value_parser = parse_png_output_path)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum InputType {
    Auto,
    Markdown,
    Mermaid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ThemeArg {
    Dark,
    Light,
}

impl From<ThemeArg> for ThemeMode {
    fn from(value: ThemeArg) -> Self {
        match value {
            ThemeArg::Dark => ThemeMode::Dark,
            ThemeArg::Light => ThemeMode::Light,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileType {
    Markdown,
    Mermaid,
}

#[derive(Debug, Clone, Copy)]
enum OutputTarget<'a> {
    Terminal,
    Png(&'a Path),
}

fn detect_file_type(path: &Path) -> FileType {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match ext.as_str() {
        "mmd" | "mermaid" => FileType::Mermaid,
        _ => FileType::Markdown,
    }
}

fn resolve_input_type(input_type: InputType, path: Option<&Path>) -> FileType {
    match input_type {
        InputType::Markdown => FileType::Markdown,
        InputType::Mermaid => FileType::Mermaid,
        InputType::Auto => path.map(detect_file_type).unwrap_or(FileType::Markdown),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let width_cols = resolved_width_cols(args.width_cols);
    let theme = ThemeMode::from(args.theme);
    let zoom = render_zoom(args.zoom);
    let output = args.output;
    if let Some(path) = output.as_deref() {
        ensure_output_parent(path)?;
    }
    let output_target = output
        .as_deref()
        .map(OutputTarget::Png)
        .unwrap_or(OutputTarget::Terminal);

    match args.file {
        Some(path) if path.to_str() == Some("-") => {
            let input_type = resolve_input_type(args.input_type, None);
            let content = read_stdin()?;
            render_content(
                &content,
                None,
                input_type,
                width_cols,
                theme,
                zoom,
                output_target,
            )?;
        }
        Some(path) => {
            if !path.exists() {
                anyhow::bail!("File not found: {}", path.display());
            }
            let input_type = resolve_input_type(args.input_type, Some(&path));
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let base_dir = path.parent();
            render_content(
                &content,
                base_dir,
                input_type,
                width_cols,
                theme,
                zoom,
                output_target,
            )?;
        }
        None => {
            if !atty::is(atty::Stream::Stdin) {
                let input_type = resolve_input_type(args.input_type, None);
                let content = read_stdin()?;
                render_content(
                    &content,
                    None,
                    input_type,
                    width_cols,
                    theme,
                    zoom,
                    output_target,
                )?;
            } else {
                eprintln!(
                    "Usage: kitmd [--input-type auto|markdown|mermaid] [--width-cols N] [--theme dark|light] [--zoom RATIO] [--output FILE.png] <FILE|-"
                );
            }
        }
    }

    Ok(())
}

fn render_content(
    content: &str,
    base_dir: Option<&Path>,
    input_type: FileType,
    width_cols: u16,
    theme: ThemeMode,
    zoom: f32,
    output: OutputTarget<'_>,
) -> Result<()> {
    match output {
        OutputTarget::Terminal => {
            let mut stdout = io::stdout().lock();
            match input_type {
                FileType::Markdown => {
                    let options =
                        MarkdownRenderOptions::new(pixel_width_for_cols(width_cols), theme)
                            .with_zoom(zoom);
                    let images = render_markdown_document(content, base_dir, &options)?;
                    for image in images {
                        print_image(
                            &mut stdout,
                            image,
                            PlacementOptions::scaled_to_width(width_cols),
                        )?;
                    }
                }
                FileType::Mermaid => {
                    let image = render_mermaid_png(content, &MermaidRenderOptions { theme, zoom })?;
                    print_image(&mut stdout, image, PlacementOptions::natural_size())?;
                }
            }
            stdout.flush()?;
        }
        OutputTarget::Png(path) => match input_type {
            FileType::Markdown => {
                let options = MarkdownRenderOptions::new(pixel_width_for_cols(width_cols), theme)
                    .with_zoom(zoom);
                let image = render_markdown_document_image(content, base_dir, &options)?;
                let rendered = RenderedImage::from_rgba_owned(image)?;
                write_png_file(path, &rendered.png)?;
            }
            FileType::Mermaid => {
                let image = render_mermaid_png(content, &MermaidRenderOptions { theme, zoom })?;
                write_png_file(path, &image.png)?;
            }
        },
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .context("failed to read stdin")?;
    Ok(buffer)
}

fn resolved_width_cols(width_cols: Option<u16>) -> u16 {
    width_cols
        .or_else(|| crossterm::terminal::size().ok().map(|(cols, _)| cols))
        .unwrap_or(80)
        .max(20)
}

fn pixel_width_for_cols(cols: u16) -> u32 {
    u32::from(cols.max(20)) * 12
}

fn parse_zoom(value: &str) -> std::result::Result<f32, String> {
    let zoom: f32 = value
        .parse()
        .map_err(|_| format!("invalid zoom ratio: {value}"))?;
    if !zoom.is_finite() || zoom <= 0.0 {
        return Err("zoom must be a positive finite ratio".to_string());
    }
    if zoom > 16.0 {
        return Err("zoom must be 16 or less".to_string());
    }
    Ok(zoom)
}

fn parse_png_output_path(value: &str) -> std::result::Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
    {
        Ok(path)
    } else {
        Err("output file must have a .png extension".to_string())
    }
}

fn ensure_output_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }
        if !parent.exists() {
            anyhow::bail!(
                "output parent directory does not exist: {}",
                parent.display()
            );
        }
        if !parent.is_dir() {
            anyhow::bail!(
                "output parent path is not a directory: {}",
                parent.display()
            );
        }
    }
    Ok(())
}

fn write_png_file(path: &Path, png: &[u8]) -> Result<()> {
    ensure_output_parent(path)?;
    std::fs::write(path, png)
        .with_context(|| format!("failed to write PNG output {}", path.display()))
}

fn render_zoom(zoom_multiplier: f32) -> f32 {
    DEFAULT_RENDER_ZOOM * zoom_multiplier
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_png_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("kitmd_{name}_{}_{}.png", std::process::id(), nanos))
    }

    #[test]
    fn detects_mermaid_extensions() {
        assert_eq!(detect_file_type(Path::new("a.mmd")), FileType::Mermaid);
        assert_eq!(detect_file_type(Path::new("a.mermaid")), FileType::Mermaid);
        assert_eq!(detect_file_type(Path::new("a.md")), FileType::Markdown);
        assert_eq!(detect_file_type(Path::new("a.unknown")), FileType::Markdown);
    }

    #[test]
    fn explicit_input_type_overrides_extension() {
        assert_eq!(
            resolve_input_type(InputType::Mermaid, Some(Path::new("a.md"))),
            FileType::Mermaid
        );
        assert_eq!(
            resolve_input_type(InputType::Auto, None),
            FileType::Markdown
        );
    }

    #[test]
    fn pixel_width_scales_from_columns() {
        assert_eq!(pixel_width_for_cols(80), 960);
        assert_eq!(pixel_width_for_cols(1), 240);
    }

    #[test]
    fn parses_positive_zoom_ratio() {
        assert_eq!(parse_zoom("1.5").unwrap(), 1.5);
        assert_eq!(parse_zoom("0.5").unwrap(), 0.5);
        assert!(parse_zoom("0").is_err());
        assert!(parse_zoom("nan").is_err());
    }

    #[test]
    fn parses_png_output_path() {
        assert_eq!(
            parse_png_output_path("out.png").unwrap(),
            PathBuf::from("out.png")
        );
        assert_eq!(
            parse_png_output_path("OUT.PNG").unwrap(),
            PathBuf::from("OUT.PNG")
        );
        assert!(parse_png_output_path("out.svg").is_err());
        assert!(parse_png_output_path("out").is_err());
        assert!(parse_png_output_path("out.jpeg").is_err());
    }

    #[test]
    fn parses_short_and_long_output_args() {
        let short = Args::try_parse_from(["kitmd", "-o", "out.png", "diagram.mmd"]).unwrap();
        assert_eq!(short.output.as_deref(), Some(Path::new("out.png")));

        let long = Args::try_parse_from(["kitmd", "--output", "out.png", "diagram.mmd"]).unwrap();
        assert_eq!(long.output.as_deref(), Some(Path::new("out.png")));
    }

    #[test]
    fn cli_defaults_zoom_multiplier_to_one() {
        let args = Args::try_parse_from(["kitmd", "diagram.mmd"]).unwrap();
        assert_eq!(args.zoom, 1.0);
    }

    #[test]
    fn zoom_multiplier_is_relative_to_default_render_zoom() {
        assert_eq!(render_zoom(1.0), 2.0);
        assert_eq!(render_zoom(0.5), 1.0);
        assert_eq!(render_zoom(1.5), 3.0);
    }

    #[test]
    fn writes_mermaid_output_png_file() {
        let file = temp_png_path("mermaid");
        render_content(
            "flowchart LR\nA-->B",
            None,
            FileType::Mermaid,
            80,
            ThemeMode::Dark,
            2.0,
            OutputTarget::Png(&file),
        )
        .unwrap();

        assert!(file.exists());
        let decoded = image::open(&file).unwrap().to_rgba8();
        assert!(decoded.width() > 0);
        assert!(decoded.height() > 0);
        let _ = fs::remove_file(file);
    }

    #[test]
    fn writes_markdown_output_as_one_combined_png_file() {
        let file = temp_png_path("markdown");
        let content = (0..180)
            .map(|idx| {
                format!(
                    "## Section {idx}\n\nThis paragraph has enough text to produce a normal rendered block.\n\n"
                )
            })
            .collect::<String>();
        let width_cols = 24;
        let options = MarkdownRenderOptions::new(pixel_width_for_cols(width_cols), ThemeMode::Dark)
            .with_zoom(2.0);
        let strips = render_markdown_document(&content, None, &options).unwrap();
        assert!(strips.len() > 1);

        render_content(
            &content,
            None,
            FileType::Markdown,
            width_cols,
            ThemeMode::Dark,
            2.0,
            OutputTarget::Png(&file),
        )
        .unwrap();

        let decoded = image::open(&file).unwrap().to_rgba8();
        let expected_height = strips
            .iter()
            .map(|strip| strip.height)
            .fold(0u32, u32::saturating_add);
        assert_eq!(decoded.width(), strips[0].width);
        assert_eq!(decoded.height(), expected_height);
        let _ = fs::remove_file(file);
    }
}
