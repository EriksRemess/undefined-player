//! Player-owned overlay images and placement. Native code only shapes Unicode
//! into natural-size masks and uploads the prepared images to Vulkan.
use crate::geometry::{SCRUBBER_MARGIN, TOP_BAR_HEIGHT};
use crate::{Result, ffi, pixel_font};
use std::borrow::Cow;
use std::ffi::CStr;

const WIDTH: usize = 1024;
const HEIGHT: usize = 704;
const SCALE: usize = 2;
const BASE_HEIGHT: usize = 7 * SCALE;
// Accents occupy their own rows so the 5x7 letter bodies never shrink.
const ACCENT_PAD: usize = 3 * SCALE + 1;
const GLYPH_HEIGHT: usize = BASE_HEIGHT + 2 * ACCENT_PAD;
const CELL: usize = 6 * SCALE;
const TITLE_HEIGHT: usize = GLYPH_HEIGHT + 2;
const INFO_Y: usize = 16;
const CLOSE_X: usize = WIDTH - 12;
const CLOSE_Y: usize = 48;
const POSITION_Y: usize = 80;
const DETAILS_Y: usize = 112;
// Maximum advance, for adjacent lines with accents below and above.
const LINE_ADVANCE: usize = GLYPH_HEIGHT + 2;
const MAX_LINES: usize = 16;
const METADATA_Y: usize = 608;
const INSET: f32 = 32.0;

struct PixelGlyph {
    base: u8,
    above: [u8; 3],
    below: [u8; 3],
}

impl PixelGlyph {
    fn for_character(character: char) -> Option<Self> {
        let mut glyph = Self {
            base: character as u8,
            above: [0; 3],
            below: [0; 3],
        };
        if character.is_ascii() {
            return Some(glyph);
        }
        let mut decomposition = [0; 4];
        // Canonical decomposition only; GLib writes at most capacity codepoints
        // and returns the full length. Unsupported combinations use Pango.
        let length = unsafe {
            ffi::up_text_decompose(
                character as u32,
                decomposition.as_mut_ptr(),
                decomposition.len(),
            )
        };
        if !(2..=decomposition.len()).contains(&length) {
            return None;
        }
        glyph.base = u8::try_from(decomposition[0]).ok()?;
        if !glyph.base.is_ascii_alphabetic() {
            return None;
        }
        for &mark in &decomposition[1..length] {
            let (below, rows) = match mark {
                0x0300 => (false, [0, 0b01000, 0b00100]),       // grave
                0x0301 => (false, [0, 0b00010, 0b00100]),       // acute
                0x0302 => (false, [0, 0b00100, 0b01010]),       // circumflex
                0x0303 => (false, [0, 0b01101, 0b10010]),       // tilde
                0x0304 => (false, [0, 0, 0b01110]),             // macron
                0x0306 => (false, [0, 0b10001, 0b01110]),       // breve
                0x0307 => (false, [0, 0, 0b00100]),             // dot above
                0x0308 => (false, [0, 0, 0b01010]),             // diaeresis
                0x030a => (false, [0b00100, 0b01010, 0b00100]), // ring
                0x030b => (false, [0, 0b01010, 0b10100]),       // double acute
                0x030c => (false, [0, 0b01010, 0b00100]),       // caron
                0x0323 => (true, [0b00100, 0, 0]),              // dot below
                0x0326 => (true, [0b00100, 0b01000, 0]),        // comma below
                // Latvian cedillas are displayed as commas in uppercase.
                0x0327
                    if matches!(
                        glyph.base.to_ascii_uppercase(),
                        b'G' | b'K' | b'L' | b'N' | b'R'
                    ) =>
                {
                    (true, [0b00100, 0b01000, 0])
                }
                0x0327 => (true, [0b00100, 0b00010, 0b00100]), // cedilla
                0x0328 => (true, [0b00010, 0b00100, 0b00010]), // ogonek
                _ => return None,
            };
            let destination = if below {
                &mut glyph.below
            } else {
                &mut glyph.above
            };
            if *destination != [0; 3] {
                return None; // Do not merge or silently discard stacked accents.
            }
            *destination = rows;
        }
        Some(glyph)
    }
}

fn normalized(text: &str) -> Result<Cow<'_, str>> {
    if text.is_ascii() {
        return Ok(Cow::Borrowed(text));
    }
    // GLib returns a separately allocated, NUL-terminated NFC string.
    let pointer = unsafe { ffi::up_text_normalize(text.as_ptr().cast(), text.len()) };
    if pointer.is_null() {
        return Err("could not normalize overlay text".into());
    }
    let result = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::up_text_free(pointer) };
    Ok(Cow::Owned(result))
}

