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
            pl_rect2df rect = fitted_video_rect(&image, (AVRational){sar, 1}, 1920, 1080);
            double expected = rotation % 2 ? 180.0 / (320 * sar) : 320.0 * sar / 180;
            assert(fabs(pl_rect2df_aspect(&rect) - expected) < 0.00001);
            assert(fabs(rect.x0 + rect.x1 - 1920) < 0.001);
            assert(fabs(rect.y0 + rect.y1 - 1080) < 0.001);
            assert(rect.x0 >= 0 && rect.y0 >= 0 && rect.x1 <= 1920 && rect.y1 <= 1080);
        }
    }
}

int main(void)
{
    invisible_subtitles();
    rotated_video();
    puts("Native renderer: invisible subtitles and rotated video passed");
    return 0;
}
