use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::ValueEnum;
use plotters::coord::ranged1d::{AsRangedCoord, ValueFormatter};
use plotters::coord::Shift;
use plotters::prelude::*;
use plotters::style::text_anchor::{HPos, Pos, VPos};

use crate::exporter::{write_table, ExportFormat};

/// Y-axis scale of a statistics plot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scale {
    Linear,
    Log,
}

impl Scale {
    fn label(self) -> &'static str {
        match self {
            Scale::Linear => "linear",
            Scale::Log => "log",
        }
    }
}

/// Y-axis scale of each of the three stacked plots.
#[derive(Debug, Clone, Copy)]
pub struct ChartScales {
    pub ratio: Scale,
    pub words: Scale,
    pub characters: Scale,
}

#[derive(Debug, Clone)]
pub struct ChapterStat {
    pub chapter_index: usize,
    pub title: String,
    pub filtered_words: usize,
    pub characters: usize,
    pub raw_ratio: f64,
    pub normalized_ratio: f64,
}

impl ChapterStat {
    /// `baseline_ratio` is the raw ratio of the reference chapter (prologue, or chapter 1).
    pub fn new(chapter_index: usize, title: &str, filtered_words: usize, characters: usize, baseline_ratio: f64) -> Self {
        let raw_ratio = vocabulary_ratio(filtered_words, characters);
        let normalized_ratio = if baseline_ratio > 0.0 { raw_ratio / baseline_ratio } else { 0.0 };
        Self {
            chapter_index,
            title: title.to_string(),
            filtered_words,
            characters,
            raw_ratio,
            normalized_ratio,
        }
    }
}

/// Filtered vocabulary words per character.
pub fn vocabulary_ratio(filtered_words: usize, characters: usize) -> f64 {
    if characters == 0 {
        0.0
    } else {
        filtered_words as f64 / characters as f64
    }
}

pub struct StatisticsExportPaths {
    pub table_path: PathBuf,
    pub svg_path: PathBuf,
}

/// Export the statistics table (TSV/CSV) and the SVG chart into `<output_dir>/statistics/`.
pub fn export_statistics(
    output_dir: &Path,
    stats: &[ChapterStat],
    format: ExportFormat,
    scales: ChartScales,
) -> Result<StatisticsExportPaths> {
    let stats_dir = output_dir.join("statistics");
    fs::create_dir_all(&stats_dir)
        .with_context(|| format!("Failed to create statistics directory: {:?}", stats_dir))?;

    let table_path = stats_dir.join(format!("vocabulary_ratio.{}", format.extension()));
    let svg_path = stats_dir.join("vocabulary_ratio.svg");

    let header = ["chapter", "title", "filtered_words", "characters", "raw_ratio", "normalized_ratio"];
    let rows: Vec<Vec<String>> = std::iter::once(header.iter().map(|h| h.to_string()).collect())
        .chain(stats.iter().map(|s| {
            vec![
                s.chapter_index.to_string(),
                s.title.clone(),
                s.filtered_words.to_string(),
                s.characters.to_string(),
                format!("{:.6}", s.raw_ratio),
                format!("{:.6}", s.normalized_ratio),
            ]
        }))
        .collect();
    write_table(&table_path, &rows, format)?;

    render_chart(&svg_path, stats, scales)
        .with_context(|| format!("Failed to write SVG chart: {:?}", svg_path))?;

    Ok(StatisticsExportPaths { table_path, svg_path })
}

/// One stacked plot of the chart.
struct Panel<'a> {
    caption: &'a str,
    y_desc: &'a str,
    scale: Scale,
    color: RGBColor,
    points: Vec<(f64, f64)>,
    /// Horizontal dashed reference line.
    baseline: Option<f64>,
    y_format: fn(&f64) -> String,
}

impl Panel<'_> {
    fn is_visible(&self, y: f64) -> bool {
        self.scale == Scale::Linear || y > 0.0
    }

    /// "min … · avg … · max …" over the panel's values (which must not be empty).
    fn summary(&self) -> String {
        let ys = || self.points.iter().map(|&(_, y)| y);
        let min = ys().fold(f64::INFINITY, f64::min);
        let max = ys().fold(f64::NEG_INFINITY, f64::max);
        let avg = ys().sum::<f64>() / self.points.len() as f64;
        let f = self.y_format;
        format!("min {} · avg {} · max {}", f(&min), f(&avg), f(&max))
    }
}

/// Pixel geometry of a drawn panel, used to place the hover overlay.
struct PanelLayout {
    plot_x: Range<i32>,
    plot_y: Range<i32>,
    /// Per chapter: x pixel, and y pixel when the point is drawn.
    points: Vec<(i32, Option<i32>)>,
}