struct NativeMask(ffi::UpTextMask);
impl NativeMask {
    fn new(text: &str) -> Result<Self> {
        let mut mask = Self(ffi::UpTextMask::default());
        // Pango reads this UTF-8 slice during the call and retains no Rust data.
        if !unsafe { ffi::up_text_mask_create(text.as_ptr().cast(), text.len(), &mut mask.0) } {
            return Err("could not rasterize overlay text".into());
        }
        Ok(mask)
    }

    fn view(&self) -> Option<Mask<'_>> {
        let width = usize::try_from(self.0.width).ok()?;
        let height = usize::try_from(self.0.height).ok()?;
        let stride = usize::try_from(self.0.stride).ok()?;
        let length = stride.checked_mul(height)?;
        if self.0.pixels.is_null() || width == 0 || height == 0 || stride < width {
            return None;
        }
        // The Cairo surface owns stride * height bytes and outlives this view.
        let pixels = unsafe { std::slice::from_raw_parts(self.0.pixels, length) };
        Some(Mask {
            pixels,
            width,
            height,
            stride,
        })
    }
}
impl Drop for NativeMask {
    fn drop(&mut self) {
        unsafe { ffi::up_text_mask_free(&mut self.0) };
    }
}

struct Mask<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
    stride: usize,
}

struct Image {
    pixels: Vec<u8>,
    height: usize,
    serial: u64,
}
impl Image {
    fn new(height: usize) -> Self {
        Self {
            pixels: vec![0; WIDTH * height],
            height,
            serial: 0,
        }
    }

    fn clear(&mut self) {
        self.pixels.fill(0);
    }

    fn glyph(&mut self, x: usize, y: usize, character: u8) {
        self.pixel_glyph(
            x,
            y,
            &PixelGlyph {
                base: character,
                above: [0; 3],
                below: [0; 3],
            },
        );
    }

    fn pixel_glyph(&mut self, x: usize, y: usize, glyph: &PixelGlyph) {
        if x > WIDTH - 5 * SCALE
            || y > self.height.saturating_sub(GLYPH_HEIGHT)
            || self.height < GLYPH_HEIGHT
        {
            return;
        }
        self.rows(x, y + ACCENT_PAD, &pixel_font::rows(glyph.base));
        self.rows(x, y, &glyph.above);
        self.rows(x, y + ACCENT_PAD + BASE_HEIGHT + 1, &glyph.below);
    }

    fn rows(&mut self, x: usize, y: usize, rows: &[u8]) {
        for (gy, row) in rows.iter().enumerate() {
            for gx in 0..5 {
                if row & (1 << (4 - gx)) == 0 {
                    continue;
                }
                for sy in 0..SCALE {
                    for sx in 0..SCALE {
                        self.pixels[(y + gy * SCALE + sy) * WIDTH + x + gx * SCALE + sx] = 255;
                    }
                }
            }
        }
    }

    // Fit all nonzero source pixels, including antialiased accent edges. Area
    // averaging preserves their contribution when shrinking to the fixed cell.
    fn fit_mask(&mut self, mut x: usize, mut y: usize, width: usize, mask: Mask<'_>) {
        if width == 0
            || width > WIDTH
            || x > WIDTH - width
            || self.height < BASE_HEIGHT
            || y > self.height - BASE_HEIGHT
        {
            return;
        }
        let (mut left, mut top, mut right, mut bottom) = (mask.width, mask.height, 0, 0);
        for sy in 0..mask.height {
            for sx in 0..mask.width {
                if mask.pixels[sy * mask.stride + sx] != 0 {
                    left = left.min(sx);
                    top = top.min(sy);
                    right = right.max(sx + 1);
                    bottom = bottom.max(sy + 1);
                }
            }
        }
        if right <= left || bottom <= top {
            return;
        }
        let scale = 1.0_f64
            .min(width as f64 / (right - left) as f64)
            .min(BASE_HEIGHT as f64 / (bottom - top) as f64);
        let fitted_width = (((right - left) as f64 * scale).floor() as usize).max(1);
        let fitted_height = (((bottom - top) as f64 * scale).floor() as usize).max(1);
        x += (width - fitted_width) / 2;
        y += (BASE_HEIGHT - fitted_height) / 2;
        let step_x = (right - left) as f64 / fitted_width as f64;
        let step_y = (bottom - top) as f64 / fitted_height as f64;
        for dy in 0..fitted_height {
            let y0 = top as f64 + dy as f64 * step_y;
            let y1 = top as f64 + (dy + 1) as f64 * step_y;
            for dx in 0..fitted_width {
                let x0 = left as f64 + dx as f64 * step_x;
                let x1 = left as f64 + (dx + 1) as f64 * step_x;
                let mut alpha = 0.0;
                for sy in y0.floor() as usize..bottom.min(y1.ceil() as usize) {
                    let coverage_y = y1.min((sy + 1) as f64) - y0.max(sy as f64);
                    for sx in x0.floor() as usize..right.min(x1.ceil() as usize) {
                        let coverage_x = x1.min((sx + 1) as f64) - x0.max(sx as f64);
                        alpha +=
                            f64::from(mask.pixels[sy * mask.stride + sx]) * coverage_x * coverage_y;
                    }
                }
                let value = if alpha > 0.0 {
                    (alpha / (step_x * step_y)).round().max(1.0) as u16
                } else {
                    0
                };
                let destination = &mut self.pixels[(y + dy) * WIDTH + x + dx];
                *destination =
                    (value + (u16::from(*destination) * (255 - value) + 127) / 255) as u8;
            }
        }
    }

