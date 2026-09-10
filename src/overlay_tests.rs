use super::*;

fn rendered(text: &str) -> (Image, usize) {
    let mut image = Image::new(HEIGHT);
    let width = image.text(0, INFO_Y, text).unwrap();
    (image, width)
}

#[test]
fn printable_ascii_keeps_real_symbols() {
    let (question, _) = rendered("?");
    for character in b'!'..=b'~' {
        let text = char::from(character).to_string();
        let (image, width) = rendered(&text);
        assert_eq!(width, 10);
        assert!(image.pixels.iter().any(|p| *p != 0), "{text}");
        if character != b'?' {
            assert!(image.pixels != question.pixels, "{text} rendered as ?");
        }
    }
    let (space, width) = rendered(" ");
    assert_eq!(width, 10);
    assert!(space.pixels.iter().all(|p| *p == 0));
    assert_eq!(
        rendered("abcdefghijklmnopqrstuvwxyz").0.pixels,
        rendered("ABCDEFGHIJKLMNOPQRSTUVWXYZ").0.pixels
    );
}

#[test]
fn latin_accents_keep_ascii_letter_size_and_cell_spacing() {
    let plain = "GACEGLNORSZU";
    let accented = "GĀČĒĢĻŅŌŖŠŽŪ";
    let (reference, _) = rendered(plain);
    let (actual, width) = rendered(accented);
    assert_eq!(width, pixel_width(accented.chars().count()));
    for y in INFO_Y + ACCENT_PAD..INFO_Y + ACCENT_PAD + BASE_HEIGHT {
        assert_eq!(
            &actual.pixels[y * WIDTH..y * WIDTH + width],
            &reference.pixels[y * WIDTH..y * WIDTH + width],
            "accented letters must retain the ASCII body and spacing at row {y}"
        );
    }
    assert_eq!(actual.pixels, rendered("gāčēģļņōŗšžū").0.pixels);
    assert_eq!(actual.pixels, rendered("GA\u{304}C\u{30c}E\u{304}G\u{327}L\u{327}N\u{327}O\u{304}R\u{327}S\u{30c}Z\u{30c}U\u{304}").0.pixels);
    for (index, character) in accented.chars().enumerate().skip(1) {
        let below = matches!(character, 'Ģ' | 'Ļ' | 'Ņ' | 'Ŗ');
        let start = INFO_Y + if below { ACCENT_PAD + BASE_HEIGHT } else { 0 };
        assert!(
            (start..start + ACCENT_PAD).any(|y| {
                actual.pixels[y * WIDTH + index * CELL..y * WIDTH + index * CELL + 10]
                    .iter()
                    .any(|p| *p != 0)
            }),
            "{character} is missing its accent"
        );
    }
}

#[test]
fn latin_pixel_accents_do_not_swallow_adjacent_fallback_text() {
    for character in "ÉÈÊËÅÄÖÜÑÕĄĘǍĚŠŽŐŰḌ".chars() {
        let glyph = PixelGlyph::for_character(character).unwrap();
        let (actual, _) = rendered(&format!("日本語{character}🗼"));
        let (reference, _) = rendered(&char::from(glyph.base).to_string());
        for y in INFO_Y + ACCENT_PAD..INFO_Y + ACCENT_PAD + BASE_HEIGHT {
            assert_eq!(
                &actual.pixels[y * WIDTH + 3 * CELL..y * WIDTH + 3 * CELL + 10],
                &reference.pixels[y * WIDTH..y * WIDTH + 10],
                "{character}"
            );
        }
    }
    // Multiple marks on one side and non-Latin bases retain shaped fallback.
    for character in ['ḗ', 'ǜ', 'ΐ', '日'] {
        assert!(PixelGlyph::for_character(character).is_none());
        assert!(
            rendered(&character.to_string())
                .0
                .pixels
                .iter()
                .any(|p| *p != 0)
        );
    }
}

