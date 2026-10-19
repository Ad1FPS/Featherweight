use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use tiny_skia::{FillRule, Paint, Path as SkPath, PathBuilder, Pixmap, Stroke, Transform};

pub const VIEW_WIDTH: f32 = 106.0;

/// Vertical stretch of the mouse shape. Width stays the same, height is
/// multiplied by this factor and clamped to the icon size.
pub const VERTICAL_SCALE: f32 = 1.6;

const SHAPE_LEFT: f32 = 3.0;
const SHAPE_RIGHT: f32 = 103.0;
const SHAPE_TOP: f32 = 3.0;
const SHAPE_BOTTOM: f32 = 35.1;

const SHAPE_PATH: &str = "M12.8 35.1H97.6Q100 35.1 101 33.4L103 30.2C101.5 23 96.5 16.5 89 12.2C80 7.2 66 3 55.2 3C48 3 40 4 33 6.8C31 7.7 30.2 8.6 28.8 8.4C24 7.6 20 8 17.6 11C16.6 12.4 16.4 14.5 15 16L3.6 23.1Q2.6 23.9 3.4 25.2Q4.4 26.4 5.6 27.4L8.4 29.8L10.3 32.1Q11 33.2 10.8 34.1Q11.2 35.1 12.8 35.1Z";

const OUTLINE_DARK: [u8; 3] = [0xE6, 0xE6, 0xE6];
const OUTLINE_LIGHT: [u8; 3] = [0x1A, 0x1A, 0x1A];
const EMPTY_DARK: [u8; 3] = [0x3A, 0x3A, 0x3A];
const EMPTY_LIGHT: [u8; 3] = [0xC8, 0xC8, 0xC8];
const GREEN: [u8; 3] = [0x3D, 0xBE, 0x6B];
const YELLOW: [u8; 3] = [0xE8, 0xB9, 0x31];
const RED: [u8; 3] = [0xE5, 0x48, 0x4D];

const CURVE_TOLERANCE: f32 = 0.05;

#[derive(Clone, Copy)]
enum Token {
    Command(char),
    Number(f32),
}

pub fn render_icon(size_px: u32, battery: Option<u8>, light_taskbar: bool) -> Vec<u8> {
    render_icon_scaled(size_px, battery, light_taskbar, VERTICAL_SCALE)
}

