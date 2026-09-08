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
    pl_tex overlay_textures[2];
    uint64_t overlay_serials[2];
    pl_tex subtitle_texture;
    pl_tex text_subtitle_texture;
    int subtitle_width;
    int subtitle_height;
    uint64_t subtitle_serial;
    int text_subtitle_width;
    int text_subtitle_height;
    int text_subtitle_layout_width;
    uint64_t text_subtitle_serial;
    bool text_subtitle_visible;

    AVBufferRef *hw_device;
    PFN_vkGetInstanceProcAddr get_proc_addr;
    VkInstance instance;
    VkSurfaceKHR surface;

    char error[256];
};

static void set_error(UpVideoRenderer *renderer, const char *message);

static bool upload_overlay_image(UpVideoRenderer *renderer, size_t index,
                                 const UpOverlayImage *image)
{
    if (!image->pixels || image->width <= 0 || image->height <= 0) {
        set_error(renderer, "invalid overlay image");
        return false;
    }
    pl_tex *texture = &renderer->overlay_textures[index];
    if (*texture && (*texture)->params.w == image->width &&
        (*texture)->params.h == image->height &&
        renderer->overlay_serials[index] == image->serial)
        return true;
    if (!*texture || (*texture)->params.w != image->width ||
        (*texture)->params.h != image->height) {
        pl_tex_destroy(renderer->vulkan->gpu, texture);
        pl_fmt format = pl_find_fmt(renderer->vulkan->gpu, PL_FMT_UNORM, 1,
                                    8, 8, PL_FMT_CAP_SAMPLEABLE);
        if (!format || !(*texture = pl_tex_create(renderer->vulkan->gpu,
                pl_tex_params(.w = image->width, .h = image->height,
                              .format = format, .sampleable = true,
                              .host_writable = true)))) {
            set_error(renderer, "could not create Vulkan overlay image");
            return false;
        }
    }
    if (!pl_tex_upload(renderer->vulkan->gpu, pl_tex_transfer_params(
            .tex = *texture, .row_pitch = (size_t) image->width,
            .ptr = (void *) image->pixels))) {
        set_error(renderer, "could not upload overlay image to Vulkan");
        return false;
    }
    renderer->overlay_serials[index] = image->serial;
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
    if (renderer->text_subtitle_serial == serial &&
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
    // Invisible cues are valid. Keep the old allocation for reuse, but hide
    // both its glyphs and background until a visible cue replaces it.
    if (has_glyphs && (!renderer->text_subtitle_texture ||
        renderer->text_subtitle_width != text_width ||
        renderer->text_subtitle_height != text_height)) {
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
    if (success && has_glyphs && !pl_tex_upload(
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
        renderer->text_subtitle_visible = has_glyphs;
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

#if LIBAVUTIL_VERSION_MAJOR >= 61 && PL_API_VER < 365
/* libplacebo before API 365 always retrieves imported queues with
 * vkGetDeviceQueue. FFmpeg 9 may create them with flags that require the
 * equivalent vkGetDeviceQueue2 call, so intercept only that proc lookup. */
static PFN_vkGetInstanceProcAddr queue_compat_get_instance_proc_addr;
static PFN_vkGetDeviceProcAddr queue_compat_get_device_proc_addr;
static PFN_vkGetDeviceQueue2 queue_compat_get_device_queue2;
static VkDeviceQueueCreateFlags queue_compat_flags;

static VKAPI_ATTR void VKAPI_CALL
queue_compat_get_device_queue(VkDevice device, uint32_t family, uint32_t index,
                              VkQueue *queue)
{
    const VkDeviceQueueInfo2 info = {
        .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_INFO_2,
        .flags = queue_compat_flags,
        .queueFamilyIndex = family,
        .queueIndex = index,
    };
    queue_compat_get_device_queue2(device, &info, queue);
}

static VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
queue_compat_device_proc_addr(VkDevice device, const char *name)
{
    if (!strcmp(name, "vkGetDeviceQueue"))
        return (PFN_vkVoidFunction) queue_compat_get_device_queue;
    return queue_compat_get_device_proc_addr(device, name);
}

static VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL
queue_compat_instance_proc_addr(VkInstance instance, const char *name)
{
    if (!strcmp(name, "vkGetDeviceProcAddr"))
        return (PFN_vkVoidFunction) queue_compat_device_proc_addr;
    return queue_compat_get_instance_proc_addr(instance, name);
}
#endif

static bool excluded_extension(const char *extension, const char *first,
                               const char *second)
{
    return (first && !strcmp(extension, first)) ||
           (second && !strcmp(extension, second));
}

static char *join_extensions(const char *const *first, size_t first_count,
                             const char *const *second, size_t second_count,
                             const char *prefix, const char *excluded_first,
                             const char *excluded_second)
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
        if (excluded_extension(first[i], excluded_first, excluded_second))
            continue;
        if (result[0])
            strcat(result, "+");
        strcat(result, first[i]);
    }
    for (size_t i = 0; i < second_count; i++) {
        if (excluded_extension(second[i], excluded_first, excluded_second))
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
                                    NULL, 0, NULL, NULL, NULL);
    device_list = join_extensions(pl_vulkan_recommended_extensions,
                                  pl_vulkan_num_recommended_extensions,
                                  NULL, 0, VK_KHR_SWAPCHAIN_EXTENSION_NAME,
                                  VK_KHR_PORTABILITY_SUBSET_EXTENSION_NAME,
                                  VK_EXT_HOST_IMAGE_COPY_EXTENSION_NAME);
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

    PFN_vkGetInstanceProcAddr placebo_get_proc_addr = vulkan->get_proc_addr;
#if LIBAVUTIL_VERSION_MAJOR >= 61 && PL_API_VER < 365
    if (vulkan->queue_flags) {
        queue_compat_get_instance_proc_addr = vulkan->get_proc_addr;
        queue_compat_get_device_proc_addr = (PFN_vkGetDeviceProcAddr)
            vulkan->get_proc_addr(vulkan->inst, "vkGetDeviceProcAddr");
        if (queue_compat_get_device_proc_addr)
            queue_compat_get_device_queue2 = (PFN_vkGetDeviceQueue2)
                queue_compat_get_device_proc_addr(vulkan->act_dev,
                                                  "vkGetDeviceQueue2");
        if (!queue_compat_get_device_proc_addr ||
            !queue_compat_get_device_queue2) {
            set_error(renderer,
                      "Vulkan queue compatibility functions are unavailable");
            goto fail;
        }
        queue_compat_flags = vulkan->queue_flags;
        placebo_get_proc_addr = queue_compat_instance_proc_addr;
    }
#endif

    struct pl_vulkan_import_params import = {
        .instance = vulkan->inst,
        .get_proc_addr = placebo_get_proc_addr,
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
        if (queue->flags & VK_QUEUE_GRAPHICS_BIT) {
            import.queue_graphics = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
#if LIBAVUTIL_VERSION_MAJOR >= 61 && PL_API_VER >= 365
            import.queue_graphics.flags = vulkan->queue_flags;
#endif
        }
        if (queue->flags & VK_QUEUE_COMPUTE_BIT) {
            import.queue_compute = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
#if LIBAVUTIL_VERSION_MAJOR >= 61 && PL_API_VER >= 365
            import.queue_compute.flags = vulkan->queue_flags;
#endif
        }
        if (queue->flags & VK_QUEUE_TRANSFER_BIT) {
            import.queue_transfer = (struct pl_vulkan_queue) {
                .index = queue->idx, .count = queue->num,
            };
#if LIBAVUTIL_VERSION_MAJOR >= 61 && PL_API_VER >= 365
            import.queue_transfer.flags = vulkan->queue_flags;
#endif
        }
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
    if (!mask_format ||
        !(renderer->solid_texture = pl_tex_create(
              renderer->vulkan->gpu,
              pl_tex_params(.w = 1, .h = 1, .format = mask_format,
                            .sampleable = true, .initial_data = &white)))) {
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

static pl_rect2df fitted_video_rect(const struct pl_frame *image,
                                     AVRational sample_aspect, int width, int height,
                                     uint32_t integer_scale)
{
    if (integer_scale) {
        double sw = fabs(image->crop.x1 - image->crop.x0);
        double sh = fabs(image->crop.y1 - image->crop.y0);
        if (image->rotation % 2) {
            double temporary = sw;
            sw = sh;
            sh = temporary;
        }
        const double dw = sw * integer_scale, dh = sh * integer_scale;
        // Whole-pixel origins avoid sampling between pixels in odd-sized windows.
        const double x = floor((width - dw) * 0.5);
        const double y = floor((height - dh) * 0.5);
        return (pl_rect2df) {x, y, x + dw, y + dh};
    }
    pl_rect2df rect = { .x1 = width, .y1 = height };
    double aspect = pl_rect2df_aspect(&image->crop);
    if (sample_aspect.num > 0 && sample_aspect.den > 0)
        aspect *= (double) sample_aspect.num / sample_aspect.den;
    if (isfinite(aspect) && aspect > 0.0)
        pl_rect2df_aspect_set_rot(&rect, aspect, image->rotation, 0.0f);
    return rect;
}

int up_video_renderer_display(UpVideoRenderer *renderer, void *frame_pointer,
                              int width, int height, const UpOverlayFrame *overlay,
                              const char *subtitle_text,
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
    int ret = -1;

    if (!renderer || !renderer->renderer || !frame || width <= 0 || height <= 0)
        return -1;

    // Reserve two slots for subtitle text and its background.
    if (!overlay || overlay->count > 14 || (overlay->count && !overlay->parts)) {
        set_error(renderer, "invalid overlay geometry");
        return -1;
    }
    for (size_t i = 0; i < overlay->count; i++) {
        if (overlay->parts[i].texture > 2) {
            set_error(renderer, "invalid overlay texture index");
            return -1;
        }
    }
    if (!upload_overlay_image(renderer, 0, &overlay->text) ||
        !upload_overlay_image(renderer, 1, &overlay->title))
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
    const uint32_t integer_scale = up_video_integer_scale(
        fabs(image.crop.x1 - image.crop.x0), fabs(image.crop.y1 - image.crop.y0),
        frame->sample_aspect_ratio.num, frame->sample_aspect_ratio.den,
        image.rotation, width, height);
    target.crop = fitted_video_rect(&image, frame->sample_aspect_ratio, width, height,
                                    integer_scale);
    const float x0 = target.crop.x0, y0 = target.crop.y0;
    const float x1 = target.crop.x1, y1 = target.crop.y1;

    if (subtitle_text && *subtitle_text) {
        int layout_width = (int) (x1 - x0) - 80;
        layout_width = layout_width > 120 ? layout_width : 120;
        if (!update_text_subtitle_texture(renderer, subtitle_text, layout_width,
                                          subtitle_serial))
            goto out;
    }

    for (size_t i = 0; i < overlay->count; i++) {
        const UpOverlayPart *part = &overlay->parts[i];
        parts[num_overlays] = (struct pl_overlay_part) {
            .src = {part->src[0], part->src[1], part->src[2], part->src[3]},
            .dst = {part->dst[0], part->dst[1], part->dst[2], part->dst[3]},
            .color = {part->color[0], part->color[1], part->color[2], part->color[3]},
        };
        overlays[num_overlays] = (struct pl_overlay) {
            .tex = part->texture == 0 ? renderer->solid_texture :
                   renderer->overlay_textures[part->texture - 1],
            .mode = PL_OVERLAY_MONOCHROME,
            .coords = PL_OVERLAY_COORDS_DST_FRAME,
            .repr = pl_color_repr_rgb,
            .color = pl_color_space_srgb,
            .parts = &parts[num_overlays], .num_parts = 1,
        };
        overlays[num_overlays].repr.alpha = PL_ALPHA_INDEPENDENT;
        num_overlays++;
    }

    if (subtitle_text && *subtitle_text && renderer->text_subtitle_texture &&
        renderer->text_subtitle_visible) {
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
    if (integer_scale)
        params.upscaler = &pl_filter_nearest;
    else if (frame->width <= 1280 && frame->height <= 720)
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
        for (size_t i = 0; i < 2; i++)
            pl_tex_destroy(renderer->vulkan->gpu, &renderer->overlay_textures[i]);
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
