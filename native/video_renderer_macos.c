// FFmpeg/VideoToolbox frames -> floating-point Metal -> HDR-aware SDL output.
// Keep the existing renderer ABI so playback, timing and overlays stay shared.
#include "video_renderer.h"
#include "video_color_macos.h"
#include "video_upscaler_macos.h"

#include <SDL3/SDL.h>
#include <libavutil/display.h>
#include <libavutil/frame.h>
#include <libavutil/imgutils.h>
#include <libavutil/mem.h>
#include <libavutil/hwcontext.h>
#include <libavutil/mastering_display_metadata.h>
#include <libswscale/swscale.h>
#include <pango/pangocairo.h>

#include <math.h>
#include <stdio.h>
#include <string.h>

typedef struct Image {
    SDL_Texture *texture;
    uint64_t serial;
    bool uploaded;
} Image;

struct UpVideoRenderer {
    SDL_Renderer *renderer;
    SDL_Texture *video;
    struct SwsContext *conversion;
    AVFrame *rgba;
    AVBufferRef *hw_device;
    UpMetalColor *color;
    UpVideoUpscaler *upscaler;
    bool upscale_enabled;
    AVFrame *converted_frame;
    int converted_field;
    int converted_width, converted_height;
    int render_width, render_height;
    bool output_reported;
    float output_headroom;
    Image overlays[2], bitmap, subtitle;
    int subtitle_layout_width;
    bool subtitle_visible;
    char error[256];
};

static int fail(UpVideoRenderer *renderer, const char *message)
{
    snprintf(renderer->error, sizeof(renderer->error), "%s", message);
    return -1;
}

static bool texture_size(const SDL_Texture *texture, int width, int height)
{
    return texture && texture->w == width && texture->h == height;
}

static bool make_texture(UpVideoRenderer *renderer, SDL_Texture **texture,
                         int width, int height, SDL_BlendMode blend)
{
    if (texture_size(*texture, width, height) && (*texture)->format == SDL_PIXELFORMAT_RGBA32)
        return true;
    SDL_DestroyTexture(*texture);
    *texture = SDL_CreateTexture(renderer->renderer, SDL_PIXELFORMAT_RGBA32,
                                 SDL_TEXTUREACCESS_STREAMING, width, height);
    return *texture && SDL_SetTextureBlendMode(*texture, blend);
}

// Upload straight-alpha RGBA, expanding the player's A8 masks to white+alpha.
static bool upload_image(UpVideoRenderer *renderer, Image *image,
                          const uint8_t *pixels, int width, int height,
                          int stride, bool mask, uint64_t serial)
{
    if (image->uploaded && texture_size(image->texture, width, height) && image->serial == serial)
        return true;
    image->uploaded = false;
    if (!pixels || width <= 0 || height <= 0 ||
        !make_texture(renderer, &image->texture, width, height, SDL_BLENDMODE_BLEND))
        return false;
    if (mask) {
        void *destination;
        int pitch;
        if (!SDL_LockTexture(image->texture, NULL, &destination, &pitch))
            return false;
        for (int y = 0; y < height; y++) {
            uint8_t *row = (uint8_t *) destination + (size_t) y * pitch;
            for (int x = 0; x < width; x++) {
                row[x * 4] = row[x * 4 + 1] = row[x * 4 + 2] = 255;
                row[x * 4 + 3] = pixels[(size_t) y * stride + x];
            }
        }
        SDL_UnlockTexture(image->texture);
    } else if (!SDL_UpdateTexture(image->texture, NULL, pixels, stride)) {
        return false;
    }
    image->serial = serial;
    image->uploaded = true;
    return true;
}