pub fn render_icon_scaled(
    size_px: u32,
    battery: Option<u8>,
    light_taskbar: bool,
    vertical_scale: f32,
) -> Vec<u8> {
    let size = size_px.max(1);
    let Some(mut pixmap) = Pixmap::new(size, size) else {
        return vec![0; size as usize * size as usize * 4];
    };

    let polygon = flatten_shape();
    let width_scale = size as f32 / VIEW_WIDTH;
    let shape_height = SHAPE_BOTTOM - SHAPE_TOP;
    let y_scale = (width_scale * vertical_scale.max(0.1)).min(size as f32 / shape_height);
    let y_offset = (size as f32 - shape_height * y_scale) * 0.5;
    let project = |point: (f32, f32)| -> (f32, f32) {
        (point.0 * width_scale, (point.1 - SHAPE_TOP) * y_scale + y_offset)
    };

    let base_width = (1.5 * size as f32 / 32.0).max(1.0);
    let width = if size <= 24 {
        base_width * 1.3
    } else {
        base_width
    };
    let low_battery = matches!(battery, Some(percent) if percent < 15);

    if let Some(percent) = battery {
        let percent = percent.min(100);
        let mut left_edge =
            SHAPE_LEFT + (1.0 - f32::from(percent) / 100.0) * (SHAPE_RIGHT - SHAPE_LEFT);

        if percent > 0 {
            let min_fill_width = (3.0 + width * 0.5) / width_scale;
            left_edge = left_edge.min((SHAPE_RIGHT - min_fill_width).max(SHAPE_LEFT));
        }

        let empty = clip_right(&polygon, left_edge);
        if empty.len() >= 3 {
            let [red, green, blue] = if light_taskbar {
                EMPTY_LIGHT
            } else {
                EMPTY_DARK
            };
            fill_polygon(&mut pixmap, &empty, &project, [red, green, blue]);
        }

        let filled = clip_left(&polygon, left_edge);
        if filled.len() >= 3 {
            fill_polygon(&mut pixmap, &filled, &project, fill_color(percent));
        }
    }

    let projected: Vec<(f32, f32)> = polygon.iter().map(|p| project(*p)).collect();
    if let Some(path) = polygon_path(&projected) {
        let [red, green, blue] = if low_battery {
            RED
        } else if light_taskbar {
            OUTLINE_LIGHT
        } else {
            OUTLINE_DARK
        };
        let alpha = if battery.is_some() { 255 } else { 128 };
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color_rgba8(red, green, blue, alpha);
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    let mut rgba = vec![0; size as usize * size as usize * 4];
    for (index, pixel) in pixmap.pixels().iter().enumerate() {
        let color = pixel.demultiply();
        let offset = index * 4;
        rgba[offset] = color.red();
        rgba[offset + 1] = color.green();
        rgba[offset + 2] = color.blue();
        rgba[offset + 3] = color.alpha();
    }
    rgba
}

fn fill_color(percent: u8) -> [u8; 3] {
    if percent > 30 {
        GREEN
    } else if percent >= 15 {
        YELLOW
    } else {
        RED
    }
}

fn fill_polygon(
    pixmap: &mut Pixmap,
    polygon: &[(f32, f32)],
    project: &impl Fn((f32, f32)) -> (f32, f32),
    color: [u8; 3],
) {
    let projected: Vec<(f32, f32)> = polygon.iter().map(|point| project(*point)).collect();
    let Some(path) = polygon_path(&projected) else {
        return;
    };
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color_rgba8(color[0], color[1], color[2], 255);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn flatten_shape() -> Vec<(f32, f32)> {
    let tokens = tokenize(SHAPE_PATH);
    let mut points = Vec::new();
    let mut index = 0usize;
    let mut current = (0.0, 0.0);
    let mut start = (0.0, 0.0);

    while index < tokens.len() {
        let Token::Command(command) = tokens[index] else {
            index += 1;
            continue;
        };
        index += 1;
        match command {
            'M' => {
                let x = take_number(&tokens, &mut index);
                let y = take_number(&tokens, &mut index);
                current = (x, y);
                start = current;
                points.push(current);
            }
            'L' => {
                let x = take_number(&tokens, &mut index);
                let y = take_number(&tokens, &mut index);
                current = (x, y);
                points.push(current);
            }
            'H' => {
                let x = take_number(&tokens, &mut index);
                current = (x, current.1);
                points.push(current);
            }
            'V' => {
                let y = take_number(&tokens, &mut index);
                current = (current.0, y);
                points.push(current);
            }
            'Q' => {
                let control = (
                    take_number(&tokens, &mut index),
                    take_number(&tokens, &mut index),
                );
                let end = (
                    take_number(&tokens, &mut index),
                    take_number(&tokens, &mut index),
                );
                flatten_quadratic(current, control, end, 0, &mut points);
                current = end;
            }
            'C' => {
                let control1 = (
                    take_number(&tokens, &mut index),
                    take_number(&tokens, &mut index),
                );
                let control2 = (
                    take_number(&tokens, &mut index),
                    take_number(&tokens, &mut index),
                );
                let end = (
                    take_number(&tokens, &mut index),
                    take_number(&tokens, &mut index),
                );
                flatten_cubic(current, control1, control2, end, 0, &mut points);
                current = end;
            }
            'Z' | 'z' => {
                if points.last().copied() != Some(start) {
                    points.push(start);
                }
                current = start;
            }
            _ => {}
        }
    }

    points
}

fn tokenize(data: &str) -> Vec<Token> {
    let bytes = data.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() || byte == b',' {
            index += 1;
            continue;
        }
        if byte.is_ascii_alphabetic() {
            tokens.push(Token::Command(byte as char));
            index += 1;
            continue;
        }
        let start = index;
        if byte == b'-' || byte == b'+' {
            index += 1;
        }
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if let Ok(text) = std::str::from_utf8(&bytes[start..index]) {
            if let Ok(value) = text.parse::<f32>() {
                tokens.push(Token::Number(value));
            }
        }
    }
    tokens
}

fn take_number(tokens: &[Token], index: &mut usize) -> f32 {
    while *index < tokens.len() {
        if let Token::Number(value) = tokens[*index] {
            *index += 1;
            return value;
        }
        *index += 1;
    }
    0.0
}

fn flatten_quadratic(
    start: (f32, f32),
    control: (f32, f32),
    end: (f32, f32),
    depth: u32,
    out: &mut Vec<(f32, f32)>,
) {
    if depth >= 12 || distance_to_line(control, start, end) <= CURVE_TOLERANCE {
        out.push(end);
        return;
    }
    let first = midpoint(start, control);
    let second = midpoint(control, end);
    let middle = midpoint(first, second);
    flatten_quadratic(start, first, middle, depth + 1, out);
    flatten_quadratic(middle, second, end, depth + 1, out);
}

fn flatten_cubic(
    start: (f32, f32),
    control1: (f32, f32),
    control2: (f32, f32),
    end: (f32, f32),
    depth: u32,
    out: &mut Vec<(f32, f32)>,
) {
    if depth >= 14
        || (distance_to_line(control1, start, end) <= CURVE_TOLERANCE
            && distance_to_line(control2, start, end) <= CURVE_TOLERANCE)
    {
        out.push(end);
        return;
    }
    let p01 = midpoint(start, control1);
    let p12 = midpoint(control1, control2);
    let p23 = midpoint(control2, end);
    let p012 = midpoint(p01, p12);
    let p123 = midpoint(p12, p23);
    let p0123 = midpoint(p012, p123);
    flatten_cubic(start, p01, p012, p0123, depth + 1, out);
    flatten_cubic(p0123, p123, p23, end, depth + 1, out);
}

fn midpoint(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

fn distance_to_line(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length = (dx * dx + dy * dy).sqrt();
    if length < f32::EPSILON {
        let px = point.0 - start.0;
        let py = point.1 - start.1;
        return (px * px + py * py).sqrt();
    }
    ((point.0 - start.0) * dy - (point.1 - start.1) * dx).abs() / length
}

fn clip_left(polygon: &[(f32, f32)], left: f32) -> Vec<(f32, f32)> {
    let mut output = Vec::with_capacity(polygon.len() + 2);
    if polygon.is_empty() {
        return output;
    }
    let mut previous = polygon[polygon.len() - 1];
    let mut previous_inside = previous.0 >= left;
    for &current in polygon {
        let current_inside = current.0 >= left;
        if previous_inside != current_inside {
            let dx = current.0 - previous.0;
            let t = if dx.abs() < f32::EPSILON {
                0.0
            } else {
                (left - previous.0) / dx
            };
            output.push((left, previous.1 + (current.1 - previous.1) * t));
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    output
}

fn clip_right(polygon: &[(f32, f32)], right: f32) -> Vec<(f32, f32)> {
    let mut output = Vec::with_capacity(polygon.len() + 2);
    if polygon.is_empty() {
        return output;
    }
    let mut previous = polygon[polygon.len() - 1];
    let mut previous_inside = previous.0 <= right;
    for &current in polygon {
        let current_inside = current.0 <= right;
        if previous_inside != current_inside {
            let dx = current.0 - previous.0;
            let t = if dx.abs() < f32::EPSILON {
                0.0
            } else {
                (right - previous.0) / dx
            };
            output.push((right, previous.1 + (current.1 - previous.1) * t));
        }
        if current_inside {
            output.push(current);
        }
        previous = current;
        previous_inside = current_inside;
    }
    output
}

fn polygon_path(points: &[(f32, f32)]) -> Option<SkPath> {
    if points.len() < 2 {
        return None;
    }
    let mut builder = PathBuilder::new();
    builder.move_to(points[0].0, points[0].1);
    for &point in &points[1..] {
        builder.line_to(point.0, point.1);
    }
    builder.close();
    builder.finish()
}

pub fn generate_assets(ico_path: &Path) -> Result<Vec<String>, String> {
    let parent = ico_path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    write_ico(ico_path)?;
    let preview_dir = parent.join("preview");
    create_previews(&preview_dir)
}

pub fn write_ico(path: &Path) -> Result<(), String> {
    const SIZES: [u32; 6] = [16, 20, 24, 32, 48, 256];
    let mut images = Vec::with_capacity(SIZES.len());
    for size in SIZES {
        images.push((size, render_icon(size, Some(100), false)));
    }

    let mut output = Vec::new();
    push_u16(&mut output, 0);
    push_u16(&mut output, 1);
    push_u16(&mut output, SIZES.len() as u16);

    let mut offset = 6 + 16 * SIZES.len() as u32;
    let mut payloads = Vec::with_capacity(SIZES.len());
    for (size, rgba) in &images {
        let payload = if *size >= 256 {
            png_bytes(*size, *size, rgba)?
        } else {
            dib_image(*size, rgba)
        };
        output.push(if *size >= 256 { 0 } else { *size as u8 });
        output.push(if *size >= 256 { 0 } else { *size as u8 });
        output.push(0);
        output.push(0);
        push_u16(&mut output, 1);
        push_u16(&mut output, 32);
        push_u32(&mut output, payload.len() as u32);
        push_u32(&mut output, offset);
        offset += payload.len() as u32;
        payloads.push(payload);
    }

    for payload in payloads {
        output.extend_from_slice(&payload);
    }
    fs::write(path, output).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn dib_image(size: u32, rgba: &[u8]) -> Vec<u8> {
    let row_bytes = size * 4;
    let mask_row = ((size + 31) / 32) * 4;
    let mask_bytes = mask_row * size;
    let pixel_bytes = row_bytes * size;
    let mut output = Vec::with_capacity(40 + (pixel_bytes + mask_bytes) as usize);

    push_u32(&mut output, 40);
    push_u32(&mut output, size);
    push_u32(&mut output, size * 2);
    push_u16(&mut output, 1);
    push_u16(&mut output, 32);
    push_u32(&mut output, 0);
    push_u32(&mut output, pixel_bytes + mask_bytes);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);
    push_u32(&mut output, 0);

    for y in (0..size).rev() {
        for x in 0..size {
            let offset = ((y * size + x) * 4) as usize;
            output.push(rgba[offset + 2]);
            output.push(rgba[offset + 1]);
            output.push(rgba[offset]);
            output.push(rgba[offset + 3]);
        }
    }
    output.resize(output.len() + mask_bytes as usize, 0);
    output
}

const PREVIEW_SIZES: [u32; 3] = [16, 20, 24];
const PREVIEW_PERCENTS: [Option<u8>; 6] = [
    Some(100),
    Some(60),
    Some(25),
    Some(10),
    Some(5),
    None,
];
const PREVIEW_DARK_BACKGROUND: [u8; 3] = [0x20, 0x20, 0x20];
const PREVIEW_LIGHT_BACKGROUND: [u8; 3] = [0xF0, 0xF0, 0xF0];
const PREVIEW_DARK_SEPARATOR: [u8; 3] = [0x34, 0x34, 0x34];
const PREVIEW_LIGHT_SEPARATOR: [u8; 3] = [0xD8, 0xD8, 0xD8];

fn create_previews(directory: &Path) -> Result<Vec<String>, String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    remove_old_previews(directory)?;

    let overview = directory.join("overview.png");
    write_overview(&overview)?;
    Ok(vec![overview.to_string_lossy().into_owned()])
}

fn remove_old_previews(directory: &Path) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not read {}: {error}", directory.display()))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if (name.starts_with("scale-") || name.starts_with("tray-")) && name.ends_with(".png") {
            fs::remove_file(&path)
                .map_err(|error| format!("could not remove {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

fn write_overview(path: &Path) -> Result<(), String> {
    const ZOOM: u32 = 4;
    const CELL_W: u32 = 112;
    const CELL_H: u32 = 116;
    const LABEL_W: u32 = 68;
    const HEADER_H: u32 = 28;
    const TEXT_SCALE: u32 = 2;

    let columns = PREVIEW_PERCENTS.len() as u32;
    let rows = PREVIEW_SIZES.len() as u32 * 2;
    let width = LABEL_W + CELL_W * columns;
    let height = HEADER_H + CELL_H * rows;

    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    fill_rect(
        &mut canvas,
        width,
        0,
        0,
        width,
        height,
        PREVIEW_DARK_BACKGROUND,
    );

    for (column, percent) in PREVIEW_PERCENTS.iter().enumerate() {
        let label = percent
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| String::from("NONE"));
        let text_width = label_width(&label, TEXT_SCALE);
        let x = LABEL_W + column as u32 * CELL_W + (CELL_W - text_width) / 2;
        draw_text(
            &mut canvas,
            width,
            x,
            (HEADER_H - 5 * TEXT_SCALE) / 2,
            &label,
            TEXT_SCALE,
            [0xE6, 0xE6, 0xE6],
        );
    }

    for (size_index, size) in PREVIEW_SIZES.iter().enumerate() {
        for (theme_index, light) in [false, true].into_iter().enumerate() {
            let row = size_index as u32 * 2 + theme_index as u32;
            let y = HEADER_H + row * CELL_H;
            let background = if light {
                PREVIEW_LIGHT_BACKGROUND
            } else {
                PREVIEW_DARK_BACKGROUND
            };
            let separator = if light {
                PREVIEW_LIGHT_SEPARATOR
            } else {
                PREVIEW_DARK_SEPARATOR
            };
            let text = if light {
                [0x1A, 0x1A, 0x1A]
            } else {
                [0xE6, 0xE6, 0xE6]
            };
            fill_rect(&mut canvas, width, 0, y, width, CELL_H, background);
            fill_rect(&mut canvas, width, 0, y, width, 1, separator);

            let label = format!("{size} {}", if light { "LIGHT" } else { "DARK" });
            draw_text(
                &mut canvas,
                width,
                4,
                y + (CELL_H - 5 * TEXT_SCALE) / 2,
                &label,
                TEXT_SCALE,
                text,
            );

            let icon_px = size * ZOOM;
            let icon_y = y + (CELL_H - icon_px) / 2;
            for (column, percent) in PREVIEW_PERCENTS.iter().enumerate() {
                let x = LABEL_W + column as u32 * CELL_W + (CELL_W - icon_px) / 2;
                if percent.is_none() {
                    fill_checker(
                        &mut canvas,
                        width,
                        x,
                        icon_y,
                        icon_px,
                        icon_px,
                        ZOOM * 2,
                    );
                    let rgba = render_icon(*size, *percent, light);
                    blit_scaled_blend(
                        &mut canvas,
                        width,
                        x,
                        icon_y,
                        &rgba,
                        *size,
                        *size,
                        ZOOM,
                    );
                } else {
                    let rgba = render_icon(*size, *percent, light);
                    let composited = composite(rgba, background);
                    blit_scaled(
                        &mut canvas,
                        width,
                        x,
                        icon_y,
                        &composited,
                        *size,
                        *size,
                        ZOOM,
                    );
                }
            }
        }
    }

    write_png(path, width, height, &canvas)
}

fn fill_rect(
    canvas: &mut [u8],
    width: u32,
    x: u32,
    y: u32,
    rect_width: u32,
    rect_height: u32,
    color: [u8; 3],
) {
    let height = canvas.len() as u32 / (width.max(1) * 4);
    let max_x = (x + rect_width).min(width);
    let max_y = (y + rect_height).min(height);
    for py in y..max_y {
        for px in x..max_x {
            let offset = ((py * width + px) * 4) as usize;
            if offset + 3 < canvas.len() {
                canvas[offset] = color[0];
                canvas[offset + 1] = color[1];
                canvas[offset + 2] = color[2];
                canvas[offset + 3] = 255;
            }
        }
    }
}

fn label_width(text: &str, scale: u32) -> u32 {
    let chars = text.chars().count() as u32;
    if chars == 0 {
        0
    } else {
        chars * 4 * scale - scale
    }
}

fn draw_text(
    canvas: &mut [u8],
    width: u32,
    x: u32,
    y: u32,
    text: &str,
    scale: u32,
    color: [u8; 3],
) {
    let mut cursor = x;
    for character in text.chars() {
        let Some(glyph) = glyph(character) else {
            cursor += 4 * scale;
            continue;
        };
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..3u32 {
                if (*bits & (0b100 >> column)) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = cursor + column * scale + dx;
                        let py = y + row as u32 * scale + dy;
                        if px < width {
                            let offset = ((py * width + px) * 4) as usize;
                            if offset + 3 < canvas.len() {
                                canvas[offset] = color[0];
                                canvas[offset + 1] = color[1];
                                canvas[offset + 2] = color[2];
                                canvas[offset + 3] = 255;
                            }
                        }
                    }
                }
            }
        }
        cursor += 4 * scale;
    }
}

fn glyph(character: char) -> Option<[u8; 5]> {
    Some(match character {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        'x' => [0b000, 0b101, 0b010, 0b101, 0b000],
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b111, 0b100, 0b111],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'N' => [0b101, 0b111, 0b101, 0b101, 0b101],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
fn fill_checker(
    canvas: &mut [u8],
    canvas_width: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    cell: u32,
) {
    const A: [u8; 3] = [0x8A, 0x8A, 0x8A];
    const B: [u8; 3] = [0xB4, 0xB4, 0xB4];
    for py in 0..height {
        for px in 0..width {
            let color = if ((px / cell) + (py / cell)) % 2 == 0 {
                A
            } else {
                B
            };
            let target = (((y + py) * canvas_width + x + px) * 4) as usize;
            if target + 3 < canvas.len() {
                canvas[target] = color[0];
                canvas[target + 1] = color[1];
                canvas[target + 2] = color[2];
                canvas[target + 3] = 255;
            }
        }
    }
}

fn blit_scaled_blend(
    canvas: &mut [u8],
    canvas_width: u32,
    x: u32,
    y: u32,
    source: &[u8],
    source_width: u32,
    source_height: u32,
    scale: u32,
) {
    for sy in 0..source_height {
        for sx in 0..source_width {
            let offset = ((sy * source_width + sx) * 4) as usize;
            let alpha = f32::from(source[offset + 3]) / 255.0;
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x + sx * scale + dx;
                    let py = y + sy * scale + dy;
                    if px >= canvas_width {
                        continue;
                    }
                    let target = ((py * canvas_width + px) * 4) as usize;
                    if target + 3 < canvas.len() {
                        for channel in 0..3 {
                            let value = f32::from(source[offset + channel]) * alpha
                                + f32::from(canvas[target + channel]) * (1.0 - alpha);
                            canvas[target + channel] =
                                value.round().clamp(0.0, 255.0) as u8;
                        }
                        canvas[target + 3] = 255;
                    }
                }
            }
        }
    }
}

fn blit_scaled(
    canvas: &mut [u8],
    canvas_width: u32,
    x: u32,
    y: u32,
    source: &[u8],
    source_width: u32,
    source_height: u32,
    scale: u32,
) {
    for sy in 0..source_height {
        for sx in 0..source_width {
            let offset = ((sy * source_width + sx) * 4) as usize;
            let color = [
                source[offset],
                source[offset + 1],
                source[offset + 2],
                source[offset + 3],
            ];
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = x + sx * scale + dx;
                    let py = y + sy * scale + dy;
                    if px >= canvas_width {
                        continue;
                    }
                    let target = ((py * canvas_width + px) * 4) as usize;
                    if target + 3 < canvas.len() {
                        canvas[target..target + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

fn composite(rgba: Vec<u8>, background: [u8; 3]) -> Vec<u8> {
    let mut output = vec![0u8; rgba.len()];
    for (source, target) in rgba.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
        let alpha = f32::from(source[3]) / 255.0;
        for channel in 0..3 {
            let value =
                f32::from(source[channel]) * alpha + f32::from(background[channel]) * (1.0 - alpha);
            target[channel] = value.round().clamp(0.0, 255.0) as u8;
        }
        target[3] = 255;
    }
    output
}

fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("could not encode icon PNG: {error}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| format!("could not encode icon PNG: {error}"))?;
    writer
        .finish()
        .map_err(|error| format!("could not encode icon PNG: {error}"))?;
    Ok(output)
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = File::create(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    let writer = BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    writer
        .finish()
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(())
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}
