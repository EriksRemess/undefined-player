#define VK_NO_PROTOTYPES
#define VK_ENABLE_BETA_EXTENSIONS
#define PL_LIBAV_IMPLEMENTATION 1

#include "video_renderer.h"

#include <SDL3/SDL_vulkan.h>
#include <libplacebo/renderer.h>
#include <libplacebo/swapchain.h>
#include <libplacebo/utils/libav.h>
#include <libplacebo/vulkan.h>
#include <pango/pangocairo.h>

#include <libavutil/dict.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vulkan.h>
#include <libavutil/mem.h>

#if LIBAVUTIL_VERSION_MAJOR < 60
#error "undefined-player requires FFmpeg 8 or newer development headers"
#endif

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct UpVideoRenderer {
    pl_log log;
    pl_vulkan vulkan;
    pl_swapchain swapchain;
    pl_renderer renderer;
    pl_tex textures[4];
    pl_tex solid_texture;
    pl_tex text_texture;
    pl_tex title_texture;
    pl_tex subtitle_texture;
    pl_tex text_subtitle_texture;
    int title_width;
    int title_height;
    int title_layout_width;
    int subtitle_width;
    int subtitle_height;
    uint64_t subtitle_serial;
    int text_subtitle_width;
    int text_subtitle_height;
    int text_subtitle_layout_width;
    uint64_t text_subtitle_serial;

    AVBufferRef *hw_device;
    PFN_vkGetInstanceProcAddr get_proc_addr;
    VkInstance instance;
    VkSurfaceKHR surface;

    char error[256];
    char cached_title[512];
    char cached_info[256];
    char cached_details[512];
    char cached_position[128];
};

#define TEXTURE_WIDTH 1024
#define TEXTURE_HEIGHT 384
#define GLYPH_SCALE 2
#define INFO_GLYPH_Y 16
#define CLOSE_GLYPH_X (TEXTURE_WIDTH - 12)
#define CLOSE_GLYPH_Y 32
#define POSITION_GLYPH_Y 48
#define DETAILS_GLYPH_Y 64
#define DETAILS_LINE_ADVANCE 18
#define DETAILS_MAX_LINES 16
#define INFO_TEXT_INSET 32.0f
#define TITLE_TEXTURE_HEIGHT 16
#define TITLE_CELL_WIDTH (6 * GLYPH_SCALE)

static void set_error(UpVideoRenderer *renderer, const char *message);

static size_t bounded_length(const char *text, size_t maximum)
{
    size_t length = 0;
    while (length < maximum && text[length])
        length++;
    return length;
}

static int text_pixel_width_length(size_t length)
{
    const size_t maximum = TEXTURE_WIDTH / (6 * GLYPH_SCALE);
    if (length > maximum)
        length = maximum;
    return length ? (int) (length * 6 * GLYPH_SCALE - GLYPH_SCALE) : 0;
}

static int text_pixel_width(const char *text)
{
    const size_t maximum = TEXTURE_WIDTH / (6 * GLYPH_SCALE);
    return text_pixel_width_length(bounded_length(text, maximum));
}

static void details_text_metrics(const char *text, int *width, int *height)
{
    int lines = 0;
    *width = 0;
    while (text && *text && lines < DETAILS_MAX_LINES) {
        const char *newline = strchr(text, '\n');
        const size_t length = newline ? (size_t) (newline - text) : strlen(text);
        *width = fmax(*width, text_pixel_width_length(length));
        lines++;
        if (!newline)
            break;
        text = newline + 1;
    }
    *height = lines ? (lines - 1) * DETAILS_LINE_ADVANCE + 14 : 0;
}

static const uint8_t *glyph_rows(char character)
{
    static const uint8_t blank[7] = {0};
    static const uint8_t digits[10][7] = {
        {14, 17, 19, 21, 25, 17, 14}, {4, 12, 4, 4, 4, 4, 14},
        {14, 17, 1, 2, 4, 8, 31},     {30, 1, 1, 14, 1, 1, 30},
        {2, 6, 10, 18, 31, 2, 2},     {31, 16, 16, 30, 1, 1, 30},
        {14, 16, 16, 30, 17, 17, 14}, {31, 1, 2, 4, 8, 8, 8},
        {14, 17, 17, 14, 17, 17, 14}, {14, 17, 17, 15, 1, 1, 14},
    };
    static const uint8_t letters[26][7] = {
        {14,17,17,31,17,17,17}, {30,17,17,30,17,17,30},
        {14,17,16,16,16,17,14}, {30,17,17,17,17,17,30},
        {31,16,16,30,16,16,31}, {31,16,16,30,16,16,16},
        {14,17,16,23,17,17,15}, {17,17,17,31,17,17,17},
        {14,4,4,4,4,4,14},      {7,2,2,2,2,18,12},
        {17,18,20,24,20,18,17}, {16,16,16,16,16,16,31},
        {17,27,21,21,17,17,17}, {17,25,21,19,17,17,17},
        {14,17,17,17,17,17,14}, {30,17,17,30,16,16,16},
        {14,17,17,17,21,18,13}, {30,17,17,30,20,18,17},
        {15,16,16,14,1,1,30},   {31,4,4,4,4,4,4},
        {17,17,17,17,17,17,14}, {17,17,17,17,17,10,4},
        {17,17,17,21,21,21,10}, {17,17,10,4,10,17,17},
        {17,17,10,4,4,4,4},     {31,1,2,4,8,16,31},
    };
    static const uint8_t colon[7] = {0, 4, 4, 0, 4, 4, 0};
    static const uint8_t dot[7] = {0, 0, 0, 0, 0, 6, 6};
    static const uint8_t dash[7] = {0, 0, 0, 31, 0, 0, 0};
    static const uint8_t slash[7] = {1, 2, 2, 4, 8, 8, 16};
    static const uint8_t underscore[7] = {0, 0, 0, 0, 0, 0, 31};
    static const uint8_t comma[7] = {0, 0, 0, 0, 0, 6, 4};
    static const uint8_t apostrophe[7] = {6, 4, 8, 0, 0, 0, 0};
    static const uint8_t quote[7] = {10, 10, 20, 0, 0, 0, 0};
    static const uint8_t exclamation[7] = {4, 4, 4, 4, 4, 0, 4};
    static const uint8_t question[7] = {14, 17, 1, 2, 4, 0, 4};
    static const uint8_t left_paren[7] = {2, 4, 8, 8, 8, 4, 2};
    static const uint8_t right_paren[7] = {8, 4, 2, 2, 2, 4, 8};
    static const uint8_t ampersand[7] = {12, 18, 20, 8, 21, 18, 13};

    if (character >= '0' && character <= '9')
        return digits[character - '0'];
    if (character >= 'a' && character <= 'z')
        character -= 'a' - 'A';
    if (character >= 'A' && character <= 'Z')
        return letters[character - 'A'];
    if (character == ':') return colon;
    if (character == '.') return dot;
    if (character == '-') return dash;
    if (character == '/') return slash;
    if (character == '_') return underscore;
    if (character == ',') return comma;
    if (character == '\'') return apostrophe;
    if (character == '"') return quote;
    if (character == '!') return exclamation;
    if (character == '?') return question;
    if (character == '(') return left_paren;
    if (character == ')') return right_paren;
    if (character == '&') return ampersand;
    return blank;
}

