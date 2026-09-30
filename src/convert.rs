use crate::Source;
use crate::ink::encode_stroke_input_batch;
use crate::model::{
    Attachment, ConvertedNotebook, ConvertedPage, ConvertedSection, ConvertedStroke,
};
use anyhow::{Context, Result, bail};
use image::GenericImageView;
use onenote_parser::contents::{
    Content, EmbeddedObject, Image, Ink, InkStroke, MathInlineObject, MathObjectType, Outline,
    OutlineElement, OutlineItem, ParagraphStyling, RichText, Table,
};
use onenote_parser::page::Page;
use onenote_parser::property::common::{Color, ColorRef};
use onenote_parser::property::rich_text::ParagraphAlignment;
use onenote_parser::section::{Section, SectionEntry};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;
use uuid::Uuid;

const HALF_INCH_TO_DP: f32 = 80.0;
const HIMETRIC_TO_DP: f32 = 160.0 / 2540.0;
const DEFAULT_COLOR: i32 = 0xff6750a4u32 as i32;

pub fn convert(source: &Source, input: &Path) -> Result<ConvertedNotebook> {
    let name = input
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Imported OneNote notebook")
        .to_owned();
    let notebook_id = stable_id(&format!("onenote-notebook:{name}"));
    let mut converter = Converter {
        namespace: Uuid::parse_str(&notebook_id)?,
        attachments: BTreeMap::new(),
        warnings: Vec::new(),
    };
    let mut sections = Vec::new();
    let notebook_color = match source {
        Source::Notebook(notebook) => {
            converter.convert_entries(notebook.entries(), "", &mut sections)?;
            color_argb(notebook.color()).unwrap_or(DEFAULT_COLOR)
        }
        Source::Section(section) => {
            converter.convert_section(section, section.display_name(), 0, &mut sections)?;
            color_argb(section.color()).unwrap_or(DEFAULT_COLOR)
        }
    };
    if sections.is_empty() {
        bail!("the OneNote source contains no readable sections")
    }

    let created_at = sections
        .iter()
        .flat_map(|section| &section.pages)
        .map(|page| page.created_at)
        .min()
        .unwrap_or(0);
    let updated_at = sections
        .iter()
        .flat_map(|section| &section.pages)
        .map(|page| page.updated_at)
        .max()
        .unwrap_or(created_at);

    Ok(ConvertedNotebook {
        id: notebook_id,
        name,
        color_argb: notebook_color,
        created_at,
        updated_at,
        sections,
        attachments: converter.attachments.into_values().collect(),
        warnings: converter.warnings,
    })
}

struct Converter {
    namespace: Uuid,
    attachments: BTreeMap<String, Attachment>,
    warnings: Vec<String>,
}

struct PageBuild {
    page_key: String,
    created_at: i64,
    serial: usize,
    seq: i32,
    outlines: Vec<Value>,
    strokes: Vec<ConvertedStroke>,
    preview_parts: Vec<String>,
}

impl PageBuild {
    fn next_id(&mut self, namespace: &Uuid, kind: &str) -> String {
        let serial = self.serial;
        self.serial += 1;
        Uuid::new_v5(
            namespace,
            format!("{}:{kind}:{serial}", self.page_key).as_bytes(),
        )
        .to_string()
    }
}