static bool upload_subtitle(UpVideoRenderer *renderer, const char *text,
                             int layout_width, uint64_t serial)
{
    if (renderer->subtitle.serial == serial &&
        renderer->subtitle_layout_width == layout_width)
        return true;
    cairo_surface_t *measure_surface = cairo_image_surface_create(CAIRO_FORMAT_A8, 1, 1);
    cairo_t *measure = cairo_create(measure_surface);
    PangoLayout *layout = pango_cairo_create_layout(measure);
    PangoFontDescription *font = pango_font_description_from_string("monospace Bold");
    pango_font_description_set_absolute_size(font, 18 * PANGO_SCALE);
    pango_layout_set_font_description(layout, font);
    pango_layout_set_text(layout, text, -1);
    pango_layout_set_width(layout, layout_width * PANGO_SCALE);
    pango_layout_set_wrap(layout, PANGO_WRAP_WORD_CHAR);
    pango_layout_set_alignment(layout, PANGO_ALIGN_CENTER);
    pango_layout_set_spacing(layout, 8 * PANGO_SCALE);
    pango_layout_set_height(layout, -4);
    pango_layout_set_ellipsize(layout, PANGO_ELLIPSIZE_END);
    PangoRectangle ink;
    pango_layout_get_pixel_extents(layout, &ink, NULL);
    bool success = true;
    renderer->subtitle_visible = ink.width > 0 && ink.height > 0;
    if (renderer->subtitle_visible) {
        // The same cue can be reflowed on resize without changing its serial.
        renderer->subtitle.uploaded = false;
        // Account for glyph overhangs as well as logical layout bounds.
        cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_A8,
                                                              ink.width + 4, ink.height + 4);
        cairo_t *context = cairo_create(surface);
        cairo_set_source_rgba(context, 1, 1, 1, 1);
        cairo_move_to(context, 2 - ink.x, 2 - ink.y);
        pango_cairo_show_layout(context, layout);
        cairo_surface_flush(surface);
        success = cairo_status(context) == CAIRO_STATUS_SUCCESS &&
                  cairo_surface_status(surface) == CAIRO_STATUS_SUCCESS &&
                  upload_image(renderer, &renderer->subtitle,
                      cairo_image_surface_get_data(surface), ink.width + 4, ink.height + 4,
                      cairo_image_surface_get_stride(surface), true, serial);
        cairo_destroy(context);
        cairo_surface_destroy(surface);
    }
    pango_font_description_free(font);
    g_object_unref(layout);
    cairo_destroy(measure);
    cairo_surface_destroy(measure_surface);
    if (success) {
        renderer->subtitle.serial = serial;
        renderer->subtitle_layout_width = layout_width;
    }
    return success;
}

// Spatial bob for software SDR frames; hardware/HDR bob runs on the GPU.
static void deinterlace_rgba(AVFrame *frame, int field)
{
    if (!field || frame->height < 2)
        return;
    const int parity = field == 2 ? 1 : 0;
    for (int y = 1 - parity; y < frame->height; y += 2) {
        const int above = y > 0 ? y - 1 : y + 1;
        const int below = y + 1 < frame->height ? y + 1 : y - 1;
        const uint8_t *a = frame->data[0] + (size_t) above * frame->linesize[0];
        const uint8_t *b = frame->data[0] + (size_t) below * frame->linesize[0];
        uint8_t *row = frame->data[0] + (size_t) y * frame->linesize[0];
        for (int x = 0; x < frame->width * 4; x++)
            row[x] = ((unsigned int) a[x] + b[x] + 1) / 2;
    }
}

static int source_colorspace(const AVFrame *frame)
{
    switch (frame->colorspace) {
    case AVCOL_SPC_BT709: return SWS_CS_ITU709;
    case AVCOL_SPC_FCC: return SWS_CS_FCC;
    case AVCOL_SPC_SMPTE240M: return SWS_CS_SMPTE240M;
    case AVCOL_SPC_BT2020_NCL:
    case AVCOL_SPC_BT2020_CL: return SWS_CS_BT2020;
    case AVCOL_SPC_BT470BG:
    case AVCOL_SPC_SMPTE170M: return SWS_CS_ITU601;
    default: return frame->width >= 1280 || frame->height > 576 ? SWS_CS_ITU709 : SWS_CS_ITU601;
    }
}

static float hdr_peak(const AVFrame *frame)
{
    const AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_CONTENT_LIGHT_LEVEL);
    if (side && side->size >= sizeof(AVContentLightMetadata)) {
        const AVContentLightMetadata *light = (const AVContentLightMetadata *) side->data;
        if (light->MaxCLL > 0 && light->MaxCLL <= 10000) return light->MaxCLL;
    }
    side = av_frame_get_side_data(frame, AV_FRAME_DATA_MASTERING_DISPLAY_METADATA);
    if (side && side->size >= sizeof(AVMasteringDisplayMetadata)) {
        const AVMasteringDisplayMetadata *master = (const AVMasteringDisplayMetadata *) side->data;
        const double peak = av_q2d(master->max_luminance);
        if (master->has_luminance && isfinite(peak) && peak > 0 && peak <= 10000) return peak;
    }
    return 1000.0f;
}