#[test]
fn unicode_and_decomposed_accents_are_preserved() {
    let (actual, width) = rendered("AUDIO: 1 / 2 - FRA - FRANÇAIS - AC3");
    assert_eq!(width, 35 * CELL - SCALE);
    assert_eq!(
        actual.pixels,
        rendered("AUDIO: 1 / 2 - FRA - FRANC\u{327}AIS - AC3")
            .0
            .pixels
    );
    assert!(actual.pixels != rendered("AUDIO: 1 / 2 - FRA - FRAN?AIS - AC3").0.pixels);
    for text in ["Ç", "Š", "Ā", "É", "FRANÇAIS", "LATVIEŠU", "日本語", "🗼"] {
        for row in [INFO_Y, POSITION_Y, DETAILS_Y] {
            let mut image = Image::new(HEIGHT);
            let width = image.text(0, row, text).unwrap();
            let mut visible = false;
            for (index, alpha) in image.pixels.iter().enumerate() {
                if *alpha != 0 {
                    visible = true;
                    assert!(
                        (row..row + GLYPH_HEIGHT).contains(&(index / WIDTH)),
                        "{text}"
                    );
                    assert!(index % WIDTH < width, "{text}");
                }
            }
            assert!(visible, "{text}");
        }
    }
}

#[test]
fn mask_scaling_keeps_all_four_edges() {
    let mut pixels = [0; 10 * 20];
    for corner in [0, 9, 190, 199] {
        pixels[corner] = 255;
    }
    let mut image = Image::new(HEIGHT);
    image.fit_mask(
        0,
        POSITION_Y,
        10,
        Mask {
            pixels: &pixels,
            width: 10,
            height: 20,
            stride: 10,
        },
    );
    for y in 0..14 {
        for x in 0..10 {
            let alpha = image.pixels[(POSITION_Y + y) * WIDTH + x];
            if (y == 0 || y == 13) && (x == 1 || x == 7) {
                assert!((124..=126).contains(&alpha));
            } else {
                assert_eq!(alpha, 0);
            }
        }
    }
}

fn content(title: &str) -> Content<'_> {
    Content {
        title,
        info: "",
        details: "",
        metadata: "",
        position: "",
    }
}

#[test]
fn title_truncation_and_invisible_names_are_safe() {
    let mut overlays = Overlays::default();
    let long = "日本語".repeat(300);
    for width in [1, 84, 96, 120, 1280, i32::MAX] {
        overlays.update(content(&long), width).unwrap();
        assert!(overlays.title_width <= WIDTH);
        assert!(overlays.title.pixels.iter().any(|p| *p != 0));
    }
    // 84 reserved pixels plus five cells: keep two letters and three dots.
    overlays
        .update(content("ABCDEF"), 84 + 5 * CELL as i32)
        .unwrap();
    let mut expected = Image::new(TITLE_HEIGHT);
    expected.text(0, 1, "AB...").unwrap();
    assert_eq!(overlays.title.pixels, expected.pixels);
    for invisible in ["\u{200b}", "\u{ad}", "   "] {
        overlays.update(content(invisible), 1280).unwrap();
        assert!(overlays.title.pixels.iter().any(|p| *p != 0));
    }
    overlays.update(content(""), 1280).unwrap();
    assert_eq!(overlays.title_width, 0);
    assert!(overlays.title.pixels.iter().all(|p| *p == 0));
}