const WIDTH: u32 = 1200;
const HEADER_HEIGHT: u32 = 100;
const PANEL_HEIGHT: u32 = 340;

const BLUE: RGBColor = RGBColor(37, 99, 235);
const TEAL: RGBColor = RGBColor(13, 148, 136);
const PURPLE: RGBColor = RGBColor(124, 58, 237);
const AMBER: RGBColor = RGBColor(217, 119, 6);
const SLATE: RGBColor = RGBColor(71, 85, 105);

/// Render a summary header and three stacked plots sharing the chapter axis:
/// normalized vocabulary ratio (top), filtered word count, and character count (bottom).
/// Hovering a chapter highlights its points and shows its numbers as a tooltip.
pub fn render_chart(path: &Path, stats: &[ChapterStat], scales: ChartScales) -> Result<()> {
    let mut svg = String::new();
    let overlay = draw_chart(&mut svg, stats, scales)?;
    let end = svg.rfind("</svg>").context("plotters output has no closing </svg> tag")?;
    svg.insert_str(end, &overlay);
    fs::write(path, svg)?;
    Ok(())
}

/// Draw the chart into `svg` with plotters and return the hover overlay to append to it.
fn draw_chart(svg: &mut String, stats: &[ChapterStat], scales: ChartScales) -> Result<String> {
    let series = |f: fn(&ChapterStat) -> f64| -> Vec<(f64, f64)> {
        stats.iter().map(|s| (s.chapter_index as f64, f(s))).collect()
    };
    let panels = [
        Panel {
            caption: "New vocabulary density: filtered words per character, relative to the first chapter",
            y_desc: "Normalized ratio",
            scale: scales.ratio,
            color: BLUE,
            points: series(|s| s.normalized_ratio),
            baseline: Some(1.0),
            y_format: format_ratio,
        },
        Panel {
            caption: "Filtered words exported",
            y_desc: "Words",
            scale: scales.words,
            color: TEAL,
            points: series(|s| s.filtered_words as f64),
            baseline: None,
            y_format: format_count,
        },
        Panel {
            caption: "Chapter length",
            y_desc: "Characters",
            scale: scales.characters,
            color: PURPLE,
            points: series(|s| s.characters as f64),
            baseline: None,
            y_format: format_count,
        },
    ];

    let height = HEADER_HEIGHT + PANEL_HEIGHT * panels.len() as u32;
    let root = SVGBackend::with_string(svg, (WIDTH, height)).into_drawing_area();
    root.fill(&WHITE)?;
    let (header, body) = root.split_vertically(HEADER_HEIGHT);

    // Header: title and per-plot summary statistics
    let centered = |size: f64, style: FontStyle, color: RGBColor| {
        ("sans-serif", size, style)
            .into_font()
            .color(&color)
            .pos(Pos::new(HPos::Center, VPos::Top))
    };
    let title = format!("Vocabulary statistics per chapter · {} chapters", stats.len());
    header.draw(&Text::new(title, (WIDTH as i32 / 2, 14), centered(26.0, FontStyle::Bold, BLACK)))?;
    if !stats.is_empty() {
        for (i, panel) in panels.iter().enumerate() {
            let x = (WIDTH as usize * (2 * i + 1) / (2 * panels.len())) as i32;
            header.draw(&Text::new(panel.y_desc, (x, 54), centered(15.0, FontStyle::Bold, panel.color)))?;
            header.draw(&Text::new(panel.summary(), (x, 75), centered(14.0, FontStyle::Normal, SLATE)))?;
        }
    }

    let (Some(first), Some(last)) = (stats.first(), stats.last()) else {
        root.present()?;
        return Ok(String::new());
    };
    let x_range = first.chapter_index as f64 - 0.5..last.chapter_index as f64 + 0.5;

    let areas = body.split_evenly((panels.len(), 1));
    let mut layouts = Vec::with_capacity(panels.len());
    for (i, (panel, area)) in panels.iter().zip(&areas).enumerate() {
        let is_bottom = i == panels.len() - 1;
        let (lo, hi) = y_bounds(panel);
        layouts.push(match panel.scale {
            Scale::Linear => draw_panel(area, x_range.clone(), lo..hi, lo, panel, is_bottom)?,
            Scale::Log => {
                let y_range = (lo..hi).log_scale().with_key_points(log_ticks(lo, hi));
                draw_panel(area, x_range.clone(), y_range, lo, panel, is_bottom)?
            }
        });
    }

    root.present()?;
    Ok(hover_overlay(stats, &panels, &layouts))
}