static void draw_glyph(uint8_t *pixels, int stride, int x, int y,
                       char character)
{
    const uint8_t *rows = glyph_rows(character);
    for (int gy = 0; gy < 7; gy++) {
        for (int gx = 0; gx < 5; gx++) {
            if (!(rows[gy] & (1 << (4 - gx))))
                continue;
            for (int sy = 0; sy < GLYPH_SCALE; sy++)
                for (int sx = 0; sx < GLYPH_SCALE; sx++)
                    pixels[(y + gy * GLYPH_SCALE + sy) * stride +
                           x + gx * GLYPH_SCALE + sx] = 255;
        }
    }
}

static int draw_text_line(uint8_t *pixels, int start_x, int y,
                          const char *text, size_t length)
{
    int x = start_x;
    if (!text)
        return 0;

    for (size_t index = 0;
         index < length && x + 6 * GLYPH_SCALE <= TEXTURE_WIDTH;
         index++) {
        const char character = text[index];
        draw_glyph(pixels, TEXTURE_WIDTH, x, y, character);
        x += 6 * GLYPH_SCALE;
    }
    return x > start_x ? x - start_x - GLYPH_SCALE : 0;
}

static int draw_text_at(uint8_t *pixels, int start_x, int y, const char *text)
{
    return draw_text_line(pixels, start_x, y, text,
                          text ? strlen(text) : 0);
}

static int draw_text(uint8_t *pixels, int y, const char *text)
{
    return draw_text_at(pixels, 0, y, text);
}

static int draw_details_text(uint8_t *pixels, int y, const char *text)
{
    int width = 0;
    int lines = 0;
    while (text && *text && lines < DETAILS_MAX_LINES) {
        const char *newline = strchr(text, '\n');
        const size_t length = newline ? (size_t) (newline - text) : strlen(text);
        width = fmax(width, draw_text_line(pixels, 0,
                                          y + lines * DETAILS_LINE_ADVANCE,
                                          text, length));
        lines++;
        if (!newline)
            break;
        text = newline + 1;
    }
    return width;
}

