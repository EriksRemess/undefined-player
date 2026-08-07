#define VK_NO_PROTOTYPES
#define VK_ENABLE_BETA_EXTENSIONS
#define PL_LIBAV_IMPLEMENTATION 1

#include "video_renderer.h"

#include <SDL3/SDL_vulkan.h>
#include <libplacebo/renderer.h>
#include <libplacebo/swapchain.h>
#include <libplacebo/utils/libav.h>
#include <libplacebo/vulkan.h>

#include <libavutil/dict.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vulkan.h>
#include <libavutil/mem.h>

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

    AVBufferRef *hw_device;
    PFN_vkGetInstanceProcAddr get_proc_addr;
    VkInstance instance;
    VkSurfaceKHR surface;

    char error[256];
    char cached_title[512];
    char cached_info[256];
    char cached_position[128];
};

#define TEXTURE_WIDTH 1024
#define TEXTURE_HEIGHT 64
#define GLYPH_SCALE 2
#define CLOSE_GLYPH_X (TEXTURE_WIDTH - 12)
#define CLOSE_GLYPH_Y 32
#define POSITION_GLYPH_Y 48

static void set_error(UpVideoRenderer *renderer, const char *message);

static size_t bounded_length(const char *text, size_t maximum)
{
    size_t length = 0;
    while (length < maximum && text[length])
        length++;
    return length;
}

static int text_pixel_width(const char *text)
{
    const size_t maximum = TEXTURE_WIDTH / (6 * GLYPH_SCALE);
    const size_t length = bounded_length(text, maximum);
    return length ? (int) (length * 6 * GLYPH_SCALE - GLYPH_SCALE) : 0;
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
    return blank;
}

static int draw_text_at(uint8_t *pixels, int start_x, int y, const char *text)
{
    int x = start_x;
    if (!text)
        return 0;

    for (; *text && x + 6 * GLYPH_SCALE <= TEXTURE_WIDTH; text++) {
        const uint8_t *rows = glyph_rows(*text);
        for (int gy = 0; gy < 7; gy++) {
            for (int gx = 0; gx < 5; gx++) {
                if (!(rows[gy] & (1 << (4 - gx))))
                    continue;
                for (int sy = 0; sy < GLYPH_SCALE; sy++)
                    for (int sx = 0; sx < GLYPH_SCALE; sx++)
                        pixels[(y + gy * GLYPH_SCALE + sy) * TEXTURE_WIDTH +
                               x + gx * GLYPH_SCALE + sx] = 255;
            }
        }
        x += 6 * GLYPH_SCALE;
    }
    return x > start_x ? x - start_x - GLYPH_SCALE : 0;
}

static int draw_text(uint8_t *pixels, int y, const char *text)
{
    return draw_text_at(pixels, 0, y, text);
}

static bool update_text_texture(UpVideoRenderer *renderer, const char *title,
                                const char *info, const char *position,
                                int *title_width, int *info_width,
                                int *position_width)
{
    uint8_t pixels[TEXTURE_WIDTH * TEXTURE_HEIGHT] = {0};
    const char *safe_title = title ? title : "";
    const char *safe_info = info ? info : "";
    const char *safe_position = position ? position : "";

    *title_width = text_pixel_width(safe_title);
    *info_width = text_pixel_width(safe_info);
    *position_width = text_pixel_width(safe_position);
    if (!strcmp(renderer->cached_title, safe_title) &&
        !strcmp(renderer->cached_info, safe_info) &&
        !strcmp(renderer->cached_position, safe_position))
        return true;

    *title_width = draw_text(pixels, 0, safe_title);
    *info_width = draw_text(pixels, 16, safe_info);
    draw_text_at(pixels, CLOSE_GLYPH_X, CLOSE_GLYPH_Y, "X");
    *position_width = draw_text(pixels, POSITION_GLYPH_Y, safe_position);
    if (!pl_tex_upload(renderer->vulkan->gpu,
                       pl_tex_transfer_params(
                           .tex = renderer->text_texture,
                           .row_pitch = TEXTURE_WIDTH,
                           .ptr = pixels))) {
        set_error(renderer, "could not upload overlay text to Vulkan");
        return false;
    }
    snprintf(renderer->cached_title, sizeof(renderer->cached_title), "%s",
             safe_title);
    snprintf(renderer->cached_info, sizeof(renderer->cached_info), "%s",
             safe_info);
    snprintf(renderer->cached_position, sizeof(renderer->cached_position), "%s",
             safe_position);
    return true;
}

static void set_error(UpVideoRenderer *renderer, const char *message)
{
    snprintf(renderer->error, sizeof(renderer->error), "%s", message);
}

static char *join_extensions(const char *const *first, size_t first_count,
                             const char *const *second, size_t second_count,
                             const char *prefix)
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
        if (result[0])
            strcat(result, "+");
        strcat(result, first[i]);
    }
    for (size_t i = 0; i < second_count; i++) {
        if (result[0])
            strcat(result, "+");
        strcat(result, second[i]);
    }
    return result;
}