#[test]
fn text_and_detail_buffers_are_bounded() {
    let mut image = Image::new(HEIGHT);
    let long = "A".repeat(83) + &"日本語".repeat(100);
    assert_eq!(
        image.text(0, INFO_Y, &long).unwrap(),
        pixel_width(WIDTH / CELL)
    );
    assert_eq!(image.text(WIDTH, INFO_Y, &long).unwrap(), 0);
    assert_eq!(image.text(usize::MAX, INFO_Y, &long).unwrap(), 0);
    image.glyph(usize::MAX, usize::MAX, b'A');
    let mut overlays = Overlays::default();
    overlays
        .update(
            Content {
                details: &"ÅÇ\n".repeat(100),
                ..content("")
            },
            1280,
        )
        .unwrap();
    assert_eq!(
        overlays.details_height,
        (MAX_LINES - 1) * LINE_ADVANCE + GLYPH_HEIGHT
    );
    assert!(DETAILS_Y + overlays.details_height <= HEIGHT);
    assert_eq!(overlays.details_width, pixel_width(2));
    let last_y = DETAILS_Y + (MAX_LINES - 1) * LINE_ADVANCE;
    let mut expected = Image::new(HEIGHT);
    expected.text(0, last_y, "ÅÇ").unwrap();
    assert_eq!(
        &overlays.text.pixels[last_y * WIDTH..],
        &expected.pixels[last_y * WIDTH..]
    );
}

#[test]
fn caches_track_content_and_title_width_without_truncating_keys() {
    let mut overlays = Overlays::default();
    let title = "A".repeat(600);
    overlays.update(content(&title), 1280).unwrap();
    let first = (overlays.text.serial, overlays.title.serial);
    overlays.update(content(&title), 1280).unwrap();
    assert_eq!(first, (overlays.text.serial, overlays.title.serial));
    overlays.update(content(&title), 640).unwrap();
    assert_eq!(overlays.text.serial, first.0);
    assert_eq!(overlays.title.serial, first.1 + 1);
    overlays
        .update(
            Content {
                position: "1:23",
                ..content(&title)
            },
            640,
        )
        .unwrap();
    assert_eq!(overlays.text.serial, first.0 + 1);
    overlays.update(content(&title), 640).unwrap();
    assert_eq!(overlays.position_width, 0);
    assert!(
        overlays.text.pixels[POSITION_Y * WIDTH..(POSITION_Y + GLYPH_HEIGHT) * WIDTH]
            .iter()
            .all(|p| *p == 0)
    );
}

#[test]
fn details_scale_to_fit_above_controls_in_a_small_window() {
    let details = [
        "CODEC: VP9",
        "RESOLUTION: 3840X2160",
        "BITRATE: 20 MBPS",
        "PIXEL FORMAT: YUV420P",
        "DECODE: VULKAN HW",
        "MATRIX: BT709",
        "PRIMARIES: BT709",
        "TRANSFER: BT709",
        "RANGE: TV",
        "HDR: NO",
        "AUDIO TRACK: 1 / 1",
        "AUDIO CODEC: OPUS",
        "AUDIO LANGUAGE: ENG",
        "AUDIO TITLE: GĀČĒĢĻŅŌŖŠŽŪ",
    ]
    .join("\n");
    let mut overlays = Overlays::default();
    let prepared = overlays
        .prepare(
            Content {
                details: &details,
                info: "FPS: 60",
                position: "0:01 / 1:00",
                ..content("TITLE")
            },
            Visibility {
                top_bar: 1.0,
                info: 1.0,
                position: 1.0,
                scrubber: None,
                chapter_markers: &[],
            },
            640,
            360,
        )
        .unwrap();
    let panel = &prepared.parts[3];
    assert!(panel.dst[3] + 2.0 <= prepared.parts[4].dst[1]);
    let scale_x = (panel.dst[2] - panel.dst[0]) / (panel.src[2] - panel.src[0]);
    let scale_y = (panel.dst[3] - panel.dst[1]) / (panel.src[3] - panel.src[1]);
    assert!((scale_x - scale_y).abs() < 0.00001);
    assert!(scale_x > 0.0 && scale_x < 1.0);
}