static void report_hdr(UpVideoRenderer *renderer, const AVFrame *frame)
{
    const bool hdr = frame->color_trc == AVCOL_TRC_SMPTE2084 || frame->color_trc == AVCOL_TRC_ARIB_STD_B67;
    if (hdr) {
        const SDL_PropertiesID properties = SDL_GetRendererProperties(renderer->renderer);
        const float headroom = SDL_GetFloatProperty(properties, SDL_PROP_RENDERER_HDR_HEADROOM_FLOAT, 1.0f);
        if (!renderer->output_reported || fabsf(headroom - renderer->output_headroom) > 0.5f) {
            fprintf(stderr, "hdr: %s -> %s, display headroom %.2fx, content peak %.0f nits\n",
                frame->color_trc == AVCOL_TRC_SMPTE2084 ? "PQ" : "HLG",
                SDL_GetBooleanProperty(properties, SDL_PROP_RENDERER_HDR_ENABLED_BOOLEAN, false) ?
                "Metal EDR" : "SDR tone mapping", headroom, hdr_peak(frame));
            renderer->output_reported = true;
            renderer->output_headroom = headroom;
        }
    }
}

static bool upload_video(UpVideoRenderer *renderer, const AVFrame *frame, int field)
{
    if (!frame || frame->width <= 0 || frame->height <= 0)
        return false;
    const bool hdr = frame->color_trc == AVCOL_TRC_SMPTE2084 || frame->color_trc == AVCOL_TRC_ARIB_STD_B67;
    if (frame->format == AV_PIX_FMT_VIDEOTOOLBOX || hdr) {
        const int width = renderer->render_width > 0 ? renderer->render_width : frame->width;
        const int height = renderer->render_height > 0 ? renderer->render_height : frame->height;
        const AVFrame *previous = renderer->converted_frame;
        if (previous && renderer->converted_width == width && renderer->converted_height == height && renderer->converted_field == field &&
            previous->data[0] == frame->data[0] && previous->data[3] == frame->data[3] &&
            previous->width == frame->width && previous->height == frame->height &&
            previous->format == frame->format && previous->pts == frame->pts &&
            previous->color_trc == frame->color_trc && previous->color_range == frame->color_range &&
            previous->color_primaries == frame->color_primaries && previous->colorspace == frame->colorspace &&
            hdr_peak(previous) == hdr_peak(frame))
            return true;
        if (!renderer->color)
            renderer->color = up_metal_color_create(renderer->renderer, renderer->error, sizeof(renderer->error));
        if (!renderer->color) return false;
        SDL_Texture *texture = up_metal_color_convert(renderer->color, renderer->renderer,
            frame, field, hdr_peak(frame), width, height, renderer->error, sizeof(renderer->error));
        if (!texture) return false;
        SDL_DestroyTexture(renderer->video);
        renderer->video = texture;
        av_frame_free(&renderer->converted_frame);
        renderer->converted_frame = av_frame_clone(frame);
        renderer->converted_field = field;
        renderer->converted_width = width;
        renderer->converted_height = height;
        report_hdr(renderer, frame);
        return true;
    }
    av_frame_free(&renderer->converted_frame);
    if (!renderer->rgba || renderer->rgba->width != frame->width ||
        renderer->rgba->height != frame->height) {
        av_frame_free(&renderer->rgba);
        renderer->rgba = av_frame_alloc();
        if (!renderer->rgba)
            return false;
        renderer->rgba->width = frame->width;
        renderer->rgba->height = frame->height;
        renderer->rgba->format = AV_PIX_FMT_RGBA;
        if (av_frame_get_buffer(renderer->rgba, 32) < 0) {
            av_frame_free(&renderer->rgba);
            return false;
        }
    }
    renderer->conversion = sws_getCachedContext(renderer->conversion,
        frame->width, frame->height, frame->format,
        frame->width, frame->height, AV_PIX_FMT_RGBA, SWS_BILINEAR, NULL, NULL, NULL);
    if (!renderer->conversion)
        return false;
    const int *coefficients = sws_getCoefficients(source_colorspace(frame));
    if (sws_setColorspaceDetails(renderer->conversion, coefficients,
            frame->color_range == AVCOL_RANGE_JPEG, coefficients, 1, 0, 1 << 16, 1 << 16) < 0)
        return false;
    if (sws_scale(renderer->conversion, (const uint8_t *const *) frame->data,
            frame->linesize, 0, frame->height, renderer->rgba->data,
            renderer->rgba->linesize) != frame->height)
        return false;
    deinterlace_rgba(renderer->rgba, field);
    return make_texture(renderer, &renderer->video, frame->width, frame->height,
                         SDL_BLENDMODE_NONE) &&
           SDL_UpdateTexture(renderer->video, NULL, renderer->rgba->data[0],
                              renderer->rgba->linesize[0]);
}