static bool update_title_texture(UpVideoRenderer *renderer, const char *title,
                                 int layout_width, int *title_width,
                                 int *title_height)
{
    struct unicode_run {
        const char *text;
        size_t length;
        int x;
        int cells;
    } unicode_runs[TEXTURE_WIDTH / TITLE_CELL_WIDTH] = {0};
    uint8_t pixels[TEXTURE_WIDTH * TITLE_TEXTURE_HEIGHT] = {0};
    const char *safe_title = title ? title : "";
    *title_width = renderer->title_width;
    *title_height = renderer->title_height;
    if (!*safe_title || layout_width <= 0) {
        *title_width = 0;
        *title_height = 0;
        return true;
    }
    if (renderer->title_texture &&
        renderer->title_layout_width == layout_width &&
        !strcmp(renderer->cached_title, safe_title))
        return true;

    const int texture_cells = TEXTURE_WIDTH / TITLE_CELL_WIDTH;
    int maximum_cells = (layout_width + GLYPH_SCALE) / TITLE_CELL_WIDTH;
    maximum_cells = maximum_cells < texture_cells
        ? maximum_cells : texture_cells;
    maximum_cells = maximum_cells > 0 ? maximum_cells : 1;
    const glong glyph_count = g_utf8_strlen(safe_title, -1);
    const bool ellipsized = glyph_count > maximum_cells;
    const int ellipsis_cells = ellipsized
        ? (maximum_cells < 3 ? maximum_cells : 3) : 0;
    const int content_cells = maximum_cells - ellipsis_cells;
    const size_t length = strlen(safe_title);
    size_t offset = 0;
    int cells = 0;
    int num_unicode_runs = 0;
    while (offset < length && cells < content_cells) {
        const char *current = safe_title + offset;
        gunichar character = g_utf8_get_char_validated(
            current, (gssize) (length - offset));
        if (character == (gunichar) -1 || character == (gunichar) -2) {
            draw_glyph(pixels, TEXTURE_WIDTH, cells * TITLE_CELL_WIDTH,
                       (TITLE_TEXTURE_HEIGHT - 14) / 2, '?');
            offset++;
            cells++;
            continue;
        }
        if (character < 0x80) {
            draw_glyph(pixels, TEXTURE_WIDTH, cells * TITLE_CELL_WIDTH,
                       (TITLE_TEXTURE_HEIGHT - 14) / 2, (char) character);
            offset = (size_t) (g_utf8_next_char(current) - safe_title);
            cells++;
            continue;
        }

        const char *run_start = current;
        int run_cells = 0;
        while (offset < length && cells + run_cells < content_cells) {
            current = safe_title + offset;
            character = g_utf8_get_char_validated(
                current, (gssize) (length - offset));
            if (character < 0x80 || character == (gunichar) -1 ||
                character == (gunichar) -2)
                break;
            offset = (size_t) (g_utf8_next_char(current) - safe_title);
            run_cells++;
        }
        unicode_runs[num_unicode_runs++] = (struct unicode_run) {
            .text = run_start,
            .length = (size_t) (safe_title + offset - run_start),
            .x = cells * TITLE_CELL_WIDTH,
            .cells = run_cells,
        };
        cells += run_cells;
    }
    for (int index = 0; index < ellipsis_cells; index++) {
        draw_glyph(pixels, TEXTURE_WIDTH, cells * TITLE_CELL_WIDTH,
                   (TITLE_TEXTURE_HEIGHT - 14) / 2, '.');
        cells++;
    }

    cairo_surface_t *surface = cairo_image_surface_create_for_data(
        pixels, CAIRO_FORMAT_A8, TEXTURE_WIDTH, TITLE_TEXTURE_HEIGHT,
        TEXTURE_WIDTH);
    cairo_surface_mark_dirty(surface);
    cairo_t *context = cairo_create(surface);
    cairo_font_options_t *font_options = cairo_font_options_create();
    cairo_font_options_set_antialias(font_options, CAIRO_ANTIALIAS_GRAY);
    cairo_font_options_set_hint_style(font_options, CAIRO_HINT_STYLE_FULL);
    cairo_font_options_set_hint_metrics(font_options, CAIRO_HINT_METRICS_ON);
    cairo_set_font_options(context, font_options);
    PangoLayout *layout = pango_cairo_create_layout(context);
    PangoFontDescription *font = pango_font_description_new();
    pango_font_description_set_family(font, "DejaVu Sans Mono");
    pango_font_description_set_absolute_size(font, 11 * PANGO_SCALE);
    pango_layout_set_font_description(layout, font);
    pango_layout_set_single_paragraph_mode(layout, true);
    cairo_set_source_rgba(context, 1.0, 1.0, 1.0, 1.0);
    for (int index = 0; index < num_unicode_runs; index++) {
        const struct unicode_run *run = &unicode_runs[index];
        char *run_text = g_strndup(run->text, run->length);
        const int run_width = run->cells * TITLE_CELL_WIDTH;
        pango_layout_set_text(layout, run_text, -1);
        pango_layout_set_width(layout, run_width * PANGO_SCALE);
        pango_layout_set_alignment(layout, PANGO_ALIGN_CENTER);
        PangoRectangle logical_extents;
        pango_layout_get_pixel_extents(layout, NULL, &logical_extents);
        cairo_save(context);
        cairo_translate(context, run->x,
                        ((TITLE_TEXTURE_HEIGHT - logical_extents.height) * 0.5) -
                        logical_extents.y);
        pango_cairo_show_layout(context, layout);
        cairo_restore(context);
        g_free(run_text);
    }
    cairo_surface_flush(surface);

    bool success = true;
    const unsigned char *glyphs = cairo_image_surface_get_data(surface);
    const int glyph_stride = cairo_image_surface_get_stride(surface);
    bool has_glyphs = false;
    for (int y = 0; y < TITLE_TEXTURE_HEIGHT && !has_glyphs; y++) {
        for (int x = 0; x < TEXTURE_WIDTH; x++) {
            if (glyphs[y * glyph_stride + x] != 0) {
                has_glyphs = true;
                break;
            }
        }
    }
    if (success && !has_glyphs) {
        set_error(renderer, "Pango produced an empty title texture");
        success = false;
    }
    if (success && (!renderer->title_texture ||
                    renderer->title_height != TITLE_TEXTURE_HEIGHT)) {
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->title_texture);
        pl_fmt format = pl_find_fmt(renderer->vulkan->gpu, PL_FMT_UNORM, 1,
                                    8, 8, PL_FMT_CAP_SAMPLEABLE);
        if (!format || !(renderer->title_texture = pl_tex_create(
                renderer->vulkan->gpu,
                pl_tex_params(.w = TEXTURE_WIDTH,
                              .h = TITLE_TEXTURE_HEIGHT,
                              .format = format, .sampleable = true,
                              .host_writable = true)))) {
            set_error(renderer, "could not create Vulkan title texture");
            success = false;
        }
    }
    if (success && !pl_tex_upload(
            renderer->vulkan->gpu,
            pl_tex_transfer_params(
                .tex = renderer->title_texture,
                .row_pitch = (size_t) glyph_stride,
                .ptr = (void *) glyphs))) {
        set_error(renderer, "could not upload the title to Vulkan");
        success = false;
    }
    if (success) {
        renderer->title_width = cells * TITLE_CELL_WIDTH - GLYPH_SCALE;
        renderer->title_height = TITLE_TEXTURE_HEIGHT;
        renderer->title_layout_width = layout_width;
        *title_width = renderer->title_width;
        *title_height = renderer->title_height;
        snprintf(renderer->cached_title, sizeof(renderer->cached_title), "%s",
                 safe_title);
    }

    pango_font_description_free(font);
    g_object_unref(layout);
    cairo_font_options_destroy(font_options);
    cairo_destroy(context);
    cairo_surface_destroy(surface);
    return success;
}