#[test]
fn details_use_even_spacing_and_preserve_accents() {
    let mut overlays = Overlays::default();
    overlays
        .update(
            Content {
                details: "A\nÅ\nÇ\nĀ\nG",
                ..content("")
            },
            640,
        )
        .unwrap();
    let mut expected = Image::new(HEIGHT);
    // Both info blocks use the same advance, including accented lines.
    for (line, text) in ["A", "Å", "Ç", "Ā", "G"].into_iter().enumerate() {
        let origin = line * LINE_ADVANCE;
        expected.text(0, DETAILS_Y + origin, text).unwrap();
    }
    assert_eq!(
        &overlays.text.pixels[DETAILS_Y * WIDTH..],
        &expected.pixels[DETAILS_Y * WIDTH..]
    );
    assert_eq!(overlays.details_height, 4 * LINE_ADVANCE + GLYPH_HEIGHT);
}

#[test]
fn oversized_details_scale_with_the_window_without_changing_cached_pixels() {
    let details = "AUDIO TITLE: GĀČĒĢĻŅŌŖŠŽŪ\n".repeat(MAX_LINES);
    let mut overlays = Overlays::default();
    for (width, height) in [(320, 180), (640, 240), (640, 360), (1280, 720)] {
        let prepared = overlays
            .prepare(
                Content {
                    details: &details,
                    info: "FPS: 60",
                    position: "0:01",
                    ..content("TITLE")
                },
                Visibility {
                    top_bar: 1.0,
                    info: 1.0,
                    position: 1.0,
                    scrubber: None,
                    chapter_markers: &[],
                },
                width,
                height,
            )
            .unwrap();
        let panel = &prepared.parts[3];
        assert!(panel.dst[2] <= width as f32 - INSET);
        assert!(panel.dst[3] + 2.0 <= prepared.parts[4].dst[1]);
        let scale_x = (panel.dst[2] - panel.dst[0]) / (panel.src[2] - panel.src[0]);
        let scale_y = (panel.dst[3] - panel.dst[1]) / (panel.src[3] - panel.src[1]);
        assert!((scale_x - scale_y).abs() < 0.00001);
        assert!(scale_x > 0.0 && scale_x <= 1.0);
        if height == 720 {
            assert_eq!(scale_x, 1.0);
        }
        assert_eq!(overlays.text.serial, 1);
    }
}

#[test]
fn overlay_geometry_matches_controls_and_pixel_buffers() {
    let mut overlays = Overlays::default();
    let parts = overlays
        .prepare(
            Content {
                title: "TITLE",
                info: "AUDIO",
                details: "VIDEO\nHDR",
                metadata: "",
                position: "1:23",
            },
            Visibility {
                top_bar: 1.0,
                info: 0.5,
                position: 1.0,
                scrubber: Some((0.5, 1.0)),
                chapter_markers: &[],
            },
            1280,
            720,
        )
        .unwrap();
    assert_eq!(parts.parts.len(), 9);
    for part in parts.parts {
        let (width, height) = match part.texture {
            0 => (1, 1),
            1 => (WIDTH, HEIGHT),
            2 => (WIDTH, TITLE_HEIGHT),
            _ => panic!("invalid texture"),
        };
        assert!(part.src[0] >= 0.0 && part.src[1] >= 0.0);
        assert!(part.src[2] <= width as f32 && part.src[3] <= height as f32);
        assert!(part.dst.iter().all(|v| v.is_finite()));
        if part.texture != 0 {
            assert_eq!(part.src[2] - part.src[0], part.dst[2] - part.dst[0]);
            assert_eq!(part.src[3] - part.src[1], part.dst[3] - part.dst[1]);
        }
    }
    assert_eq!(parts.parts[1].dst[1] + 1.0 + ACCENT_PAD as f32, 14.0);
    assert_eq!(parts.parts[3].dst[1] + ACCENT_PAD as f32, INSET);
    assert_eq!(parts.parts[4].dst[1] + ACCENT_PAD as f32, 674.0);
    assert_eq!(parts.parts[5].dst[1] + ACCENT_PAD as f32, 674.0);
    assert_eq!(parts.parts[2].dst, [1254.0, 14.0, 1264.0, 28.0]);
    assert_eq!(
        parts.parts.last().unwrap().dst,
        [637.0, 696.0, 643.0, 708.0]
    );
    // Initial empty strings still prepare the close button; hiding everything
    // must submit no geometry while retaining cached images for the next frame.
    let mut empty = Overlays::default();
    let parts = empty
        .prepare(
            content(""),
            Visibility {
                top_bar: 1.0,
                info: 0.0,
                position: 0.0,
                scrubber: None,
                chapter_markers: &[],
            },
            1280,
            720,
        )
        .unwrap();
    assert_eq!(parts.parts.len(), 2);
    assert!(empty.text.pixels.iter().any(|p| *p != 0));
    assert!(
        empty
            .prepare(
                content(""),
                Visibility {
                    top_bar: 0.0,
                    info: 0.0,
                    position: 0.0,
                    scrubber: None,
                    chapter_markers: &[],
                },
                1280,
                720
            )
            .unwrap()
            .parts
            .is_empty()
    );
}

