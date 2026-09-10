// Native library integration checks; pixel rendering is tested in Rust.
#include "video_renderer.c"
#include <assert.h>

static void invisible_subtitles(void)
{
    UpVideoRenderer renderer = {.text_subtitle_visible = true};
    // No GPU is present: invisible cues must clear visibility without uploads.
    assert(update_text_subtitle_texture(&renderer, "\342\200\213", 640, 1));
    assert(!renderer.text_subtitle_visible);
    assert(renderer.text_subtitle_serial == 1);
    assert(update_text_subtitle_texture(&renderer, "\342\200\213", 640, 1));
    assert(!renderer.text_subtitle_visible);
    renderer.text_subtitle_visible = true;
    assert(update_text_subtitle_texture(&renderer, "\302\255", 320, 2));
    assert(!renderer.text_subtitle_visible);
}

static void rotated_video(void)
{
    for (int rotation = 0; rotation < 4; rotation++) {
        struct pl_frame image = {
            .crop = {.x1 = 320, .y1 = 180}, .rotation = rotation,
        };
        for (int sar = 1; sar <= 2; sar++) {
            pl_rect2df rect = fitted_video_rect(&image, (AVRational){sar, 1}, 1920, 1080, 0);
            double expected = rotation % 2 ? 180.0 / (320 * sar) : 320.0 * sar / 180;
            assert(fabs(pl_rect2df_aspect(&rect) - expected) < 0.00001);
            assert(fabs(rect.x0 + rect.x1 - 1920) < 0.001);
            assert(fabs(rect.y0 + rect.y1 - 1080) < 0.001);
            assert(rect.x0 >= 0 && rect.y0 >= 0 && rect.x1 <= 1920 && rect.y1 <= 1080);
        }
    }
}

static void integer_scaled_video(void)
{
    struct pl_frame image = {.crop = {.x1 = 248, .y1 = 248}};
    pl_rect2df rect = fitted_video_rect(&image, (AVRational){1, 1}, 1280, 720, 2);
    assert(rect.x0 == 392 && rect.y0 == 112 && rect.x1 == 888 && rect.y1 == 608);
    rect = fitted_video_rect(&image, (AVRational){1, 1}, 497, 497, 2);
    assert(rect.x0 == 0 && rect.y0 == 0 && rect.x1 == 496 && rect.y1 == 496);
    image.crop = (pl_rect2df) {.x1 = 320, .y1 = 180};
    for (int rotation = 0; rotation < 4; rotation++) {
        image.rotation = rotation;
        rect = fitted_video_rect(&image, (AVRational){1, 1}, 1281, 721, 2);
        assert(rect.x1 - rect.x0 == (rotation % 2 ? 360 : 640));
        assert(rect.y1 - rect.y0 == (rotation % 2 ? 640 : 360));
        assert(rect.x0 == floor(rect.x0) && rect.y0 == floor(rect.y0));
        assert(fabs(rect.x0 + rect.x1 - 1281) <= 1);
        assert(fabs(rect.y0 + rect.y1 - 721) <= 1);
    }
}

static void deinterlacing(void)
{
    const struct pl_fmt_t format = {.num_components = 1};
    struct pl_tex_t texture = {.params = {.w = 720, .h = 480, .format = &format}};
    struct pl_frame image = {
        .num_planes = 1,
        .planes = {{.texture = &texture, .components = 1}},
    };
    struct pl_frame prev = image, next = image;
    struct pl_render_params params = pl_render_default_params;
    for (int field = 1; field <= 2; field++) {
        configure_deinterlace(&image, &params, field);
        assert(image.field == (field == 1 ? PL_FIELD_TOP : PL_FIELD_BOTTOM));
        assert(image.first_field == image.field);
        assert(params.deinterlace_params->algo == PL_DEINTERLACE_BOB);
    }
    image.prev = &prev;
    image.next = &next;
    configure_deinterlace(&image, &params, 1);
    assert(params.deinterlace_params->algo == PL_DEINTERLACE_YADIF);
    configure_deinterlace(&image, &params, 0);
    assert(image.field == PL_FIELD_NONE && !params.deinterlace_params);
    AVFrame frame = {.width = 720, .height = 480, .format = AV_PIX_FMT_YUV420P};
    AVFrame reference = frame;
    assert(compatible_reference(&frame, &reference));
    reference.width = 640;
    assert(!compatible_reference(&frame, &reference));
    reference = frame;
    reference.format = AV_PIX_FMT_YUV420P10LE;
    assert(!compatible_reference(&frame, &reference));
    assert(!compatible_reference(&frame, NULL));
}