static bool update_text_texture(UpVideoRenderer *renderer, const char *info,
                                const char *details,
                                const char *position,
                                int *info_width, int *details_width,
                                int *details_height, int *position_width)
{
    uint8_t pixels[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    const char *safe_info = info ? info : "";
    const char *safe_details = details ? details : "";
    const char *safe_position = position ? position : "";

    *info_width = text_pixel_width(safe_info);
    details_text_metrics(safe_details, details_width, details_height);
    *position_width = text_pixel_width(safe_position);
    if (!strcmp(renderer->cached_info, safe_info) &&
        !strcmp(renderer->cached_details, safe_details) &&
        !strcmp(renderer->cached_position, safe_position))
        return true;

    *info_width = draw_text(pixels, INFO_GLYPH_Y, safe_info);
    draw_text_at(pixels, CLOSE_GLYPH_X, CLOSE_GLYPH_Y, "X");
    *position_width = draw_text(pixels, POSITION_GLYPH_Y, safe_position);
    *details_width = draw_details_text(pixels, DETAILS_GLYPH_Y, safe_details);
    if (!pl_tex_upload(renderer->vulkan->gpu,
                       pl_tex_transfer_params(
                           .tex = renderer->text_texture,
                           .row_pitch = TEXTURE_WIDTH,
                           .ptr = pixels))) {
        set_error(renderer, "could not upload overlay text to Vulkan");
        return false;
    }
    snprintf(renderer->cached_info, sizeof(renderer->cached_info), "%s",
             safe_info);
    snprintf(renderer->cached_details, sizeof(renderer->cached_details), "%s",
             safe_details);
    snprintf(renderer->cached_position, sizeof(renderer->cached_position), "%s",
             safe_position);
    return true;
}

static bool update_subtitle_texture(UpVideoRenderer *renderer,
                                    const uint8_t *pixels, int width, int height,
                                    uint64_t serial)
{
    if (!pixels || width <= 0 || height <= 0)
        return true;
    if (renderer->subtitle_texture && renderer->subtitle_width == width &&
        renderer->subtitle_height == height && renderer->subtitle_serial == serial)
        return true;

    if (!renderer->subtitle_texture || renderer->subtitle_width != width ||
        renderer->subtitle_height != height) {
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->subtitle_texture);
        pl_fmt format = pl_find_fmt(renderer->vulkan->gpu, PL_FMT_UNORM, 4,
                                    8, 8, PL_FMT_CAP_SAMPLEABLE);
        if (!format || !(renderer->subtitle_texture = pl_tex_create(
                renderer->vulkan->gpu,
                pl_tex_params(.w = width, .h = height, .format = format,
                              .sampleable = true, .host_writable = true)))) {
            set_error(renderer, "could not create Vulkan subtitle texture");
            return false;
        }
        renderer->subtitle_width = width;
        renderer->subtitle_height = height;
    }
    if (!pl_tex_upload(renderer->vulkan->gpu,
                       pl_tex_transfer_params(
                           .tex = renderer->subtitle_texture,
                           .row_pitch = (size_t) width * 4,
                           .ptr = (void *) pixels))) {
        set_error(renderer, "could not upload subtitle bitmap to Vulkan");
        return false;
    }
    renderer->subtitle_serial = serial;
    return true;
}

static bool update_text_subtitle_texture(UpVideoRenderer *renderer,
                                         const char *text, int layout_width,
                                         uint64_t serial)
{
    if (!text || !*text || layout_width <= 0)
        return true;
    if (renderer->text_subtitle_texture &&
        renderer->text_subtitle_serial == serial &&
        renderer->text_subtitle_layout_width == layout_width)
        return true;

    cairo_surface_t *measure_surface = cairo_image_surface_create(
        CAIRO_FORMAT_ARGB32, 1, 1);
    cairo_t *measure = cairo_create(measure_surface);
    PangoLayout *layout = pango_cairo_create_layout(measure);
    PangoFontDescription *font = pango_font_description_new();
    pango_font_description_set_family(font, "DejaVu Sans Mono");
    pango_font_description_set_weight(font, PANGO_WEIGHT_BOLD);
    pango_font_description_set_absolute_size(font, 18 * PANGO_SCALE);
    pango_layout_set_font_description(layout, font);
    pango_layout_set_text(layout, text, -1);
    pango_layout_set_width(layout, layout_width * PANGO_SCALE);
    pango_layout_set_wrap(layout, PANGO_WRAP_WORD_CHAR);
    pango_layout_set_alignment(layout, PANGO_ALIGN_CENTER);
    pango_layout_set_spacing(layout, 8 * PANGO_SCALE);
    pango_layout_set_height(layout, -4);
    pango_layout_set_ellipsize(layout, PANGO_ELLIPSIZE_END);

    PangoRectangle logical_extents;
    pango_layout_get_pixel_extents(layout, NULL, &logical_extents);
    int text_width = logical_extents.width;
    int text_height = logical_extents.height;
    text_width = text_width > 0 ? text_width : 1;
    text_height = text_height > 0 ? text_height : 1;
    cairo_surface_t *surface = cairo_image_surface_create(
        CAIRO_FORMAT_A8, text_width, text_height);
    cairo_t *context = cairo_create(surface);
    PangoLayout *render_layout = pango_cairo_create_layout(context);
    pango_layout_set_font_description(render_layout, font);
    pango_layout_set_text(render_layout, text, -1);
    pango_layout_set_width(render_layout, layout_width * PANGO_SCALE);
    pango_layout_set_wrap(render_layout, PANGO_WRAP_WORD_CHAR);
    pango_layout_set_alignment(render_layout, PANGO_ALIGN_CENTER);
    pango_layout_set_spacing(render_layout, 8 * PANGO_SCALE);
    pango_layout_set_height(render_layout, -4);
    pango_layout_set_ellipsize(render_layout, PANGO_ELLIPSIZE_END);
    cairo_set_operator(context, CAIRO_OPERATOR_SOURCE);
    cairo_set_source_rgba(context, 0.0, 0.0, 0.0, 0.0);
    cairo_paint(context);
    cairo_set_operator(context, CAIRO_OPERATOR_OVER);
    cairo_set_source_rgba(context, 1.0, 1.0, 1.0, 1.0);
    cairo_translate(context, -logical_extents.x, -logical_extents.y);
    pango_cairo_show_layout(context, render_layout);
    cairo_surface_flush(surface);

    bool success = true;
    const unsigned char *glyphs = cairo_image_surface_get_data(surface);
    int glyph_stride = cairo_image_surface_get_stride(surface);
    bool has_glyphs = false;
    for (int y = 0; y < text_height && !has_glyphs; y++) {
        for (int x = 0; x < text_width; x++) {
            if (glyphs[y * glyph_stride + x] != 0) {
                has_glyphs = true;
                break;
            }
        }
    }
    if (!has_glyphs) {
        set_error(renderer, "Pango produced an empty subtitle glyph mask");
        success = false;
    }
    if (!renderer->text_subtitle_texture ||
        renderer->text_subtitle_width != text_width ||
        renderer->text_subtitle_height != text_height) {
        pl_tex_destroy(renderer->vulkan->gpu,
                       &renderer->text_subtitle_texture);
        pl_fmt format = pl_find_fmt(renderer->vulkan->gpu, PL_FMT_UNORM, 1,
                                    8, 8, PL_FMT_CAP_SAMPLEABLE);
        if (!format || !(renderer->text_subtitle_texture = pl_tex_create(
                renderer->vulkan->gpu,
                pl_tex_params(.w = text_width, .h = text_height,
                              .format = format, .sampleable = true,
                              .host_writable = true)))) {
            set_error(renderer, "could not create Vulkan text subtitle texture");
            success = false;
        }
        renderer->text_subtitle_width = text_width;
        renderer->text_subtitle_height = text_height;
    }
    if (success && !pl_tex_upload(
            renderer->vulkan->gpu,
            pl_tex_transfer_params(
                .tex = renderer->text_subtitle_texture,
                .row_pitch = (size_t) glyph_stride,
                .ptr = (void *) glyphs))) {
        set_error(renderer, "could not upload text subtitle glyphs to Vulkan");
        success = false;
    }
    if (success) {
        renderer->text_subtitle_serial = serial;
        renderer->text_subtitle_layout_width = layout_width;
    }

    g_object_unref(render_layout);
    cairo_destroy(context);
    cairo_surface_destroy(surface);
    pango_font_description_free(font);
    g_object_unref(layout);
    cairo_destroy(measure);
    cairo_surface_destroy(measure_surface);
    return success;
}