/// Draw one panel; `y_anchor` is any in-range y value, used to locate undrawn points' x pixel.
fn draw_panel<Y>(
    area: &DrawingArea<SVGBackend, Shift>,
    x_range: Range<f64>,
    y_range: Y,
    y_anchor: f64,
    panel: &Panel,
    is_bottom: bool,
) -> Result<PanelLayout>
where
    Y: AsRangedCoord<Value = f64>,
    Y::CoordDescType: ValueFormatter<f64>,
{
    let mut chart = ChartBuilder::on(area)
        .margin_left(10)
        .margin_right(30)
        .margin_top(5)
        .caption(format!("{} · {} scale", panel.caption, panel.scale.label()), ("sans-serif", 17))
        .x_label_area_size(if is_bottom { 45 } else { 20 })
        .y_label_area_size(85)
        .build_cartesian_2d(x_range.clone(), y_range)?;

    let x_format = |x: &f64| format!("{x:.0}");
    let mut mesh = chart.configure_mesh();
    mesh.y_desc(panel.y_desc)
        .y_label_formatter(&panel.y_format)
        .x_label_formatter(&x_format)
        .axis_desc_style(("sans-serif", 15))
        .label_style(("sans-serif", 13))
        // Log axes use explicit key points; leave room for all of them.
        .y_labels(if panel.scale == Scale::Log { 20 } else { 8 })
        .light_line_style(TRANSPARENT)
        .bold_line_style(BLACK.mix(0.08));
    if is_bottom {
        mesh.x_desc("Chapter");
    }
    mesh.draw()?;

    if let Some(y) = panel.baseline {
        chart.draw_series(DashedLineSeries::new(
            [(x_range.start, y), (x_range.end, y)],
            6,
            4,
            AMBER.stroke_width(2),
        ))?;
    }

    // Non-positive values cannot be shown on a log axis.
    let visible = panel.points.iter().copied().filter(|&(_, y)| panel.is_visible(y));
    chart.draw_series(
        LineSeries::new(visible, panel.color.stroke_width(2)).point_size(if panel.points.len() <= 60 { 3 } else { 2 }),
    )?;

    let (plot_x, plot_y) = chart.plotting_area().get_pixel_range();
    let points = panel
        .points
        .iter()
        .map(|&(x, y)| {
            let shown = panel.is_visible(y);
            let (px, py) = chart.backend_coord(&(x, if shown { y } else { y_anchor }));
            (px, shown.then_some(py))
        })
        .collect();
    Ok(PanelLayout { plot_x, plot_y, points })
}

/// One full-height hover band per chapter, highlighting its points and carrying a `<title>` tooltip.
fn hover_overlay(stats: &[ChapterStat], panels: &[Panel], layouts: &[PanelLayout]) -> String {
    let (Some(top), Some(bottom)) = (layouts.first(), layouts.last()) else {
        return String::new();
    };
    let centers: Vec<i32> = top.points.iter().map(|&(x, _)| x).collect();
    let (y0, y1) = (top.plot_y.start, bottom.plot_y.end);

    let mut svg = String::from(
        "<style>\
         .hover rect { fill: #0f172a; fill-opacity: 0; }\
         .hover:hover rect { fill-opacity: 0.06; }\
         .hover circle { visibility: hidden; }\
         .hover:hover circle { visibility: visible; }\
         </style>\n",
    );
    for (i, s) in stats.iter().enumerate() {
        let left = if i == 0 { top.plot_x.start } else { (centers[i - 1] + centers[i]) / 2 };
        let right = centers.get(i + 1).map_or(top.plot_x.end, |&next| (centers[i] + next) / 2);
        let tooltip = format!(
            "Chapter {}: {}\nNormalized ratio: {:.4}\nRaw ratio: {:.6}\nWords: {}\nCharacters: {}",
            s.chapter_index,
            s.title,
            s.normalized_ratio,
            s.raw_ratio,
            format_count(&(s.filtered_words as f64)),
            format_count(&(s.characters as f64)),
        );
        svg.push_str(&format!(
            "<g class=\"hover\"><title>{}</title><rect x=\"{left}\" y=\"{y0}\" width=\"{}\" height=\"{}\"/>",
            xml_escape(&tooltip),
            right - left,
            y1 - y0,
        ));
        for (panel, layout) in panels.iter().zip(layouts) {
            if let (x, Some(y)) = layout.points[i] {
                let RGBColor(r, g, b) = panel.color;
                svg.push_str(&format!(
                    "<circle cx=\"{x}\" cy=\"{y}\" r=\"5\" fill=\"white\" stroke=\"#{r:02x}{g:02x}{b:02x}\" stroke-width=\"2.5\"/>"
                ));
            }
        }
        svg.push_str("</g>\n");
    }
    svg
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Padded y-range covering the panel's values (and its baseline, if any).
fn y_bounds(panel: &Panel) -> (f64, f64) {
    let values = panel.points.iter().map(|&(_, y)| y).chain(panel.baseline);
    match panel.scale {
        Scale::Linear => {
            let max = values.fold(0.0, f64::max);
            (0.0, if max > 0.0 { max * 1.05 } else { 1.0 })
        }
        Scale::Log => {
            let (min, max) = values
                .filter(|&y| y > 0.0)
                .fold((f64::INFINITY, 0.0f64), |(lo, hi), y| (lo.min(y), hi.max(y)));
            if min.is_finite() {
                (min / 1.15, max * 1.15)
            } else {
                (1.0, 10.0)
            }
        }
    }
}

/// 1-2-5 ticks inside `lo..=hi`, or 1..9 when the range is too narrow for that.
fn log_ticks(lo: f64, hi: f64) -> Vec<f64> {
    let ticks = |mantissas: &[f64]| -> Vec<f64> {
        (lo.log10().floor() as i32..=hi.log10().ceil() as i32)
            .flat_map(|e| mantissas.iter().map(move |m| m * 10f64.powi(e)))
            .filter(|v| (lo..=hi).contains(v))
            .collect()
    };
    let ticks_125 = ticks(&[1.0, 2.0, 5.0]);
    if ticks_125.len() >= 3 {
        ticks_125
    } else {
        ticks(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])
    }
}