static uint32_t video_rotation(const AVFrame *frame)
{
    const AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_DISPLAYMATRIX);
    if (!side || side->size < 9 * sizeof(int32_t))
        return 0;
    const double degrees = -av_display_rotation_get((const int32_t *) side->data);
    if (!isfinite(degrees))
        return 0;
    return ((int) lround(degrees / 90.0) % 4 + 4) % 4;
}

static SDL_FRect video_source_rect(const AVFrame *frame, const UpVideoCrop *crop)
{
    // Honor decoder padding and clean-aperture cropping before user autocrop.
    float left = 0, top = 0, right = frame->width, bottom = frame->height;
    if (frame->crop_left < (size_t) frame->width && frame->crop_right < (size_t) frame->width &&
        frame->crop_top < (size_t) frame->height && frame->crop_bottom < (size_t) frame->height) {
        left = frame->crop_left; top = frame->crop_top;
        right -= frame->crop_right; bottom -= frame->crop_bottom;
    }
    if (left >= right || top >= bottom) {
        left = top = 0;
        right = frame->width;
        bottom = frame->height;
    }
    if (crop && crop->width == frame->width && crop->height == frame->height &&
        crop->left >= 0 && crop->top >= 0 && crop->right <= frame->width &&
        crop->bottom <= frame->height && crop->left < crop->right && crop->top < crop->bottom) {
        const float x0 = fmaxf(left, crop->left), y0 = fmaxf(top, crop->top);
        const float x1 = fminf(right, crop->right), y1 = fminf(bottom, crop->bottom);
        if (x0 < x1 && y0 < y1) {
            left = x0; top = y0; right = x1; bottom = y1;
        }
    }
    return (SDL_FRect) {left, top, right - left, bottom - top};
}

static SDL_FRect fit_rect(double source_width, double source_height,
                           AVRational sar, uint32_t rotation, int width, int height,
                           uint32_t integer_scale)
{
    double sw = source_width, sh = source_height;
    if (!integer_scale && sar.num > 0 && sar.den > 0)
        sw *= (double) sar.num / sar.den;
    if (rotation % 2) {
        double temporary = sw; sw = sh; sh = temporary;
    }
    const double scale = integer_scale ? integer_scale : fmin(width / sw, height / sh);
    const float w = sw * scale, h = sh * scale;
    float x = (width - w) * 0.5f, y = (height - h) * 0.5f;
    if (integer_scale) { x = floorf(x); y = floorf(y); }
    return (SDL_FRect) {x, y, w, h};
}

// SDL rotates the destination rectangle around its center, so undo the size
// swap before submitting a portrait frame.
static bool draw_rotated(UpVideoRenderer *renderer, SDL_Texture *texture,
                           const SDL_FRect *source, SDL_FRect destination,
                           uint32_t rotation)
{
    if (rotation % 2) {
        const float difference = (destination.w - destination.h) * 0.5f;
        destination.x += difference;
        destination.y -= difference;
        const float temporary = destination.w;
        destination.w = destination.h;
        destination.h = temporary;
    }
    return SDL_RenderTextureRotated(renderer->renderer, texture, source, &destination,
                                    rotation * 90.0, NULL, SDL_FLIP_NONE);
}

