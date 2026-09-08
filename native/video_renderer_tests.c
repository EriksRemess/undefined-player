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

int main(void)
{
    invisible_subtitles();
    rotated_video();
    integer_scaled_video();
    puts("Native renderer: invisible subtitles, rotation, and integer scaling passed");
    return 0;
}