impl Converter {
    fn convert_entries(
        &mut self,
        entries: &[SectionEntry],
        group_path: &str,
        output: &mut Vec<ConvertedSection>,
    ) -> Result<()> {
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                SectionEntry::Section(section) => {
                    let display = if group_path.is_empty() {
                        section.display_name().to_owned()
                    } else {
                        format!("{group_path} / {}", section.display_name())
                    };
                    self.convert_section(section, &display, index, output)?;
                }
                SectionEntry::SectionGroup(group) => {
                    let path = if group_path.is_empty() {
                        group.display_name().to_owned()
                    } else {
                        format!("{group_path} / {}", group.display_name())
                    };
                    self.convert_entries(group.entries(), &path, output)?;
                }
            }
        }
        Ok(())
    }

    fn convert_section(
        &mut self,
        section: &Section,
        display_name: &str,
        source_index: usize,
        output: &mut Vec<ConvertedSection>,
    ) -> Result<()> {
        let section_key = format!("section:{display_name}:{source_index}");
        let section_id = self.id(&section_key);
        let mut pages = Vec::new();
        for (page_index, page) in section
            .page_series()
            .iter()
            .flat_map(|series| series.pages())
            .enumerate()
        {
            pages.push(self.convert_page(page, &section_key, page_index)?);
        }
        let created_at = pages.iter().map(|page| page.created_at).min().unwrap_or(0);
        let updated_at = pages
            .iter()
            .map(|page| page.updated_at)
            .max()
            .unwrap_or(created_at);
        output.push(ConvertedSection {
            id: section_id,
            name: display_name.to_owned(),
            color_argb: color_argb(section.color()).unwrap_or(DEFAULT_COLOR),
            created_at,
            updated_at,
            pages,
        });
        Ok(())
    }

    fn convert_page(
        &mut self,
        page: &Page,
        section_key: &str,
        page_index: usize,
    ) -> Result<ConvertedPage> {
        let page_key = format!("{section_key}:page:{}:{page_index}", page.link_target_id());
        let page_id = self.id(&page_key);
        let created_at = timestamp_millis(page.created_time());
        let updated_at = timestamp_millis(page.updated_time());
        let mut build = PageBuild {
            page_key,
            created_at,
            serial: 0,
            seq: 0,
            outlines: Vec::new(),
            strokes: Vec::new(),
            preview_parts: Vec::new(),
        };

        for (content_index, content) in page.contents().iter().enumerate() {
            if let Some(outline) = content.outline() {
                self.convert_outline(outline, &mut build, content_index)?;
            } else if let Some(ink) = content.ink() {
                self.convert_ink(ink, 0.0, 0.0, &mut build)?;
            } else if let Some(image) = content.image() {
                let x = image.offset_horizontal().unwrap_or_default() * HALF_INCH_TO_DP;
                let y = image.offset_vertical().unwrap_or_default() * HALF_INCH_TO_DP;
                if let Some(outline) = self.convert_image(image, x, y, created_at, &mut build)? {
                    build.outlines.push(outline);
                }
            } else if content.embedded_file().is_some() {
                self.warnings.push(format!(
                    "page {:?}: omitted an embedded file",
                    page.title_text().unwrap_or("Untitled")
                ));
            } else {
                self.warnings.push(format!(
                    "page {:?}: omitted an unknown top-level object",
                    page.title_text().unwrap_or("Untitled")
                ));
            }
        }

        let doc = json!({
            "schema": 2,
            "outlines": build.outlines,
            "style": { "ruleLines": "None" }
        });
        let preview = build
            .preview_parts
            .iter()
            .map(|part| part.trim())
            .find(|part| !part.is_empty())
            .unwrap_or("")
            .chars()
            .take(4096)
            .collect::<String>();
        Ok(ConvertedPage {
            id: page_id,
            title: page
                .title_text()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or("Untitled page")
                .chars()
                .take(512)
                .collect(),
            preview,
            created_at,
            updated_at,
            doc_json: serde_json::to_string(&doc)?,
            strokes: build.strokes,
        })
    }

    fn convert_outline(
        &mut self,
        outline: &Outline,
        build: &mut PageBuild,
        _content_index: usize,
    ) -> Result<()> {
        let x = outline.offset_horizontal().unwrap_or_default() * HALF_INCH_TO_DP;
        let y = outline.offset_vertical().unwrap_or_default() * HALF_INCH_TO_DP;
        let width = outline
            .layout_max_width()
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value * HALF_INCH_TO_DP)
            .unwrap_or(720.0)
            .clamp(24.0, 10_000.0);
        let min_height = outline
            .layout_max_height()
            .unwrap_or_default()
            .mul_add(HALF_INCH_TO_DP, 0.0)
            .max(0.0);
        let mut blocks = Vec::new();
        let mut extras = Vec::new();
        self.convert_outline_items(
            outline.items(),
            outline.child_level() as usize,
            x,
            y,
            width,
            build,
            &mut blocks,
            &mut extras,
        )?;
        if !blocks.is_empty() {
            let id = build.next_id(&self.namespace, "text-outline");
            build.outlines.push(json!({
                "t": "text", "id": id, "x": x, "y": y, "width": width,
                "minHeight": min_height, "blocks": blocks
            }));
        }
        build.outlines.extend(extras);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn convert_outline_items(
        &mut self,
        items: &[OutlineItem],
        level: usize,
        x: f32,
        y: f32,
        width: f32,
        build: &mut PageBuild,
        blocks: &mut Vec<Value>,
        extras: &mut Vec<Value>,
    ) -> Result<()> {
        for item in items {
            match item {
                OutlineItem::Group(group) => self.convert_outline_items(
                    group.outlines(),
                    level + group.child_level() as usize,
                    x,
                    y,
                    width,
                    build,
                    blocks,
                    extras,
                )?,
                OutlineItem::Element(element) => {
                    self.convert_element(element, level, x, y, width, build, blocks, extras)?;
                    self.convert_outline_items(
                        element.children(),
                        level + element.child_level() as usize,
                        x,
                        y,
                        width,
                        build,
                        blocks,
                        extras,
                    )?;
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn convert_element(
        &mut self,
        element: &OutlineElement,
        level: usize,
        x: f32,
        y: f32,
        width: f32,
        build: &mut PageBuild,
        blocks: &mut Vec<Value>,
        extras: &mut Vec<Value>,
    ) -> Result<()> {
        for content in element.contents() {
            match content {
                Content::RichText(text) => {
                    if !text.embedded_objects().is_empty() {
                        for object in text.embedded_objects() {
                            if let EmbeddedObject::Ink(container) = object {
                                self.convert_ink(container.ink(), x, y, build)?;
                            }
                        }
                    } else {
                        let block = self.convert_rich_text(text, element, level, build)?;
                        if let Some(plain) = block_plain_text(&block)
                            && !plain.trim().is_empty()
                        {
                            build.preview_parts.push(plain);
                        }
                        blocks.push(block);
                    }
                }
                Content::Table(table) => {
                    extras.push(self.convert_table(table, x, y, width, build)?);
                }
                Content::Image(image) => {
                    if let Some(image) = self.convert_image(image, x, y, build.created_at, build)? {
                        extras.push(image);
                    }
                }
                Content::Ink(ink) => self.convert_ink(ink, x, y, build)?,
                Content::EmbeddedFile(_) => self
                    .warnings
                    .push("omitted an embedded file inside an outline".to_owned()),
                Content::Unknown => self
                    .warnings
                    .push("omitted an unknown object inside an outline".to_owned()),
            }
        }
        Ok(())
    }

    fn convert_rich_text(
        &mut self,
        text: &RichText,
        element: &OutlineElement,
        level: usize,
        build: &mut PageBuild,
    ) -> Result<Value> {
        let mut parts = split_utf16(text.text(), text.text_run_indices());
        if parts.is_empty() {
            parts.push(String::new());
        }
        let styles = text.text_run_formatting();
        let hyperlinks = self.text_links(text, &parts);
        let mut runs = Vec::new();
        let mut utf16_start = 0u32;
        let mut math_index = 0usize;
        let mut index = 0usize;
        while index < parts.len() {
            let style = styles.get(index).unwrap_or_else(|| text.paragraph_style());
            if style.math_formatting() {
                let start = index;
                while index < parts.len()
                    && styles
                        .get(index)
                        .unwrap_or_else(|| text.paragraph_style())
                        .math_formatting()
                {
                    index += 1;
                }
                let mut segments = Vec::new();
                for part in &parts[start..index] {
                    let object = text
                        .math_inline_objects()
                        .get(math_index)
                        .copied()
                        .unwrap_or_default();
                    segments.push((part.clone(), object));
                    math_index += 1;
                    utf16_start += part.encode_utf16().count() as u32;
                }
                let latex = math_segments_to_latex(&segments);
                let mut marks = style_marks(style, text.paragraph_style());
                marks.push(json!({ "t": "eq", "latex": latex }));
                runs.push(json!({ "text": "\u{fffc}", "marks": marks }));
                continue;
            }

            let part = &parts[index];
            let utf16_end = utf16_start + part.encode_utf16().count() as u32;
            if !style.hidden() {
                let mut marks = style_marks(style, text.paragraph_style());
                if let Some((_, _, href)) = hyperlinks
                    .iter()
                    .find(|(start, end, _)| *start < utf16_end && *end > utf16_start)
                {
                    marks.push(json!({ "t": "link", "href": href }));
                }
                runs.push(json!({ "text": part, "marks": marks }));
            }
            utf16_start = utf16_end;
            index += 1;
        }

        let block_type = if let Some(list) = element.list_contents().first() {
            if list.list_format().first() == Some(&'\u{fffd}') {
                "Numbered"
            } else {
                "Bullet"
            }
        } else {
            match text
                .paragraph_style()
                .style_id()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str()
            {
                "h1" | "heading 1" | "heading1" | "title" => "Heading1",
                "h2" | "heading 2" | "heading2" => "Heading2",
                "h3" | "heading 3" | "heading3" => "Heading3",
                _ => "Paragraph",
            }
        };
        let align = match text.paragraph_alignment() {
            ParagraphAlignment::Center => "Center",
            ParagraphAlignment::Right => "End",
            _ => "Start",
        };
        Ok(json!({
            "id": build.next_id(&self.namespace, "block"),
            "type": block_type,
            "indent": level.min(16),
            "align": align,
            "runs": runs
        }))
    }

    /// Visible link ranges in UTF-16 offsets, each with a destination ViveNotes can open.
    ///
    /// OneNote normally keeps the destination in a hidden marker before the linked text, which the
    /// parser resolves. A URL OneNote linked as it was typed can instead be a hyperlink-formatted
    /// run with no marker, its text being the destination.
    fn text_links(&mut self, text: &RichText, parts: &[String]) -> Vec<(u32, u32, String)> {
        let marked = text.hyperlinks();
        let ranges = marked
            .iter()
            .map(|link| (link.start(), link.end()))
            .collect::<Vec<_>>();
        let runs = parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let style = text
                    .text_run_formatting()
                    .get(index)
                    .unwrap_or_else(|| text.paragraph_style());
                (part.as_str(), style.hyperlink() && !style.hidden())
            })
            .collect::<Vec<_>>();
        let mut links = unmarked_links(&runs, &ranges);
        for link in marked {
            match web_link(link.target()) {
                Some(href) => links.push((link.start(), link.end(), href)),
                None => self
                    .warnings
                    .push("omitted a link to a non-web destination, keeping its text".to_owned()),
            }
        }
        links
    }

    fn convert_table(
        &mut self,
        table: &Table,
        x: f32,
        y: f32,
        fallback_width: f32,
        build: &mut PageBuild,
    ) -> Result<Value> {
        let column_count = (table.cols() as usize).clamp(1, 12);
        let mut columns = table
            .col_widths()
            .iter()
            .take(column_count)
            .map(|width| (*width * HALF_INCH_TO_DP).clamp(48.0, 1200.0))
            .collect::<Vec<_>>();
        if columns.len() < column_count {
            let default = (fallback_width / column_count as f32).clamp(48.0, 1200.0);
            columns.resize(column_count, default);
        }
        let mut rows = Vec::new();
        for row in table.contents().iter().take(50) {
            let mut cells = Vec::new();
            for cell in row.contents().iter().take(column_count) {
                let mut cell_blocks = Vec::new();
                for element in cell.contents() {
                    for content in element.contents() {
                        if let Content::RichText(text) = content {
                            cell_blocks.push(self.convert_rich_text(text, element, 0, build)?);
                        }
                    }
                }
                if cell_blocks.is_empty() {
                    cell_blocks.push(json!({
                        "id": build.next_id(&self.namespace, "table-empty-block"),
                        "runs": []
                    }));
                }
                cells.push(json!({
                    "id": build.next_id(&self.namespace, "table-cell"),
                    "blocks": cell_blocks
                }));
            }
            while cells.len() < column_count {
                cells.push(json!({
                    "id": build.next_id(&self.namespace, "table-cell"),
                    "blocks": [{
                        "id": build.next_id(&self.namespace, "table-empty-block"),
                        "runs": []
                    }]
                }));
            }
            rows.push(json!({
                "id": build.next_id(&self.namespace, "table-row"),
                "minHeight": 42.0,
                "cells": cells
            }));
        }
        let width: f32 = columns.iter().sum();
        Ok(json!({
            "t": "table", "id": build.next_id(&self.namespace, "table"),
            "x": x, "y": y, "width": width, "columns": columns, "rows": rows,
            "headerRow": false, "headerColumn": false,
            "borderArgb": 0xff000000u32 as i32,
            "borderFollowsTheme": false,
            "borderWidth": if table.borders_visible() { 1.0 } else { 0.0 },
            "inkOnly": false
        }))
    }

    fn convert_image(
        &mut self,
        image: &Image,
        x: f32,
        y: f32,
        created_at: i64,
        build: &mut PageBuild,
    ) -> Result<Option<Value>> {
        let Some(mut reader) = image.read() else {
            self.warnings
                .push("omitted an image whose bytes are unavailable".to_owned());
            return Ok(None);
        };
        let mut source = Vec::new();
        reader.read_to_end(&mut source)?;
        let decoded = match image::ImageReader::new(Cursor::new(&source))
            .with_guessed_format()
            .context("detecting embedded image format")?
            .decode()
        {
            Ok(decoded) => decoded,
            Err(error) => {
                self.warnings
                    .push(format!("omitted an unreadable image: {error}"));
                return Ok(None);
            }
        };
        let (source_width, source_height) = decoded.dimensions();
        let resized = if source_width > 2048 || source_height > 2048 {
            decoded.resize(2048, 2048, image::imageops::FilterType::Lanczos3)
        } else {
            decoded
        };
        let (pixel_width, pixel_height) = resized.dimensions();
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 88)
            .encode_image(&resized.to_rgb8())?;
        let id = sha256(&bytes);
        self.attachments
            .entry(id.clone())
            .and_modify(|attachment| attachment.ref_count += 1)
            .or_insert_with(|| Attachment {
                id: id.clone(),
                mime_type: "image/jpeg",
                pixel_width,
                pixel_height,
                bytes,
                ref_count: 1,
                created_at,
            });

        let natural_width = source_width as f32;
        let natural_height = source_height.max(1) as f32;
        let width = image
            .picture_width()
            .or(image.layout_max_width())
            .map(|value| value * HALF_INCH_TO_DP)
            .unwrap_or(320.0)
            .clamp(24.0, 10_000.0);
        let height = image
            .picture_height()
            .or(image.layout_max_height())
            .map(|value| value * HALF_INCH_TO_DP)
            .unwrap_or(width * natural_height / natural_width.max(1.0))
            .clamp(24.0, 10_000.0);
        Ok(Some(json!({
            "t": "image", "id": build.next_id(&self.namespace, "image"),
            "x": x, "y": y, "width": width, "height": height,
            "attachmentId": id
        })))
    }

    fn convert_ink(
        &self,
        ink: &Ink,
        base_x: f32,
        base_y: f32,
        build: &mut PageBuild,
    ) -> Result<()> {
        let x = base_x + ink.offset_horizontal().unwrap_or_default() * HALF_INCH_TO_DP;
        let y = base_y + ink.offset_vertical().unwrap_or_default() * HALF_INCH_TO_DP;
        for child in ink.child_groups() {
            self.convert_ink(child, x, y, build)?;
        }
        for stroke in ink.ink_strokes() {
            if let Some(converted) = self.convert_stroke(stroke, x, y, build)? {
                build.strokes.push(converted);
            }
        }
        Ok(())
    }

    fn convert_stroke(
        &self,
        stroke: &InkStroke,
        offset_x: f32,
        offset_y: f32,
        build: &mut PageBuild,
    ) -> Result<Option<ConvertedStroke>> {
        let Some(first) = stroke.path().first() else {
            return Ok(None);
        };
        let mut source_x = first.x();
        let mut source_y = first.y();
        let mut points = vec![(
            offset_x + source_x * HIMETRIC_TO_DP,
            offset_y + source_y * HIMETRIC_TO_DP,
        )];
        for delta in &stroke.path()[1..] {
            source_x += delta.x();
            source_y += delta.y();
            points.push((
                offset_x + source_x * HIMETRIC_TO_DP,
                offset_y + source_y * HIMETRIC_TO_DP,
            ));
        }
        if points.len() == 1 {
            points.push(points[0]);
        }
        let size_dp = stroke
            .width()
            .max(stroke.height())
            .mul_add(HIMETRIC_TO_DP, 0.0)
            .clamp(0.25, 1000.0);
        let transparency = stroke.transparency().unwrap_or_default();
        let alpha = 255u8.saturating_sub(transparency);
        let is_highlighter = stroke.pen_tip() == Some(1) && stroke.transparency().is_some();
        if is_highlighter {
            // OneNote shape-highlights are often stored as only the four rectangle corners plus
            // the closing point. AndroidX Ink's highlighter brush expects a sampled centerline;
            // feeding it thousand-dp jumps makes its chisel tip bridge the corners diagonally.
            // Sampling at a fraction of the nib width retains the exact polyline while giving the
            // brush enough inputs to form the same straight, square-edged bands OneNote draws.
            points = densify_polyline(&points, (size_dp * 0.125).clamp(1.0, 4.0));
        }
        let half = size_dp / 2.0;
        let min_x = points
            .iter()
            .map(|point| point.0)
            .fold(f32::INFINITY, f32::min)
            - half;
        let min_y = points
            .iter()
            .map(|point| point.1)
            .fold(f32::INFINITY, f32::min)
            - half;
        let max_x = points
            .iter()
            .map(|point| point.0)
            .fold(f32::NEG_INFINITY, f32::max)
            + half;
        let max_y = points
            .iter()
            .map(|point| point.1)
            .fold(f32::NEG_INFINITY, f32::max)
            + half;
        let color_argb = stroke
            .color()
            .map(|color| onenote_ink_color_argb(color, alpha))
            .unwrap_or(0xff000000u32 as i32);
        let color_follows_theme = stroke.color().is_none();
        let brush_family = if is_highlighter {
            "highlighter"
        } else {
            "marker"
        };
        let seq = build.seq;
        build.seq += 1;
        Ok(Some(ConvertedStroke {
            id: build.next_id(&self.namespace, "stroke"),
            seq,
            brush_family,
            size_dp,
            color_argb,
            color_follows_theme,
            min_x,
            min_y,
            max_x,
            max_y,
            points: encode_stroke_input_batch(&points)?,
            created_at: build.created_at,
        }))
    }

    fn id(&self, key: &str) -> String {
        Uuid::new_v5(&self.namespace, key.as_bytes()).to_string()
    }
}