static SDL_FRect part_rect(const float rect[4])
{
    return (SDL_FRect) {rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1]};
}

static bool draw_overlays(UpVideoRenderer *renderer, const UpOverlayFrame *overlay)
{
    if (!overlay)
        return true;
    const UpOverlayImage *images[] = {&overlay->text, &overlay->title};
    for (size_t i = 0; i < 2; i++) {
        const UpOverlayImage *image = images[i];
        if (image->pixels && image->width > 0 && image->height > 0 &&
            !upload_image(renderer, &renderer->overlays[i], image->pixels,
                image->width, image->height, image->width, true, image->serial))
            return false;
    }
    for (size_t i = 0; i < overlay->count; i++) {
        const UpOverlayPart *part = &overlay->parts[i];
        SDL_FRect source = part_rect(part->src), destination = part_rect(part->dst);
        if (destination.w <= 0 || destination.h <= 0)
            continue;
        if (!part->texture) {
            if (!SDL_SetRenderDrawColorFloat(renderer->renderer, part->color[0], part->color[1],
                    part->color[2], part->color[3]) ||
                !SDL_RenderFillRect(renderer->renderer, &destination))
                return false;
        } else {
            if (part->texture > 2)
                return false;
            SDL_Texture *texture = renderer->overlays[part->texture - 1].texture;
            if (!texture || !SDL_SetTextureColorModFloat(texture, part->color[0],
                    part->color[1], part->color[2]) ||
                !SDL_SetTextureAlphaModFloat(texture, part->color[3]) ||
                !SDL_SetTextureScaleMode(texture, SDL_SCALEMODE_NEAREST) ||
                !SDL_RenderTexture(renderer->renderer, texture, &source, &destination))
                return false;
        }
    }
    return true;
}

UpVideoRenderer *up_video_renderer_create(void *window)
{
    UpVideoRenderer *renderer = av_mallocz(sizeof(*renderer));
    if (!renderer)
        return NULL;
    renderer->upscale_enabled = true;
    SDL_PropertiesID properties = SDL_CreateProperties();
    SDL_SetPointerProperty(properties, SDL_PROP_RENDERER_CREATE_WINDOW_POINTER, window);
    SDL_SetStringProperty(properties, SDL_PROP_RENDERER_CREATE_NAME_STRING, "metal");
    SDL_SetNumberProperty(properties, SDL_PROP_RENDERER_CREATE_OUTPUT_COLORSPACE_NUMBER, SDL_COLORSPACE_SRGB_LINEAR);
    renderer->renderer = SDL_CreateRendererWithProperties(properties);
    SDL_DestroyProperties(properties);
    if (!renderer->renderer ||
        !SDL_SetRenderDrawBlendMode(renderer->renderer, SDL_BLENDMODE_BLEND) ||
        !SDL_SetRenderVSync(renderer->renderer, 1))
        fail(renderer, SDL_GetError());
    // Hardware decode is optional; presentation remains available if the
    // machine/FFmpeg build cannot initialize VideoToolbox.
    int result = av_hwdevice_ctx_create(&renderer->hw_device, AV_HWDEVICE_TYPE_VIDEOTOOLBOX, NULL, NULL, 0);
    if (result < 0)
        fprintf(stderr, "warning: VideoToolbox unavailable; using software decoding\n");
    return renderer;
}

int up_video_renderer_ready(const UpVideoRenderer *renderer)
{
    return renderer && renderer->renderer && !renderer->error[0];
}

void *up_video_renderer_device(UpVideoRenderer *renderer)
{
    return renderer->hw_device;
}