    fn rasterize(&mut self, x: usize, y: usize, text: &str, maximum: usize) -> Result<usize> {
        if self.height < GLYPH_HEIGHT || y > self.height - GLYPH_HEIGHT {
            return Ok(0);
        }
        let mut characters = text
            .chars()
            .take(maximum.min(WIDTH.saturating_sub(x) / CELL))
            .peekable();
        let mut cells = 0;
        while let Some(character) = characters.next() {
            if let Some(glyph) = PixelGlyph::for_character(character) {
                self.pixel_glyph(x + cells * CELL, y, &glyph);
                cells += 1;
            } else {
                let mut run = String::from(character);
                let mut run_cells = 1;
                while characters
                    .peek()
                    .is_some_and(|c| PixelGlyph::for_character(*c).is_none())
                {
                    run.push(characters.next().unwrap());
                    run_cells += 1;
                }
                let mask = NativeMask::new(&run)?;
                if let Some(view) = mask.view() {
                    self.fit_mask(
                        x + cells * CELL,
                        y + ACCENT_PAD,
                        pixel_width(run_cells),
                        view,
                    );
                }
                cells += run_cells;
            }
        }
        Ok(cells)
    }

    fn text(&mut self, x: usize, y: usize, text: &str) -> Result<usize> {
        let text = normalized(text)?;
        self.rasterize(x, y, &text, WIDTH / CELL).map(pixel_width)
    }

    fn descriptor(&self) -> ffi::UpOverlayImage {
        ffi::UpOverlayImage {
            pixels: self.pixels.as_ptr(),
            width: WIDTH as i32,
            height: self.height as i32,
            serial: self.serial,
        }
    }
}

fn pixel_width(cells: usize) -> usize {
    (cells * CELL).saturating_sub(SCALE)
}

pub struct Content<'a> {
    pub title: &'a str,
    pub info: &'a str,
    pub details: &'a str,
    pub metadata: &'a str,
    pub position: &'a str,
}

pub struct Visibility<'a> {
    pub top_bar: f32,
    pub info: f32,
    pub position: f32,
    pub scrubber: Option<(f32, f32)>,
    pub chapter_markers: &'a [f32],
}