#[test]
fn chapter_dots_are_bounded_cached_and_follow_the_timeline() {
    let mut overlays = Overlays::default();
    let markers: Vec<_> = (0..100).map(|i| i as f32 / 100.0).collect();
    for width in [320, 1280, 3840] {
        let visibility = || Visibility {
            top_bar: 0.0,
            info: 0.0,
            position: 0.0,
            scrubber: Some((0.5, 0.75)),
            chapter_markers: &markers,
        };
        let prepared = overlays
            .prepare(content(""), visibility(), width, 720)
            .unwrap();
        // Two colored parts for all chapters, plus track, progress and playhead.
        assert_eq!(prepared.parts.len(), 5);
        let dots = &prepared.parts[2];
        assert_eq!(dots.texture, 1);
        assert_eq!(
            dots.dst,
            [SCRUBBER_MARGIN - 6.0, 696.0, width as f32 * 0.5, 708.0]
        );
        assert_eq!(dots.color, prepared.parts[1].color);
        let remaining = &prepared.parts[3];
        assert_eq!(remaining.color, prepared.parts[0].color);
        assert_eq!(remaining.dst[0], dots.dst[2]);
        assert_eq!(remaining.dst[2], width as f32 - SCRUBBER_MARGIN + 6.0);
        assert_eq!(remaining.src[0], dots.src[2]);
        let pixels = &prepared.text.pixels[..WIDTH * 12];
        for marker in &markers {
            let center = 6.0 + marker * (width as f32 - 2.0 * SCRUBBER_MARGIN);
            let x =
                (center / (width as f32 - 2.0 * SCRUBBER_MARGIN + 12.0) * WIDTH as f32) as usize;
            assert!(pixels[2 * WIDTH + x] > 0);
            if *marker > 0.0 {
                assert_eq!(pixels[5 * WIDTH + x], 0);
            }
        }
        let serial = prepared.text.serial;
        assert_eq!(
            overlays
                .prepare(content(""), visibility(), width, 720)
                .unwrap()
                .text
                .serial,
            serial
        );
        // Changing text clears the atlas; markers must be restored too.
        let prepared = overlays
            .prepare(
                Content {
                    position: "0:01",
                    ..content("")
                },
                visibility(),
                width,
                720,
            )
            .unwrap();
        assert!(
            prepared.text.pixels[..WIDTH * 12]
                .iter()
                .any(|pixel| *pixel > 0)
        );
    }
    let prepared = overlays
        .prepare(
            content(""),
            Visibility {
                top_bar: 0.0,
                info: 0.0,
                position: 0.0,
                scrubber: Some((0.5, 0.0)),
                chapter_markers: &markers,
            },
            1280,
            720,
        )
        .unwrap();
    assert!(prepared.parts.is_empty());
}