fn split_utf16(text: &str, ends: &[u32]) -> Vec<String> {
    if ends.is_empty() {
        return vec![text.to_owned()];
    }
    let mut parts = Vec::new();
    let mut byte_start = 0usize;
    let mut utf16 = 0u32;
    let mut ends = ends.iter().copied().peekable();
    for (byte, character) in text.char_indices() {
        while ends.peek().is_some_and(|end| *end == utf16) {
            parts.push(text[byte_start..byte].to_owned());
            byte_start = byte;
            ends.next();
        }
        utf16 += character.len_utf16() as u32;
        let byte_end = byte + character.len_utf8();
        while ends.peek().is_some_and(|end| *end == utf16) {
            parts.push(text[byte_start..byte_end].to_owned());
            byte_start = byte_end;
            ends.next();
        }
    }
    if byte_start < text.len() || parts.is_empty() {
        parts.push(text[byte_start..].to_owned());
    }
    parts
}

/// OneNote's `InkColor` is a Windows COLORREF: `0x00BBGGRR`, not an ARGB/RGB
/// integer. Moving the low red byte into ARGB's red position is what keeps, for
/// example, OneNote's `0x0000FFFF` yellow instead of turning it cyan.
fn onenote_ink_color_argb(color_ref: u32, alpha: u8) -> i32 {
    let red = color_ref & 0xff;
    let green = color_ref & 0xff00;
    let blue = (color_ref >> 16) & 0xff;
    (((alpha as u32) << 24) | (red << 16) | green | blue) as i32
}