static void hwctx_lock_queue(void *private_data, uint32_t family, uint32_t index)
{
    AVHWDeviceContext *device = private_data;
    const AVVulkanDeviceContext *vulkan = device->hwctx;
#if FF_API_VULKAN_SYNC_QUEUES
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    vulkan->lock_queue(device, family, index);
#pragma GCC diagnostic pop
#else
    (void) family;
    (void) index;
#endif
}

static void hwctx_unlock_queue(void *private_data, uint32_t family, uint32_t index)
{
    AVHWDeviceContext *device = private_data;
    const AVVulkanDeviceContext *vulkan = device->hwctx;
#if FF_API_VULKAN_SYNC_QUEUES
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    vulkan->unlock_queue(device, family, index);
#pragma GCC diagnostic pop
#else
    (void) family;
    (void) index;
#endif
}

UpVideoRenderer *up_video_renderer_create(SDL_Window *window)
{
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
                                    NULL, 0, NULL);
    device_list = join_extensions(pl_vulkan_recommended_extensions,
                                  pl_vulkan_num_recommended_extensions,
                                  NULL, 0, VK_KHR_SWAPCHAIN_EXTENSION_NAME);
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

    ret = av_hwdevice_ctx_create(&renderer->hw_device,
                                 AV_HWDEVICE_TYPE_VULKAN,
                                 "NVIDIA RTX A4000", options, 0);
    av_dict_free(&options);
    if (ret < 0) {
        set_error(renderer, "FFmpeg could not create the NVIDIA Vulkan device");
        goto fail;
    }

    AVHWDeviceContext *device = (AVHWDeviceContext *) renderer->hw_device->data;
    AVVulkanDeviceContext *vulkan = device->hwctx;
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

AVBufferRef *up_video_renderer_device(UpVideoRenderer *renderer)
{
    if (!renderer || !renderer->renderer)
        return NULL;
    return renderer->hw_device;
}

int up_video_renderer_display(UpVideoRenderer *renderer, AVFrame *frame,
                              int width, int height, float top_bar_alpha,
                              const char *title, const char *info,
                              float info_alpha, const char *position,
                              float position_alpha)
{
    struct pl_swapchain_frame swap_frame = {0};
    struct pl_frame image = {0};
    struct pl_frame target = {0};
    struct pl_render_params params = pl_render_default_params;
    struct pl_color_space hint = {0};
    struct pl_overlay overlays[7] = {0};
    struct pl_overlay_part parts[7] = {0};
    int num_overlays = 0;
    int title_width = 0, info_width = 0, position_width = 0;
    int ret = -1;

    if (!renderer || !renderer->renderer || !frame || width <= 0 || height <= 0)
        return -1;

    if (!pl_map_avframe_ex(renderer->vulkan->gpu, &image,
                           pl_avframe_params(
                               .frame = frame,
                               .tex = renderer->textures))) {
        set_error(renderer, "libplacebo could not map the Vulkan video frame");
        return -1;
    }

    pl_color_space_from_avframe(&hint, frame);
    pl_swapchain_colorspace_hint(renderer->swapchain, &hint);
    if (!pl_swapchain_start_frame(renderer->swapchain, &swap_frame)) {
        set_error(renderer, "could not acquire a Vulkan swapchain image");
        goto out;
    }

    pl_frame_from_swapchain(&target, &swap_frame);
    if (!update_text_texture(renderer, title, info, position, &title_width,
                             &info_width, &position_width))
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

        if (title_width > 0) {
            parts[num_overlays] = (struct pl_overlay_part) {
                .src = {0, 0, title_width, 14},
                .dst = {14, 14, 14 + title_width, 28},
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

    info_alpha = fminf(fmaxf(info_alpha, 0.0f), 1.0f);
    if (info && info_width > 0 && info_alpha > 0.001f) {
        const float info_y = fmaxf(height - 32.0f, 0.0f);
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1},
            .dst = {6, info_y - 4, 22 + info_width, info_y + 22},
            .color = {0.0f, 0.0f, 0.0f, 0.72f * info_alpha},
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

        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 16, info_width, 30},
            .dst = {14, info_y, 14 + info_width, info_y + 14},
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
        const float position_y = fmaxf(height - 32.0f, 0.0f);
        const float position_x = fmaxf(width - position_width - 14.0f, 14.0f);
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {0, 0, 1, 1},
            .dst = {position_x - 8, position_y - 4,
                    width - 6, position_y + 22},
            .color = {0.0f, 0.0f, 0.0f, 0.72f * position_alpha},
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
    target.overlays = overlays;
    target.num_overlays = num_overlays;

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
    params.background = PL_CLEAR_COLOR;
    params.background_color[0] = 0.0f;
    params.background_color[1] = 0.0f;
    params.background_color[2] = 0.0f;
    params.background_transparency = 0.0f;

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