int up_video_renderer_display(UpVideoRenderer *renderer, void *frame_pointer,
                              void *previous_pointer, void *next_pointer, int field,
                              int width, int height, const UpOverlayFrame *overlay,
                              const UpVideoCrop *crop, int fill,
                              const char *subtitle_text, const uint8_t *subtitle_pixels,
                              int subtitle_width, int subtitle_height, uint64_t subtitle_serial,
                              float corner_radius, const char *corner_shader)
{
    (void) previous_pointer; (void) next_pointer; (void) corner_shader; (void) corner_radius;
    const AVFrame *frame = frame_pointer;
    if (width <= 0 || height <= 0)
        return 0;
    renderer->error[0] = '\0';
    if (!frame || frame->width <= 0 || frame->height <= 0)
        return fail(renderer, "invalid video frame");
    const uint32_t rotation = video_rotation(frame);
    SDL_FRect source = video_source_rect(frame, crop);
    if (fill) {
        float rect[] = {source.x, source.y, source.x + source.w, source.y + source.h};
        up_video_fill_crop(rect, frame->sample_aspect_ratio.num, frame->sample_aspect_ratio.den,
                            rotation, width, height);
        source = part_rect(rect);
    }
    const uint32_t integer_scale = fill ? 0 : up_video_integer_scale(source.w, source.h,
        frame->sample_aspect_ratio.num, frame->sample_aspect_ratio.den, rotation, width, height);
    const SDL_FRect destination = fit_rect(source.w, source.h, frame->sample_aspect_ratio,
                                            rotation, width, height, integer_scale);
    const float dw = rotation % 2 ? destination.h : destination.w;
    const float dh = rotation % 2 ? destination.w : destination.h;
    const float scale = fminf(1, fmaxf(dw / source.w, dh / source.h));
    renderer->render_width = (int) ceilf(frame->width * scale);
    renderer->render_height = (int) ceilf(frame->height * scale);
    if (!renderer->upscaler)
        renderer->upscaler = up_video_upscaler_create(SDL_GetWindowID(SDL_GetRenderWindow(renderer->renderer)));
    const float sar = frame->sample_aspect_ratio.num > 0 && frame->sample_aspect_ratio.den > 0 ?
        av_q2d(frame->sample_aspect_ratio) : 1;
    const bool larger = !integer_scale && dw > source.w * sar * 1.05f && dh > source.h * 1.05f;
    const AVFrame *draw_frame = integer_scale ? frame : up_video_upscaler_frame(renderer->upscaler, frame,
        field, renderer->upscale_enabled, larger);
    const bool direct = !integer_scale && up_metal_color_can_draw(draw_frame);
    if (direct && !renderer->color)
        renderer->color = up_metal_color_create(renderer->renderer, renderer->error, sizeof(renderer->error));
    if ((direct && !renderer->color) || (!direct && !upload_video(renderer, frame, field))) {
        if (!renderer->error[0]) fail(renderer, "could not convert or upload the video frame for Metal");
        return -1;
    }
    if (!SDL_SetRenderDrawColor(renderer->renderer, 0, 0, 0, 255) ||
        !SDL_RenderClear(renderer->renderer))
        return fail(renderer, SDL_GetError());
    if (direct) {
        report_hdr(renderer, frame);
        SDL_FRect draw_source = {source.x * draw_frame->width / frame->width,
            source.y * draw_frame->height / frame->height, source.w * draw_frame->width / frame->width,
            source.h * draw_frame->height / frame->height};
        if (!up_metal_color_draw(renderer->color, renderer->renderer, draw_frame, field, hdr_peak(frame),
                draw_source, destination, rotation, width, height, renderer->error, sizeof(renderer->error)))
            return -1;
    } else {
        const SDL_FRect sampled_source = {source.x * renderer->video->w / frame->width,
            source.y * renderer->video->h / frame->height, source.w * renderer->video->w / frame->width,
            source.h * renderer->video->h / frame->height};
        if (!SDL_SetTextureScaleMode(renderer->video, integer_scale ? SDL_SCALEMODE_NEAREST : SDL_SCALEMODE_LINEAR) ||
            !draw_rotated(renderer, renderer->video, &sampled_source, destination, rotation))
            return fail(renderer, SDL_GetError());
    }
    if (subtitle_pixels && subtitle_width > 0 && subtitle_height > 0) {
        // Fit the entire authored canvas within the visible video; encoded
        // letterboxes can contain captions even after autocrop removes them.
        if (!upload_image(renderer, &renderer->bitmap, subtitle_pixels, subtitle_width,
                subtitle_height, subtitle_width * 4, false, subtitle_serial))
            return fail(renderer, "could not upload the bitmap subtitle");
        const double sx = source.x, sy = source.y;
        const double aspect = (double) frame->width / frame->height;
        const double bw = fmin(source.w, source.h * aspect);
        const double bh = bw / aspect;
        SDL_FRect canvas = {(float) (sx + (source.w - bw) * 0.5),
                            (float) (sy + (source.h - bh) * 0.5), bw, bh};
        // Map source coordinates into the pre-rotation destination rectangle.
        SDL_FRect unrotated = destination;
        if (rotation % 2) { unrotated.w = destination.h; unrotated.h = destination.w; }
        const SDL_FRect subtitle_destination = {
            destination.x + (canvas.x - source.x) / source.w * unrotated.w,
            destination.y + (canvas.y - source.y) / source.h * unrotated.h,
            canvas.w / source.w * unrotated.w, canvas.h / source.h * unrotated.h};
        // Both rectangles are centered, including after a quarter turn.
        SDL_FRect centered = subtitle_destination;
        if (rotation % 2) {
            centered.w = subtitle_destination.h; centered.h = subtitle_destination.w;
            centered.x = destination.x + (destination.w - centered.w) * 0.5f;
            centered.y = destination.y + (destination.h - centered.h) * 0.5f;
        }
        if (!draw_rotated(renderer, renderer->bitmap.texture, NULL, centered, rotation))
            return fail(renderer, SDL_GetError());
    }
    if (!draw_overlays(renderer, overlay))
        return fail(renderer, "could not draw the player overlays");
    if (subtitle_text && *subtitle_text) {
        const int layout_width = (int) fmaxf(120, destination.w - 80);
        if (!upload_subtitle(renderer, subtitle_text, layout_width, subtitle_serial))
            return fail(renderer, "could not rasterize the text subtitle");
        if (renderer->subtitle_visible) {
            SDL_Texture *texture = renderer->subtitle.texture;
            const float text_width = texture->w, text_height = texture->h;
            const float bottom = fmaxf(destination.y + text_height + 8,
                                       destination.y + destination.h - 70);
            const SDL_FRect text = {(width - text_width) * 0.5f, bottom - text_height,
                                    text_width, text_height};
            const SDL_FRect background = {text.x - 8, text.y - 5, text.w + 16, text.h + 10};
            if (!SDL_SetRenderDrawColorFloat(renderer->renderer, 0, 0, 0, 0.72f) ||
                !SDL_RenderFillRect(renderer->renderer, &background) ||
                !SDL_RenderTexture(renderer->renderer, texture, NULL, &text))
                return fail(renderer, SDL_GetError());
        }
    }
    const bool presented = SDL_RenderPresent(renderer->renderer);
    up_metal_color_after_present(renderer->color);
    if (!presented)
        return fail(renderer, SDL_GetError());
    // SDL applies the window's alpha shape after the complete HDR composition.
    return 0;
}