fn densify_polyline(points: &[(f32, f32)], max_step: f32) -> Vec<(f32, f32)> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut output = vec![first];
    for pair in points.windows(2) {
        let (start_x, start_y) = pair[0];
        let (end_x, end_y) = pair[1];
        let dx = end_x - start_x;
        let dy = end_y - start_y;
        let steps = (dx.hypot(dy) / max_step).ceil().max(1.0) as usize;
        for step in 1..=steps {
            let fraction = step as f32 / steps as f32;
            output.push((start_x + dx * fraction, start_y + dy * fraction));
        }
    }
    output
}

fn style_marks(style: &ParagraphStyling, base: &ParagraphStyling) -> Vec<Value> {
    let mut marks = Vec::new();
    if style.bold() || base.bold() {
        marks.push(json!({ "t": "b" }));
    }
    if style.italic() || base.italic() {
        marks.push(json!({ "t": "i" }));
    }
    if style.underline() || base.underline() {
        marks.push(json!({ "t": "u" }));
    }
    if style.strikethrough() || base.strikethrough() {
        marks.push(json!({ "t": "s" }));
    }
    if style.subscript() || base.subscript() {
        marks.push(json!({ "t": "sub" }));
    }
    if style.superscript() || base.superscript() {
        marks.push(json!({ "t": "sup" }));
    }
    if let Some(ColorRef::Manual { r, g, b }) = style.font_color().or(base.font_color()) {
        marks.push(json!({ "t": "color", "argb": argb(255, r, g, b) }));
    }
    if let Some(ColorRef::Manual { r, g, b }) = style.highlight().or(base.highlight()) {
        marks.push(json!({ "t": "hl", "argb": argb(255, r, g, b) }));
    }
    if let Some(size) = style.font_size().or(base.font_size()) {
        marks.push(json!({ "t": "size", "sp": ((size as f32) / 2.0).round() as i32 }));
    }
    if let Some(font) = style.font().or(base.font()) {
        marks.push(json!({ "t": "font", "name": font }));
    }
    marks
}