pub struct Overlays {
    title: Image,
    text: Image,
    title_key: Option<(String, usize)>,
    text_key: Option<(String, String, String)>,
    chapter_key: Option<(Vec<f32>, i32)>,
    metadata_key: Option<(String, usize)>,
    metadata_width: usize,
    metadata_height: usize,
    title_width: usize,
    info_width: usize,
    details_width: usize,
    details_height: usize,
    position_width: usize,
    parts: Vec<ffi::UpOverlayPart>,
}
impl Default for Overlays {
    fn default() -> Self {
        Self {
            title: Image::new(TITLE_HEIGHT),
            text: Image::new(HEIGHT),
            title_key: None,
            text_key: None,
            chapter_key: None,
            metadata_key: None,
            metadata_width: 0,
            metadata_height: 0,
            title_width: 0,
            info_width: 0,
            details_width: 0,
            details_height: 0,
            position_width: 0,
            parts: Vec::with_capacity(9),
        }
    }
}
impl Overlays {
    fn update(&mut self, content: Content<'_>, width: i32) -> Result<()> {
        let layout_width = width.saturating_sub((2.0 * TOP_BAR_HEIGHT) as i32).max(1) as usize;
        if !self
            .title_key
            .as_ref()
            .is_some_and(|(text, layout)| text == content.title && *layout == layout_width)
        {
            self.title.clear();
            let title = normalized(content.title)?;
            let maximum = ((layout_width + SCALE) / CELL).clamp(1, WIDTH / CELL);
            let ellipsis = if title.chars().count() > maximum {
                maximum.min(3)
            } else {
                0
            };
            let mut cells = self.title.rasterize(0, 1, &title, maximum - ellipsis)?;
            for _ in 0..ellipsis {
                self.title.glyph(cells * CELL, 1, b'.');
                cells += 1;
            }
            if !title.is_empty() && !self.title.pixels.iter().any(|p| *p != 0) {
                self.title.glyph(0, 1, b'?');
            }
            self.title_width = pixel_width(cells);
            self.title.serial += 1;
            self.title_key = Some((content.title.into(), layout_width));
        }
        if !self
            .text_key
            .as_ref()
            .is_some_and(|(info, details, position)| {
                info == content.info && details == content.details && position == content.position
            })
        {
            self.text.clear();
            self.chapter_key = None;
            self.metadata_key = None;
            self.info_width = self.text.text(0, INFO_Y, content.info)?;
            self.position_width = self.text.text(0, POSITION_Y, content.position)?;
            self.text.glyph(CLOSE_X, CLOSE_Y, b'X');
            self.details_width = 0;
            self.details_height = 0;
            let mut line_image = Image::new(GLYPH_HEIGHT);
            // split_terminator preserves empty interior lines, without creating
            // an extra line for the final newline (matching the visible panel).
            for (line, text) in content
                .details
                .split_terminator('\n')
                .take(MAX_LINES)
                .enumerate()
            {
                line_image.clear();
                let line_width = line_image.text(0, 0, text)?;
                self.details_width = self.details_width.max(line_width);
                let origin = line * LINE_ADVANCE;
                for row in 0..GLYPH_HEIGHT {
                    let source = row * WIDTH;
                    let destination = (DETAILS_Y + origin + row) * WIDTH;
                    self.text.pixels[destination..destination + line_width]
                        .copy_from_slice(&line_image.pixels[source..source + line_width]);
                }
                self.details_height = origin + GLYPH_HEIGHT;
            }
            self.text.serial += 1;
            self.text_key = Some((
                content.info.into(),
                content.details.into(),
                content.position.into(),
            ));
        }
        Ok(())
    }

    fn update_metadata(&mut self, label: &str, width: i32) -> Result<()> {
        let available = width.saturating_sub((2.0 * INSET) as i32).max(1) as usize;
        if self
            .metadata_key
            .as_ref()
            .is_some_and(|(old, old_width)| old == label && *old_width == available)
        {
            return Ok(());
        }
        self.metadata_width = 0;
        self.metadata_height = 0;
        // The text update has already cleared this region for an empty label.
        if label.is_empty() && self.metadata_key.is_none() {
            return Ok(());
        }
        self.text.pixels[METADATA_Y * WIDTH..].fill(0);
        let mut widths = Vec::new();
        for (line, text) in label.lines().take(3).enumerate() {
            let y = METADATA_Y + line * LINE_ADVANCE;
            let maximum = ((available + SCALE) / CELL).clamp(1, WIDTH / CELL);
            let text = normalized(text)?;
            let ellipsis = if text.chars().count() > maximum {
                maximum.min(3)
            } else {
                0
            };
            let mut cells = self.text.rasterize(0, y, &text, maximum - ellipsis)?;
            for _ in 0..ellipsis {
                self.text.glyph(cells * CELL, y, b'.');
                cells += 1;
            }
            let width = pixel_width(cells);
            self.metadata_width = self.metadata_width.max(width);
            self.metadata_height = line * LINE_ADVANCE + GLYPH_HEIGHT;
            widths.push(width);
        }
        for (line, width) in widths.into_iter().enumerate() {
            for y in
                METADATA_Y + line * LINE_ADVANCE..METADATA_Y + line * LINE_ADVANCE + GLYPH_HEIGHT
            {
                let row = &mut self.text.pixels[y * WIDTH..(y + 1) * WIDTH];
                let offset = self.metadata_width - width;
                row.copy_within(0..width, offset);
                row[..offset].fill(0);
            }
        }
        self.text.serial += 1;
        self.metadata_key = Some((label.into(), available));
        Ok(())
    }