static void deinterlacing_reference_layout_changes(void)
{
    const struct pl_fmt_t format = {.num_components = 1};
    const struct pl_fmt_t other_format = {.num_components = 2};
    struct pl_tex_t luma = {.params = {.w = 720, .h = 480, .format = &format}};
    struct pl_tex_t chroma = {.params = {.w = 360, .h = 240, .format = &format}};
    const struct pl_frame source = {
        .num_planes = 3,
        .planes = {
            {.texture = &luma, .components = 1, .component_mapping = {0}},
            {.texture = &chroma, .components = 1, .component_mapping = {1}},
            {.texture = &chroma, .components = 1, .component_mapping = {2}},
        },
    };
    // Exercise either neighbor. Matching visible dimensions are insufficient
    // when a hardware decoder changes allocation size or plane layout.
    for (int side = 0; side < 2; side++) {
        for (int change = 0; change < 9; change++) {
            struct pl_frame bad = source, image = source;
            struct pl_tex_t changed = chroma;
            bad.planes[1].texture = &changed;
            switch (change) {
            case 0: changed.params.w += 16; break;
            case 1: changed.params.h += 16; break;
            case 2: changed.params.format = &other_format; break;
            case 3: bad.num_planes = 2; break;
            case 4: bad.planes[1].component_mapping[0] = 2; break;
            case 5: bad.repr.bits.bit_shift = 6; break;
            case 6: bad.planes[1].flipped = true; break;
            case 7: bad.planes[1].shift_x = 0.5f; break;
            case 8: bad.planes[1].texture = NULL; break;
            }
            image.prev = side == 0 ? &bad : &source;
            image.next = side == 1 ? &bad : &source;
            struct pl_render_params params = pl_render_default_params;
            configure_deinterlace(&image, &params, 1);
            assert(params.deinterlace_params->algo == PL_DEINTERLACE_BOB);
            assert(side == 0 ? !image.prev && image.next == &source :
                              !image.next && image.prev == &source);
            // A subsequent compatible pair must restore temporal filtering.
            image.prev = image.next = &source;
            configure_deinterlace(&image, &params, 1);
            assert(params.deinterlace_params->algo == PL_DEINTERLACE_YADIF);
        }
    }
}

int main(void)
{
    struct pl_frame cropped = {.crop = {.x1 = 720, .y1 = 480}};
    const UpVideoCrop crop = {720,480,0,112,720,372};
    apply_video_crop(&cropped,720,480,&crop);
    assert(cropped.crop.y0 == 112 && cropped.crop.y1 == 372);
    pl_rect2df rect = fitted_video_rect(&cropped,(AVRational){8,9},1920,1080,0);
    assert(fabs(pl_rect2df_aspect(&rect) - 640.0 / 260) < 0.00001);
    cropped.rotation = 1;
    rect = fitted_video_rect(&cropped,(AVRational){8,9},1920,1080,0);
    assert(fabs(pl_rect2df_aspect(&rect) - 260.0 / 640) < 0.00001);
    cropped.crop = (pl_rect2df) {.x1 = 1920,.y1 = 1080};
    apply_video_crop(&cropped,1920,1080,&crop);
    assert(cropped.crop.y0 == 0 && cropped.crop.y1 == 1080);
    deinterlacing();
    deinterlacing_reference_layout_changes();
    invisible_subtitles();
    rotated_video();
    integer_scaled_video();
    puts("Native renderer: invisible subtitles, rotation, integer scaling, and deinterlacing passed");
    return 0;
}