/// Hyperlink-formatted `(text, linked)` runs outside every `marked` range, joined into contiguous
/// ranges and kept where their text is itself a web address.
fn unmarked_links(runs: &[(&str, bool)], marked: &[(u32, u32)]) -> Vec<(u32, u32, String)> {
    let mut spans = Vec::new();
    let mut open: Option<(u32, u32, String)> = None;
    let mut start = 0u32;
    for (text, linked) in runs {
        let end = start + text.encode_utf16().count() as u32;
        if *linked && !marked.iter().any(|(from, to)| *from < end && *to > start) {
            let span = open.get_or_insert_with(|| (start, start, String::new()));
            span.1 = end;
            span.2.push_str(text);
        } else {
            spans.extend(open.take());
        }
        start = end;
    }
    spans.extend(open);
    spans
        .into_iter()
        .filter_map(|(start, end, text)| Some((start, end, web_link(&text)?)))
        .collect()
}

/// The destination as ViveNotes stores a link it can open: an http(s) address with a host and no
/// user info, or a `www.` host given a scheme. Anything else, such as a `onenote:` page link, a
/// `mailto:` address or a file path, would open as a broken web address in the app.
fn web_link(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() || target.chars().any(char::is_whitespace) {
        return None;
    }
    let url = if target.contains("://") {
        target.to_owned()
    } else if target
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("www."))
    {
        format!("https://{target}")
    } else {
        return None;
    };
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    (!authority.contains('@') && !host.is_empty()).then_some(url)
}