    // All dots share one strip in the unused first twelve rows of the text
    // atlas. Chapter count therefore does not consume native overlay slots.
    fn update_chapters(&mut self, markers: &[f32], width: i32) {
        if markers.is_empty() && self.chapter_key.is_none() {
            return;
        }
        if self
            .chapter_key
            .as_ref()
            .is_some_and(|(old, old_width)| old == markers && *old_width == width)
        {
            return;
        }
        self.text.pixels[..WIDTH * 12].fill(0);
        let span = (width as f32 - 2.0 * SCRUBBER_MARGIN).max(0.0);
        let pixel_width = (span + 12.0) / WIDTH as f32;
        for &marker in markers
            .iter()
            .filter(|marker| marker.is_finite() && (0.0..=1.0).contains(*marker))
        {
            let center = 6.0 + marker * span;
            let left = ((center - 6.0) / pixel_width).max(0.0) as usize;
            let right = (((center + 6.0) / pixel_width).ceil() as usize).min(WIDTH);
            for y in 0..12 {
                for x in left..right {
                    let pixel_x = (x as f32 + 0.5) * pixel_width;
                    // The timeline supplies the middle rows. Painting them
                    // twice would brighten the translucent unplayed section.
                    if (4..8).contains(&y) && (6.0..span + 6.0).contains(&pixel_x) {
                        continue;
                    }
                    let dx = pixel_x - center;
                    let dy = y as f32 + 0.5 - 6.0;
                    let coverage = (5.5 - dx.hypot(dy)).clamp(0.0, 1.0);
                    let pixel = &mut self.text.pixels[y * WIDTH + x];
                    *pixel = (*pixel).max((coverage * 255.0).round() as u8);
                }
            }
        }
        self.text.serial += 1;
        self.chapter_key = Some((markers.to_vec(), width));
    }