#[test]
fn chapter_info_is_top_right_and_does_not_overlap_video_details() {
    let mut overlays = Overlays::default();
    for (width, height) in [(320, 180), (320, 720), (1280, 720), (1920, 1080)] {
        let label = "TITLE: SEVEN SAMURAI\nARTIST: AKIRA KUROSAWA\nCHAPTER 3 / 29 — SHOPPING FOR SAMURAI GĀČĒ";
        let content = || Content {
            details: "CODEC: H264\nRESOLUTION: 1436X1080",
            metadata: label,
            ..content("")
        };
        let visibility = || Visibility {
            top_bar: 0.0,
            info: 0.0,
            position: 0.0,
            scrubber: None,
            chapter_markers: &[],
        };
        let prepared = overlays
            .prepare(content(), visibility(), width, height)
            .unwrap();
        assert_eq!(prepared.parts.len(), 2);
        let details = &prepared.parts[0];
        let chapter = &prepared.parts[1];
        assert_eq!(chapter.dst[2], width as f32 - INSET);
        assert_eq!(chapter.dst[1], INSET - ACCENT_PAD as f32);
        assert!(chapter.dst[0] >= INSET);
        assert!(details.dst[2] + 16.0 <= chapter.dst[0] || details.dst[1] >= chapter.dst[3] + 8.0);
        assert!(
            prepared.text.pixels[METADATA_Y * WIDTH..]
                .iter()
                .any(|p| *p > 0)
        );
        let serial = prepared.text.serial;
        assert_eq!(
            overlays
                .prepare(content(), visibility(), width, height)
                .unwrap()
                .text
                .serial,
            serial
        );
        let prepared = overlays
            .prepare(
                Content {
                    metadata: "",
                    ..content()
                },
                visibility(),
                width,
                height,
            )
            .unwrap();
        assert_eq!(prepared.parts.len(), 1);
        assert!(
            prepared.text.pixels[METADATA_Y * WIDTH..]
                .iter()
                .all(|p| *p == 0)
        );
    }
}

#[test]
fn info_corners_share_the_same_inset_and_metadata_rows_align_right() {
    let mut overlays = Overlays::default();
    let prepared = overlays
        .prepare(
            Content {
                title: "",
                details: "CODEC: H264",
                info: "FPS: 24",
                position: "0:01",
                metadata: "TITLE: SEVEN SAMURAI\nARTIST: AKIRA KUROSAWA\nCHAPTER 2 / 29",
            },
            Visibility {
                top_bar: 0.0,
                info: 1.0,
                position: 1.0,
                scrubber: None,
                chapter_markers: &[],
            },
            1280,
            720,
        )
        .unwrap();
    assert_eq!(prepared.parts.len(), 4);
    let [details, metadata, stats, position] = prepared.parts else {
        unreachable!()
    };
    assert_eq!(details.dst[0], INSET);
    assert_eq!(metadata.dst[2], 1280.0 - INSET);
    assert_eq!(details.dst[1], metadata.dst[1]);
    assert_eq!(details.dst[1] + ACCENT_PAD as f32, INSET);
    assert_eq!(stats.dst[0], INSET);
    assert_eq!(position.dst[2], 1280.0 - INSET);
    assert_eq!(stats.dst[1], position.dst[1]);
    assert_eq!(
        stats.dst[1] + (ACCENT_PAD + BASE_HEIGHT) as f32,
        720.0 - INSET
    );
    let width = (metadata.src[2] - metadata.src[0]) as usize;
    for line in 0..3 {
        let rows = &prepared.text.pixels[(METADATA_Y + line * LINE_ADVANCE) * WIDTH
            ..(METADATA_Y + line * LINE_ADVANCE + GLYPH_HEIGHT) * WIDTH];
        let right = rows
            .as_chunks::<WIDTH>()
            .0
            .iter()
            .flat_map(|row| {
                row.iter()
                    .enumerate()
                    .filter(|(_, p)| **p > 0)
                    .map(|(x, _)| x)
            })
            .max()
            .unwrap();
        // Narrow glyphs such as I retain their normal side bearing.
        assert!(right < width && width - (right + 1) <= SCALE);
    }
}