/// Ratio with three significant digits and no trailing zeros (1, 0.5, 0.001).
fn format_ratio(v: &f64) -> String {
    if *v <= 0.0 {
        return "0".to_string();
    }
    let decimals = (2 - v.log10().floor() as i32).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Integer with thousands separators (axis labels are always non-negative counts).
fn format_count(v: &f64) -> String {
    let digits = format!("{:.0}", v.max(0.0));
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ChapterStat> {
        let baseline = vocabulary_ratio(500, 5000);
        vec![
            ChapterStat::new(0, "プロローグ <Intro>", 500, 5000, baseline),
            ChapterStat::new(1, "第一話 & 冒険", 300, 5000, baseline),
            ChapterStat::new(2, "第二話", 0, 12000, baseline),
        ]
    }

    #[test]
    fn normalizes_against_baseline() {
        let stats = sample();
        assert!((stats[0].raw_ratio - 0.10).abs() < 1e-9);
        assert!((stats[0].normalized_ratio - 1.0).abs() < 1e-9);
        assert!((stats[1].normalized_ratio - 0.60).abs() < 1e-9);
        assert_eq!(stats[2].normalized_ratio, 0.0);
        assert_eq!(ChapterStat::new(1, "x", 10, 0, 0.1).raw_ratio, 0.0);
    }

    #[test]
    fn renders_three_panels_with_any_scales() {
        let dir = std::env::temp_dir().join(format!("jpfe_stats_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for scale in [Scale::Linear, Scale::Log] {
            let path = dir.join(format!("{}.svg", scale.label()));
            let scales = ChartScales { ratio: scale, words: scale, characters: scale };
            render_chart(&path, &sample(), scales).unwrap();
            let svg = fs::read_to_string(&path).unwrap();
            assert!(svg.contains("New vocabulary density"));
            assert!(svg.contains("Filtered words exported"));
            assert!(svg.contains("Chapter length"));
            assert!(svg.contains("<title>Chapter 0: プロローグ &lt;Intro&gt;"));
            assert!(svg.contains("min 0 · avg 267 · max 500"));
            assert!(svg.trim_end().ends_with("</svg>"));
        }
        render_chart(&dir.join("empty.svg"), &[], ChartScales { ratio: Scale::Log, words: Scale::Log, characters: Scale::Log }).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn log_ticks_adapt_to_range() {
        assert_eq!(log_ticks(0.9, 120.0).len(), 7); // 1 2 5 10 20 50 100
        assert_eq!(log_ticks(330.0, 650.0).len(), 3); // 400 500 600
    }

    #[test]
    fn formats_ratios() {
        assert_eq!(format_ratio(&1.0), "1");
        assert_eq!(format_ratio(&0.5), "0.5");
        assert_eq!(format_ratio(&0.001), "0.001");
        assert_eq!(format_ratio(&0.6259), "0.626");
        assert_eq!(format_ratio(&20.0), "20");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(&0.0), "0");
        assert_eq!(format_count(&999.0), "999");
        assert_eq!(format_count(&5398.4), "5,398");
        assert_eq!(format_count(&12345678.0), "12,345,678");
    }
}