fn block_plain_text(block: &Value) -> Option<String> {
    let runs = block.get("runs")?.as_array()?;
    Some(
        runs.iter()
            .filter_map(|run| run.get("text")?.as_str())
            .collect::<String>()
            .replace('\u{fffc}', ""),
    )
}

#[derive(Clone)]
enum MathToken {
    Text(String),
    Start(MathInlineObject),
    Sep(MathObjectType),
    End(MathObjectType),
}

fn math_segments_to_latex(segments: &[(String, MathInlineObject)]) -> String {
    let mut tokens = Vec::new();
    for (text, object) in segments {
        let mut text = text.as_str();
        if let Some(rest) = text.strip_prefix('\u{fdd0}') {
            tokens.push(MathToken::Start(*object));
            text = rest;
        }
        if let Some(rest) = text.strip_suffix('\u{fdee}') {
            if !rest.is_empty() {
                tokens.push(MathToken::Text(rest.to_owned()));
            }
            tokens.push(MathToken::Sep(object.object_type()));
        } else if let Some(rest) = text.strip_suffix('\u{fdef}') {
            if !rest.is_empty() {
                tokens.push(MathToken::Text(rest.to_owned()));
            }
            tokens.push(MathToken::End(object.object_type()));
        } else if !text.is_empty() {
            tokens.push(MathToken::Text(text.to_owned()));
        }
    }
    let mut cursor = 0usize;
    parse_math_expression(&tokens, &mut cursor, None).0
}