int up_video_renderer_resize(UpVideoRenderer *renderer, int width, int height)
{
    (void) renderer; (void) width; (void) height;
    // SDL tracks the drawable size and backing scale as the window changes.
    return 0;
}

const char *up_video_renderer_error(const UpVideoRenderer *renderer)
{
    return renderer->error[0] ? renderer->error : "unknown Metal renderer error";
}

void up_video_renderer_set_upscaling(UpVideoRenderer *renderer, bool enabled)
{
    renderer->upscale_enabled = enabled;
}

const char *up_video_renderer_upscaling_status(const UpVideoRenderer *renderer)
{
    return renderer->upscale_enabled ? up_video_upscaler_status(renderer->upscaler) : "OFF";
}

void up_video_renderer_destroy(UpVideoRenderer *renderer)
{
    if (!renderer)
        return;
    SDL_DestroyTexture(renderer->video);
    for (size_t i = 0; i < 2; i++)
        SDL_DestroyTexture(renderer->overlays[i].texture);
    SDL_DestroyTexture(renderer->bitmap.texture);
    SDL_DestroyTexture(renderer->subtitle.texture);
    av_frame_free(&renderer->rgba);
    av_frame_free(&renderer->converted_frame);
    av_buffer_unref(&renderer->hw_device);
    up_metal_color_destroy(renderer->color);
    up_video_upscaler_destroy(renderer->upscaler);
    sws_freeContext(renderer->conversion);
    SDL_DestroyRenderer(renderer->renderer);
    av_free(renderer);
}