    pub fn prepare(
        &mut self,
        content: Content<'_>,
        visibility: Visibility<'_>,
        width: i32,
        height: i32,
    ) -> Result<Prepared<'_>> {
        let chapter = content.metadata;
        self.update(content, width)?;
        self.update_metadata(chapter, width)?;
        self.update_chapters(visibility.chapter_markers, width);
        let (width, height) = (width as f32, height as f32);
        self.parts.clear();
        let parts = &mut self.parts;
        let mut add = |texture, src, dst, color| {
            parts.push(ffi::UpOverlayPart {
                texture,
                src,
                dst,
                color,
            })
        };
        let solid = [0.0, 0.0, 1.0, 1.0];
        let white = |alpha| [1.0, 1.0, 1.0, alpha];
        let top_alpha = visibility.top_bar.clamp(0.0, 1.0);
        if top_alpha > 0.001 {
            add(
                0,
                solid,
                [0.0, 0.0, width, TOP_BAR_HEIGHT],
                [0.02, 0.02, 0.02, 0.72 * top_alpha],
            );
            if self.title_width > 0 {
                let tw = self.title_width as f32;
                let x = ((width - tw) * 0.5).max(SCRUBBER_MARGIN);
                let y = (TOP_BAR_HEIGHT - TITLE_HEIGHT as f32) * 0.5;
                add(
                    2,
                    [0.0, 0.0, tw, TITLE_HEIGHT as f32],
                    [x, y, x + tw, y + TITLE_HEIGHT as f32],
                    white(top_alpha),
                );
            }
            add(
                1,
                [
                    CLOSE_X as f32,
                    (CLOSE_Y + ACCENT_PAD) as f32,
                    (CLOSE_X + 10) as f32,
                    (CLOSE_Y + ACCENT_PAD + BASE_HEIGHT) as f32,
                ],
                [width - 26.0, 14.0, width - 16.0, 28.0],
                white(top_alpha),
            );
        }
        let bottom = (height - INSET - (ACCENT_PAD + BASE_HEIGHT) as f32).max(0.0);
        let metadata_top = INSET - ACCENT_PAD as f32;
        let metadata_scale = if self.metadata_height > 0 {
            1.0_f32
                .min(((bottom - metadata_top - 12.0).max(0.0) * 0.5) / self.metadata_height as f32)
        } else {
            1.0
        };
        let metadata_width = self.metadata_width as f32 * metadata_scale;
        let metadata_height = self.metadata_height as f32 * metadata_scale;
        if self.details_width > 0 && self.details_height > 0 {
            let (dw, dh) = (self.details_width as f32, self.details_height as f32);
            let mut top = INSET - ACCENT_PAD as f32;
            if self.metadata_width > 0
                && INSET + dw.min((width - 2.0 * INSET).max(0.0)) + 16.0
                    > width - INSET - metadata_width
            {
                top = metadata_top + metadata_height + 8.0;
            }
            let available_width = (width - 2.0 * INSET).max(0.0);
            let available_height = (bottom - 4.0 - top).max(0.0);
            let scale = 1.0_f32.min(available_width / dw).min(available_height / dh);
            if scale > 0.0 {
                add(
                    1,
                    [0.0, DETAILS_Y as f32, dw, DETAILS_Y as f32 + dh],
                    [INSET, top, INSET + dw * scale, top + dh * scale],
                    white(1.0),
                );
            }
        }
        if self.metadata_width > 0 && metadata_scale > 0.0 {
            let x = (width - INSET - metadata_width).max(INSET);
            add(
                1,
                [
                    0.0,
                    METADATA_Y as f32,
                    self.metadata_width as f32,
                    (METADATA_Y + self.metadata_height) as f32,
                ],
                [
                    x,
                    metadata_top,
                    x + metadata_width,
                    metadata_top + metadata_height,
                ],
                white(1.0),
            );
        }
        let info_alpha = visibility.info.clamp(0.0, 1.0);
        if self.info_width > 0 && info_alpha > 0.001 {
            let iw = self.info_width as f32;
            add(
                1,
                [0.0, INFO_Y as f32, iw, (INFO_Y + GLYPH_HEIGHT) as f32],
                [INSET, bottom, INSET + iw, bottom + GLYPH_HEIGHT as f32],
                white(info_alpha),
            );
        }
        let position_alpha = visibility.position.clamp(0.0, 1.0);
        if self.position_width > 0 && position_alpha > 0.001 {
            let pw = self.position_width as f32;
            let x = (width - INSET - pw).max(INSET);
            add(
                1,
                [
                    0.0,
                    POSITION_Y as f32,
                    pw,
                    (POSITION_Y + GLYPH_HEIGHT) as f32,
                ],
                [x, bottom, x + pw, bottom + GLYPH_HEIGHT as f32],
                white(position_alpha),
            );
        }
        if let Some((progress, alpha)) = visibility.scrubber {
            let alpha = alpha.clamp(0.0, 1.0);
            if progress >= 0.0 && alpha > 0.001 {
                let left = SCRUBBER_MARGIN;
                let right = (width - SCRUBBER_MARGIN).max(left);
                let y = (height - 18.0).max(0.0);
                let x = left + (right - left) * progress.clamp(0.0, 1.0);
                add(
                    0,
                    solid,
                    [left, y - 2.0, right, y + 2.0],
                    white(0.35 * alpha),
                );
                if x > left {
                    add(
                        0,
                        solid,
                        [left, y - 2.0, x, y + 2.0],
                        [0.25, 0.70, 1.0, alpha],
                    );
                }
                if !visibility.chapter_markers.is_empty() {
                    let split = (x - left + 6.0) / (right - left + 12.0) * WIDTH as f32;
                    add(
                        1,
                        [0.0, 0.0, split, 12.0],
                        [left - 6.0, y - 6.0, x, y + 6.0],
                        [0.25, 0.70, 1.0, alpha],
                    );
                    add(
                        1,
                        [split, 0.0, WIDTH as f32, 12.0],
                        [x, y - 6.0, right + 6.0, y + 6.0],
                        white(0.35 * alpha),
                    );
                }
                add(0, solid, [x - 3.0, y - 6.0, x + 3.0, y + 6.0], white(alpha));
            }
        }
        Ok(Prepared {
            text: &self.text,
            title: &self.title,
            parts: &self.parts,
        })
    }
}

// Images and geometry cannot be changed or freed while a frame is borrowed.
pub struct Prepared<'a> {
    text: &'a Image,
    title: &'a Image,
    parts: &'a [ffi::UpOverlayPart],
}
impl Prepared<'_> {
    pub fn descriptor(&self) -> ffi::UpOverlayFrame {
        ffi::UpOverlayFrame {
            text: self.text.descriptor(),
            title: self.title.descriptor(),
            parts: self.parts.as_ptr(),
            count: self.parts.len(),
        }
    }
}

#[cfg(test)]
#[path = "overlay_tests.rs"]
mod tests;