fn parse_math_expression(
    tokens: &[MathToken],
    cursor: &mut usize,
    stop: Option<MathObjectType>,
) -> (String, bool) {
    let mut output = String::new();
    while let Some(token) = tokens.get(*cursor) {
        match token {
            MathToken::Text(text) => {
                output.push_str(&escape_latex(text));
                *cursor += 1;
            }
            MathToken::Start(object) => {
                let object = *object;
                *cursor += 1;
                output.push_str(&parse_math_object(tokens, cursor, object));
            }
            MathToken::Sep(kind) if Some(*kind) == stop => return (output, false),
            MathToken::End(kind) if Some(*kind) == stop => return (output, true),
            MathToken::Sep(_) | MathToken::End(_) => {
                *cursor += 1;
            }
        }
    }
    (output, true)
}

fn parse_math_object(tokens: &[MathToken], cursor: &mut usize, object: MathInlineObject) -> String {
    let kind = object.object_type();
    let mut args = Vec::new();
    loop {
        let (arg, ended) = parse_math_expression(tokens, cursor, Some(kind));
        args.push(arg);
        if ended || *cursor >= tokens.len() {
            if matches!(tokens.get(*cursor), Some(MathToken::End(_))) {
                *cursor += 1;
            }
            break;
        }
        *cursor += 1;
    }
    let arg = |index: usize| args.get(index).cloned().unwrap_or_default();
    match kind {
        MathObjectType::Fraction => format!("\\frac{{{}}}{{{}}}", arg(0), arg(1)),
        MathObjectType::SlashedFraction => format!("{{{}}}/{{{}}}", arg(0), arg(1)),
        MathObjectType::Radical => {
            if arg(0).is_empty() {
                format!("\\sqrt{{{}}}", arg(1))
            } else {
                format!("\\sqrt[{}]{{{}}}", arg(0), arg(1))
            }
        }
        MathObjectType::Subscript => format!("{{{}}}_{{{}}}", arg(0), arg(1)),
        MathObjectType::Superscript => format!("{{{}}}^{{{}}}", arg(0), arg(1)),
        MathObjectType::SubSup => format!("{{{}}}_{{{}}}^{{{}}}", arg(0), arg(1), arg(2)),
        MathObjectType::LeftSubSup => format!("_{{{}}}^{{{}}}{{{}}}", arg(0), arg(1), arg(2)),
        MathObjectType::Brackets | MathObjectType::BracketsWithSeps => format!(
            "\\left{}{}\\right{}",
            latex_delimiter(object.char().unwrap_or('(')),
            args.join(&object.char2().unwrap_or(',').to_string()),
            latex_delimiter(object.char1().unwrap_or(')'))
        ),
        MathObjectType::Matrix => format!("\\begin{{matrix}}{}\\end{{matrix}}", args.join(" & ")),
        MathObjectType::Nary => format!(
            "{}{}_{{{}}}^{{{}}}{}",
            nary_operator(object.char().unwrap_or('∑')),
            "",
            arg(0),
            arg(1),
            arg(2)
        ),
        MathObjectType::Overbar => format!("\\overline{{{}}}", arg(0)),
        MathObjectType::Underbar => format!("\\underline{{{}}}", arg(0)),
        MathObjectType::BoxedFormula => format!("\\boxed{{{}}}", arg(0)),
        MathObjectType::FunctionApply => format!("{}\\left({}\\right)", arg(0), arg(1)),
        _ => args.join(""),
    }
}