static void set_error(UpVideoRenderer *renderer, const char *message)
{
    snprintf(renderer->error, sizeof(renderer->error), "%s", message);
}

static char *join_extensions(const char *const *first, size_t first_count,
                             const char *const *second, size_t second_count,
                             const char *prefix, const char *excluded)
{
    size_t length = prefix ? strlen(prefix) : 0;
    size_t count = first_count + second_count + (prefix ? 1 : 0);

    for (size_t i = 0; i < first_count; i++)
        length += strlen(first[i]);
    for (size_t i = 0; i < second_count; i++)
        length += strlen(second[i]);
    if (count > 1)
        length += count - 1;

    char *result = av_malloc(length + 1);
    if (!result)
        return NULL;

    result[0] = '\0';
    if (prefix)
        strcat(result, prefix);
    for (size_t i = 0; i < first_count; i++) {
        if (excluded && !strcmp(first[i], excluded))
            continue;
        if (result[0])
            strcat(result, "+");
        strcat(result, first[i]);
    }
    for (size_t i = 0; i < second_count; i++) {
        if (excluded && !strcmp(second[i], excluded))
            continue;
        if (result[0])
            strcat(result, "+");
        strcat(result, second[i]);
    }
    return result;
}

static void hwctx_lock_queue(void *private_data, uint32_t family, uint32_t index)
{
#if FF_API_VULKAN_SYNC_QUEUES
    AVHWDeviceContext *device = private_data;
    const AVVulkanDeviceContext *vulkan = device->hwctx;
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    vulkan->lock_queue(device, family, index);
#pragma GCC diagnostic pop
#else
    (void) private_data;
    (void) family;
    (void) index;
#endif
}

static void hwctx_unlock_queue(void *private_data, uint32_t family, uint32_t index)
{
#if FF_API_VULKAN_SYNC_QUEUES
    AVHWDeviceContext *device = private_data;
    const AVVulkanDeviceContext *vulkan = device->hwctx;
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    vulkan->unlock_queue(device, family, index);
#pragma GCC diagnostic pop
#else
    (void) private_data;
    (void) family;
    (void) index;
#endif
}

