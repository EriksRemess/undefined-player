// Exercise the CPU rendering paths without a Wayland session or Vulkan GPU.
#include <cairo.h>
#include <stdbool.h>
static bool disable_glyph_clipping;
static inline void test_cairo_clip(cairo_t *context)
{
    if (disable_glyph_clipping)
        cairo_new_path(context);
    else
        cairo_clip(context);
}
#define cairo_clip test_cairo_clip
#include "video_renderer.c"
#undef cairo_clip
#include <assert.h>

static void unicode_overlays(void)
{
    uint8_t actual[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    uint8_t comparison[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    const char *text = "AUDIO: 1 / 2 - FRA - FRANÇAIS - AC3";
    const int width = draw_text(actual, 16, text);
    assert(width == text_pixel_width(text));
    assert(width == 35 * TITLE_CELL_WIDTH - GLYPH_SCALE);
    draw_text(comparison, 16, "AUDIO: 1 / 2 - FRA - FRAN?AIS - AC3");
    assert(memcmp(actual, comparison, sizeof actual) != 0);
    memset(comparison, 0, sizeof comparison);
    assert(draw_text(comparison, 16, "AUDIO: 1 / 2 - FRA - FRANC\314\247AIS - AC3") == width);
    assert(memcmp(actual, comparison, sizeof actual) == 0);

    memset(actual, 0, sizeof actual);
    memset(comparison, 0, sizeof comparison);
    const char *ascii = "AUDIO: 3 / 3 - ENG - STEREO - AC3";
    draw_text(actual, 16, ascii);
    for (size_t i = 0; i < strlen(ascii); i++)
        draw_glyph(comparison, TEXTURE_WIDTH, i * TITLE_CELL_WIDTH, 16, ascii[i]);
    assert(memcmp(actual, comparison, sizeof actual) == 0);

    char *long_text = g_strnfill(300, 'A');
    memcpy(long_text + 83, "日本語", strlen("日本語"));
    assert(draw_text(actual, 16, long_text) == text_pixel_width(long_text));
    assert(text_pixel_width(long_text) <= TEXTURE_WIDTH);
    g_free(long_text);
}

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

static void accented_glyph_bounds(void)
{
    const char *texts[] = {"Ç", "Š", "Ā", "É", "FRANÇAIS", "LATVIEŠU"};
    const int rows[] = {INFO_GLYPH_Y, POSITION_GLYPH_Y, DETAILS_GLYPH_Y};
    for (size_t i = 0; i < sizeof texts / sizeof *texts; i++) {
        for (size_t row = 0; row < sizeof rows / sizeof *rows; row++) {
            uint8_t pixels[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
            const int width = draw_text(pixels, rows[row], texts[i]);
            uint8_t unclipped[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
            disable_glyph_clipping = true;
            draw_text(unclipped, rows[row], texts[i]);
            disable_glyph_clipping = false;
            assert(memcmp(pixels, unclipped, sizeof pixels) == 0);
            int visible_pixels = 0;
            for (int y = 0; y < TEXTURE_HEIGHT; y++) {
                for (int x = 0; x < TEXTURE_WIDTH; x++) {
                    if (!pixels[y * TEXTURE_WIDTH + x])
                        continue;
                    assert(y >= rows[row] && y < rows[row] + 14);
                    assert(x < width);
                    visible_pixels++;
                }
            }
            assert(visible_pixels > 0);
        }
    }
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

static void glyph_mask_preserves_edges(void)
{
    uint8_t mask[10 * 20] = {0};
    mask[0] = mask[9] = mask[190] = mask[199] = 255;
    uint8_t pixels[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    fit_glyph_mask(pixels, TEXTURE_HEIGHT, 0, POSITION_GLYPH_Y, 10,
                   mask, 10, 20, 10);
    // The entire 10x20 mask becomes 7x14, including all four corner pixels.
    for (int y = 0; y < 14; y++) {
        for (int x = 0; x < 10; x++) {
            int alpha = pixels[(POSITION_GLYPH_Y + y) * TEXTURE_WIDTH + x];
            if ((y == 0 || y == 13) && (x == 1 || x == 7))
                assert(alpha >= 124 && alpha <= 126);
            else
                assert(alpha == 0);
        }
    }
}

int main(void)
{
    unicode_overlays();
    accented_glyph_bounds();
    glyph_mask_preserves_edges();
    invisible_subtitles();
    rotated_video();
    puts("Native renderer: Unicode overlays, invisible subtitles, and rotated video passed");
    return 0;
}