fn escape_latex(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(*character, '\u{fdd0}' | '\u{fdee}' | '\u{fdef}'))
        .flat_map(|character| match character {
            '\\' => "\\backslash ".chars().collect::<Vec<_>>(),
            '{' => "\\{".chars().collect(),
            '}' => "\\}".chars().collect(),
            '#' => "\\#".chars().collect(),
            '$' => "\\$".chars().collect(),
            '%' => "\\%".chars().collect(),
            '&' => "\\&".chars().collect(),
            '_' => "\\_".chars().collect(),
            '^' => "\\^{}".chars().collect(),
            other => vec![other],
        })
        .collect()
}

fn latex_delimiter(character: char) -> String {
    match character {
        '{' | '}' => format!("\\{character}"),
        other => other.to_string(),
    }
}

fn nary_operator(character: char) -> &'static str {
    match character {
        '∏' => "\\prod",
        '∫' => "\\int",
        '∮' => "\\oint",
        _ => "\\sum",
    }
}

fn stable_id(value: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, value.as_bytes()).to_string()
}

fn timestamp_millis(value: time::UtcDateTime) -> i64 {
    value.unix_timestamp().saturating_mul(1000) + value.nanosecond() as i64 / 1_000_000
}

fn color_argb(color: Option<Color>) -> Option<i32> {
    color.map(|color| argb(color.alpha(), color.r(), color.g(), color.b()))
}

fn argb(alpha: u8, red: u8, green: u8, blue: u8) -> i32 {
    u32::from_be_bytes([alpha, red, green, blue]) as i32
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_by_utf16_offsets() {
        assert_eq!(split_utf16("a😀b", &[1, 3, 4]), ["a", "😀", "b"]);
    }

    #[test]
    fn keeps_only_links_the_app_can_open() {
        assert_eq!(
            web_link(" https://www.youtube.com/watch?v=a&t=5s "),
            Some("https://www.youtube.com/watch?v=a&t=5s".to_owned())
        );
        assert_eq!(
            web_link("HTTP://example.com:8080/a"),
            Some("HTTP://example.com:8080/a".to_owned())
        );
        assert_eq!(
            web_link("www.example.com/a"),
            Some("https://www.example.com/a".to_owned())
        );
        for target in [
            "onenote:https://d.docs.live.net/nb/Section.one#Page&page-id={1}",
            "onenote:#Page&section-id={1}",
            "mailto:someone@example.com",
            "file:///C:/notes/a.pdf",
            "C:\\notes\\a.pdf",
            "https://user@example.com/",
            "https://",
            "https://example.com/has space",
            "Video",
            "",
        ] {
            assert_eq!(web_link(target), None, "{target}");
        }
    }

    #[test]
    fn links_hyperlink_runs_whose_text_is_the_address() {
        // A URL OneNote linked as it was typed: formatted as a link, but with no hidden marker.
        assert_eq!(
            unmarked_links(
                &[
                    ("https://youtu.be/", true),
                    ("abc", true),
                    (" after", false)
                ],
                &[]
            ),
            [(0, 20, "https://youtu.be/abc".to_owned())]
        );
        // Runs a marker already links, and linked labels that are not addresses, gain nothing.
        assert!(unmarked_links(&[("Video", true)], &[(0, 5)]).is_empty());
        assert!(unmarked_links(&[("https://example.com", true)], &[(0, 19)]).is_empty());
        assert!(unmarked_links(&[("Video", true)], &[]).is_empty());
        // Offsets are UTF-16, matching the parser's link ranges.
        assert_eq!(
            unmarked_links(&[("😀 ", false), ("www.example.com", true)], &[]),
            [(3, 18, "https://www.example.com".to_owned())]
        );
    }

    #[test]
    fn escapes_latex_syntax() {
        assert_eq!(escape_latex("a_b"), "a\\_b");
    }

    #[test]
    fn converts_windows_colorref_to_argb() {
        assert_eq!(
            onenote_ink_color_argb(0x0000_ffff, 0x7f) as u32,
            0x7fff_ff00
        );
        assert_eq!(
            onenote_ink_color_argb(0x00ff_0000, 0xff) as u32,
            0xff00_00ff
        );
    }

    #[test]
    fn densifies_sparse_highlighter_shapes_without_changing_the_path() {
        let input = [(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0), (0.0, 0.0)];
        let dense = densify_polyline(&input, 2.0);
        assert_eq!(dense.first(), input.first());
        assert_eq!(dense.last(), input.last());
        assert!(dense.windows(2).all(|pair| {
            let dx = pair[1].0 - pair[0].0;
            let dy = pair[1].1 - pair[0].1;
            dx.hypot(dy) <= 2.001
        }));
    }
}