UpVideoRenderer *up_video_renderer_create(void *window_pointer)
{
    SDL_Window *window = window_pointer;
    UpVideoRenderer *renderer = av_mallocz(sizeof(*renderer));
    AVDictionary *options = NULL;
    uint32_t instance_count = 0;
    const char *const *instance_extensions;
    char *instance_list = NULL;
    char *device_list = NULL;
    int ret;

    if (!renderer)
        return NULL;

    renderer->log = pl_log_create(
        PL_API_VER,
        pl_log_params(.log_cb = pl_log_simple, .log_priv = stderr,
                      .log_level = PL_LOG_ERR));
    if (!renderer->log) {
        set_error(renderer, "could not create the libplacebo logger");
        return renderer;
    }

    instance_extensions = SDL_Vulkan_GetInstanceExtensions(&instance_count);
    if (!instance_extensions) {
        set_error(renderer, SDL_GetError());
        return renderer;
    }

    instance_list = join_extensions(instance_extensions, instance_count,
                                    NULL, 0, NULL, NULL);
    device_list = join_extensions(pl_vulkan_recommended_extensions,
                                  pl_vulkan_num_recommended_extensions,
                                  NULL, 0, VK_KHR_SWAPCHAIN_EXTENSION_NAME,
                                  VK_KHR_PORTABILITY_SUBSET_EXTENSION_NAME);
    if (!instance_list || !device_list) {
        set_error(renderer, "out of memory while preparing Vulkan extensions");
        goto fail;
    }

    av_dict_set(&options, "instance_extensions", instance_list,
                AV_DICT_DONT_STRDUP_VAL);
    instance_list = NULL;
    av_dict_set(&options, "device_extensions", device_list,
                AV_DICT_DONT_STRDUP_VAL);
    device_list = NULL;

    const char *vulkan_device = getenv("UP_VULKAN_DEVICE");
    if (vulkan_device && !*vulkan_device)
        vulkan_device = NULL;
    ret = av_hwdevice_ctx_create(&renderer->hw_device,
                                 AV_HWDEVICE_TYPE_VULKAN,
                                 vulkan_device, options, 0);
    av_dict_free(&options);
    if (ret < 0) {
        set_error(renderer, "FFmpeg could not create a Vulkan device");
        goto fail;
    }

    AVHWDeviceContext *device = (AVHWDeviceContext *) renderer->hw_device->data;
    AVVulkanDeviceContext *vulkan = device->hwctx;
    PFN_vkGetInstanceProcAddr sdl_get_proc_addr =
        (PFN_vkGetInstanceProcAddr) SDL_Vulkan_GetVkGetInstanceProcAddr();
    if (!sdl_get_proc_addr || vulkan->get_proc_addr != sdl_get_proc_addr) {
        set_error(renderer,
                  "FFmpeg and SDL loaded different Vulkan implementations");
        goto fail;
    }
    renderer->get_proc_addr = vulkan->get_proc_addr;
    renderer->instance = vulkan->inst;

    struct pl_vulkan_import_params import = {
        .instance = vulkan->inst,
        .get_proc_addr = vulkan->get_proc_addr,
        .phys_device = vulkan->phys_dev,
        .device = vulkan->act_dev,
        .extensions = vulkan->enabled_dev_extensions,
        .num_extensions = vulkan->nb_enabled_dev_extensions,
        .features = &vulkan->device_features,
        .lock_queue = hwctx_lock_queue,
        .unlock_queue = hwctx_unlock_queue,
        .queue_ctx = device,
        .queue_graphics = { .index = VK_QUEUE_FAMILY_IGNORED },
        .queue_compute = { .index = VK_QUEUE_FAMILY_IGNORED },
        .queue_transfer = { .index = VK_QUEUE_FAMILY_IGNORED },
        .max_api_version = VK_API_VERSION_1_3,
    };

    for (int i = 0; i < vulkan->nb_qf; i++) {
        const AVVulkanDeviceQueueFamily *queue = &vulkan->qf[i];
        if (queue->flags & VK_QUEUE_GRAPHICS_BIT)
            import.queue_graphics = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
        if (queue->flags & VK_QUEUE_COMPUTE_BIT)
            import.queue_compute = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
        if (queue->flags & VK_QUEUE_TRANSFER_BIT)
            import.queue_transfer = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
    }

    renderer->vulkan = pl_vulkan_import(renderer->log, &import);
    if (!renderer->vulkan) {
        set_error(renderer, "libplacebo could not import FFmpeg's Vulkan device");
        goto fail;
    }

    if (!SDL_Vulkan_CreateSurface(window, renderer->instance, NULL,
                                  &renderer->surface)) {
        set_error(renderer, SDL_GetError());
        goto fail;
    }

    renderer->swapchain = pl_vulkan_create_swapchain(
        renderer->vulkan,
        pl_vulkan_swapchain_params(
            .surface = renderer->surface,
            .present_mode = VK_PRESENT_MODE_FIFO_KHR,
            .swapchain_depth = 3));
    if (!renderer->swapchain) {
        set_error(renderer, "libplacebo could not create the Wayland swapchain");
        goto fail;
    }

    renderer->renderer = pl_renderer_create(renderer->log,
                                             renderer->vulkan->gpu);
    if (!renderer->renderer) {
        set_error(renderer, "libplacebo could not create the renderer");
        goto fail;
    }

    pl_fmt mask_format = pl_find_fmt(renderer->vulkan->gpu, PL_FMT_UNORM, 1,
                                     8, 8, PL_FMT_CAP_SAMPLEABLE);
    const uint8_t white = 255;
    const uint8_t empty[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    if (!mask_format ||
        !(renderer->solid_texture = pl_tex_create(
              renderer->vulkan->gpu,
              pl_tex_params(.w = 1, .h = 1, .format = mask_format,
                            .sampleable = true, .initial_data = &white))) ||
        !(renderer->text_texture = pl_tex_create(
              renderer->vulkan->gpu,
              pl_tex_params(.w = TEXTURE_WIDTH, .h = TEXTURE_HEIGHT,
                            .format = mask_format, .sampleable = true,
                            .host_writable = true, .initial_data = empty)))) {
        set_error(renderer, "could not create Vulkan overlay textures");
        goto fail;
    }

    return renderer;

fail:
    av_free(instance_list);
    av_free(device_list);
    av_dict_free(&options);
    return renderer;
}

void *up_video_renderer_device(UpVideoRenderer *renderer)
{
    if (!renderer || !renderer->renderer)
        return NULL;
    return renderer->hw_device;
}

int up_video_renderer_display(UpVideoRenderer *renderer, void *frame_pointer,
                              int width, int height, float top_bar_alpha,
                              const char *title, const char *info,
                              float info_alpha, const char *details,
                              const char *position,
                              float position_alpha, float scrubber_progress,
                              float scrubber_alpha, const char *subtitle_text,
                              const uint8_t *subtitle_pixels,
                              int subtitle_width, int subtitle_height,
                              uint64_t subtitle_serial)
{
    AVFrame *frame = frame_pointer;
    struct pl_swapchain_frame swap_frame = {0};
    struct pl_frame image = {0};
    struct pl_frame target = {0};
    struct pl_render_params params = pl_render_default_params;
    struct pl_color_space hint = {0};
    struct pl_overlay overlays[16] = {0};
    struct pl_overlay_part parts[16] = {0};
    struct pl_overlay bitmap_overlay = {0};
    struct pl_overlay_part bitmap_part = {0};
    int num_overlays = 0;
    int title_width = 0, title_height = 0, info_width = 0;
    int details_width = 0, details_height = 0, position_width = 0;
    int ret = -1;

    if (!renderer || !renderer->renderer || !frame || width <= 0 || height <= 0)
        return -1;

    if (!pl_map_avframe_ex(renderer->vulkan->gpu, &image,
                           pl_avframe_params(
                               .frame = frame,
                               .tex = renderer->textures))) {
        set_error(renderer, "libplacebo could not map the decoded video frame");
        return -1;
    }

    if (subtitle_pixels && subtitle_width > 0 && subtitle_height > 0) {
        if (!update_subtitle_texture(renderer, subtitle_pixels, subtitle_width,
                                     subtitle_height, subtitle_serial))
            goto out;
        bitmap_part = (struct pl_overlay_part) {
            .src = {0, 0, subtitle_width, subtitle_height},
            .dst = {0, 0, frame->width, frame->height},
        };
        bitmap_overlay = (struct pl_overlay) {
            .tex = renderer->subtitle_texture,
            .mode = PL_OVERLAY_NORMAL,
            .coords = PL_OVERLAY_COORDS_SRC_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &bitmap_part,
            .num_parts = 1,
        };
        bitmap_overlay.repr.alpha = PL_ALPHA_INDEPENDENT;
        image.overlays = &bitmap_overlay;
        image.num_overlays = 1;
    }

    pl_color_space_from_avframe(&hint, frame);
    pl_swapchain_colorspace_hint(renderer->swapchain, &hint);
    if (!pl_swapchain_start_frame(renderer->swapchain, &swap_frame)) {
        set_error(renderer, "could not acquire a Vulkan swapchain image");
        goto out;
    }

    pl_frame_from_swapchain(&target, &swap_frame);
    double sample_aspect = 1.0;
    if (frame->sample_aspect_ratio.num > 0 &&
        frame->sample_aspect_ratio.den > 0) {
        sample_aspect = (double) frame->sample_aspect_ratio.num /
                        frame->sample_aspect_ratio.den;
    }
    double video_aspect = frame->height > 0
        ? ((double) frame->width * sample_aspect) / frame->height
        : (double) width / height;
    double window_aspect = (double) width / height;
    float x0 = 0.0f, y0 = 0.0f, x1 = (float) width, y1 = (float) height;
    if (video_aspect > window_aspect) {
        float fitted_height = (float) (width / video_aspect);
        y0 = ((float) height - fitted_height) * 0.5f;
        y1 = y0 + fitted_height;
    } else {
        float fitted_width = (float) (height * video_aspect);
        x0 = ((float) width - fitted_width) * 0.5f;
        x1 = x0 + fitted_width;
    }
    target.crop = (struct pl_rect2df) { .x0 = x0, .y0 = y0,
                                        .x1 = x1, .y1 = y1 };

    if (subtitle_text && *subtitle_text) {
        int layout_width = (int) (x1 - x0) - 80;
        layout_width = layout_width > 120 ? layout_width : 120;
        if (!update_text_subtitle_texture(renderer, subtitle_text, layout_width,
                                          subtitle_serial))
            goto out;
    }

    const int title_layout_width = width > 84 ? width - 84 : 1;
    if (!update_title_texture(renderer, title, title_layout_width,
                              &title_width, &title_height) ||
        !update_text_texture(renderer, info, details, position,
                             &info_width, &details_width, &details_height,
                             &position_width))
        goto out;

    top_bar_alpha = fminf(fmaxf(top_bar_alpha, 0.0f), 1.0f);
    if (top_bar_alpha > 0.001f) {
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1}, .dst = {0, 0, width, 42},
            .color = {0.02f, 0.02f, 0.02f, 0.72f * top_bar_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->solid_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;

        if (renderer->title_texture && title_width > 0 && title_height > 0) {
            const float title_x = fmaxf(
                ((float) width - title_width) * 0.5f, 14.0f);
            const float title_y = ((42.0f - title_height) * 0.5f);
            parts[num_overlays] = (struct pl_overlay_part) {
                .src = {0, 0, title_width, title_height},
                .dst = {title_x, title_y,
                        title_x + title_width, title_y + title_height},
                .color = {1.0f, 1.0f, 1.0f, top_bar_alpha},
            };
            overlays[num_overlays] = (struct pl_overlay) {
                .tex = renderer->title_texture,
                .mode = PL_OVERLAY_MONOCHROME,
                .coords = PL_OVERLAY_COORDS_DST_FRAME,
                .repr = pl_color_repr_rgb,
                .color = pl_color_space_srgb,
                .parts = &parts[num_overlays], .num_parts = 1,
            };
            overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
            num_overlays++;
        }

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {CLOSE_GLYPH_X, CLOSE_GLYPH_Y,
                    CLOSE_GLYPH_X + 10, CLOSE_GLYPH_Y + 14},
            .dst = {width - 26, 14, width - 16, 28},
            .color = {1.0f, 1.0f, 1.0f, top_bar_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->text_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    if (details && details_width > 0 && details_height > 0) {
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, DETAILS_GLYPH_Y,
                    details_width, DETAILS_GLYPH_Y + details_height},
            .dst = {INFO_TEXT_INSET, INFO_TEXT_INSET,
                    INFO_TEXT_INSET + details_width,
                    INFO_TEXT_INSET + details_height},
            .color = {1.0f, 1.0f, 1.0f, 1.0f},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->text_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    info_alpha = fminf(fmaxf(info_alpha, 0.0f), 1.0f);
    if (info && info_width > 0 && info_alpha > 0.001f) {
        const float info_y = fmaxf(height - INFO_TEXT_INSET - 14.0f, 0.0f);

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 16, info_width, 30},
            .dst = {INFO_TEXT_INSET, info_y,
                    INFO_TEXT_INSET + info_width, info_y + 14},
            .color = {1.0f, 1.0f, 1.0f, info_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->text_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    position_alpha = fminf(fmaxf(position_alpha, 0.0f), 1.0f);
    if (position && position_width > 0 && position_alpha > 0.001f) {
        const float position_y = fmaxf(
            height - INFO_TEXT_INSET - 14.0f, 0.0f);
        const float position_x = fmaxf(
            width - INFO_TEXT_INSET - position_width, INFO_TEXT_INSET);

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, POSITION_GLYPH_Y,
                    position_width, POSITION_GLYPH_Y + 14},
            .dst = {position_x, position_y,
                    position_x + position_width, position_y + 14},
            .color = {1.0f, 1.0f, 1.0f, position_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->text_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    scrubber_alpha = fminf(fmaxf(scrubber_alpha, 0.0f), 1.0f);
    if (scrubber_progress >= 0.0f && scrubber_alpha > 0.001f) {
        const float left = 14.0f;
        const float right = fmaxf(width - 14.0f, left);
        const float center_y = fmaxf(height - 18.0f, 0.0f);
        const float progress_x = left + (right - left) *
            fminf(fmaxf(scrubber_progress, 0.0f), 1.0f);

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1},
            .dst = {left, center_y - 2, right, center_y + 2},
            .color = {1.0f, 1.0f, 1.0f, 0.35f * scrubber_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->solid_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;

        if (progress_x > left) {
            parts[num_overlays] = (struct pl_overlay_part) {
                .src = {0, 0, 1, 1},
                .dst = {left, center_y - 2, progress_x, center_y + 2},
                .color = {0.25f, 0.70f, 1.0f, scrubber_alpha},
            };
            overlays[num_overlays] = (struct pl_overlay) {
                .tex = renderer->solid_texture,
                .mode = PL_OVERLAY_MONOCHROME,
                .coords = PL_OVERLAY_COORDS_DST_FRAME,
                .repr = pl_color_repr_rgb,
                .color = pl_color_space_srgb,
                .parts = &parts[num_overlays], .num_parts = 1,
            };
            overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
            num_overlays++;
        }

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1},
            .dst = {progress_x - 3, center_y - 6,
                    progress_x + 3, center_y + 6},
            .color = {1.0f, 1.0f, 1.0f, scrubber_alpha},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->solid_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    if (subtitle_text && *subtitle_text && renderer->text_subtitle_texture) {
        const float text_width = renderer->text_subtitle_width;
        const float text_height = renderer->text_subtitle_height;
        const float bottom = fmaxf(y0 + text_height + 8.0f, y1 - 70.0f);
        const float top = bottom - text_height;
        const float background_left = fmaxf(
            ((float) width - text_width) * 0.5f - 8.0f, x0);
        const float background_right = fminf(
            ((float) width + text_width) * 0.5f + 8.0f, x1);
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1},
            .dst = {background_left, top - 5.0f,
                    background_right, bottom + 5.0f},
            .color = {0.0f, 0.0f, 0.0f, 0.72f},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->solid_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;

        const float text_x = ((float) width - text_width) * 0.5f;
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, text_width, text_height},
            .dst = {text_x, top, text_x + text_width, bottom},
            .color = {1.0f, 1.0f, 1.0f, 1.0f},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = renderer->text_subtitle_texture,
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }
    target.overlays = overlays;
    target.num_overlays = num_overlays;
    params.background = PL_CLEAR_COLOR;
    params.background_color[0] = 0.0f;
    params.background_color[1] = 0.0f;
    params.background_color[2] = 0.0f;
    params.background_transparency = 0.0f;
    // Spend the available GPU headroom on a sharper reconstruction for small
    // sources. Hardware-decoded Vulkan frames stay on-device throughout
    // scaling; software-decoded frames are uploaded here by libplacebo.
    if (frame->width <= 1280 && frame->height <= 720)
        params.upscaler = &pl_filter_ewa_lanczossharp;
    // Dynamic HDR peak detection scans the full frame and eventually contends
    // with 8K60 AV1 decoding on this GPU. Keep it for lower resolutions, but
    // use the source's mastering metadata for 8K HDR presentation.
    if (frame->width >= 7680 || frame->height >= 4320)
        params.peak_detect_params = NULL;
    if (!pl_render_image(renderer->renderer, &image, &target, &params)) {
        set_error(renderer, "libplacebo failed to render the video frame");
        goto out;
    }
    struct pl_render_errors render_errors =
        pl_renderer_get_errors(renderer->renderer);
    if (render_errors.errors & (PL_RENDER_ERR_BLENDING | PL_RENDER_ERR_OVERLAY)) {
        snprintf(renderer->error, sizeof(renderer->error),
                 "libplacebo overlay rendering failed (errors=0x%x)",
                 render_errors.errors);
        goto out;
    }
    if (!pl_swapchain_submit_frame(renderer->swapchain)) {
        set_error(renderer, "libplacebo failed to submit the video frame");
        goto out;
    }
    pl_swapchain_swap_buffers(renderer->swapchain);
    ret = 0;

out:
    pl_unmap_avframe(renderer->vulkan->gpu, &image);
    return ret;
}

int up_video_renderer_resize(UpVideoRenderer *renderer, int width, int height)
{
    if (!renderer || !renderer->swapchain || width <= 0 || height <= 0)
        return -1;
    return pl_swapchain_resize(renderer->swapchain, &width, &height) ? 0 : -1;
}

const char *up_video_renderer_error(const UpVideoRenderer *renderer)
{
    if (!renderer || !renderer->error[0])
        return "unknown video renderer error";
    return renderer->error;
}

void up_video_renderer_destroy(UpVideoRenderer *renderer)
{
    if (!renderer)
        return;

    if (renderer->vulkan) {
        for (size_t i = 0; i < 4; i++)
            pl_tex_destroy(renderer->vulkan->gpu, &renderer->textures[i]);
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->solid_texture);
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->text_texture);
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->title_texture);
        pl_tex_destroy(renderer->vulkan->gpu, &renderer->subtitle_texture);
        pl_tex_destroy(renderer->vulkan->gpu,
                       &renderer->text_subtitle_texture);
        pl_renderer_destroy(&renderer->renderer);
        pl_swapchain_destroy(&renderer->swapchain);
        pl_vulkan_destroy(&renderer->vulkan);
    }
    if (renderer->surface)
        SDL_Vulkan_DestroySurface(renderer->instance, renderer->surface, NULL);
    av_buffer_unref(&renderer->hw_device);
    pl_log_destroy(&renderer->log);
    av_free(renderer);
}
