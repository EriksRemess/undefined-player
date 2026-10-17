// Exercise the macOS renderer with SDL's CPU surface backend, without a GPU
// or desktop session. Rust tests cover the shared fill/integer-scaling policy.
#include "video_renderer_macos.c"
#include "window_shape.h"
#include <assert.h>
#include <CoreVideo/CoreVideo.h>
#include "ffmpeg_luma.h"

static void release_pixel_buffer(void *opaque, uint8_t *data)
{
    (void) data;
    CVPixelBufferRelease((CVPixelBufferRef) opaque);
}

static AVFrame *hardware_picture(const AVFrame *source)
{
    CVPixelBufferRef buffer = NULL;
    const void *keys[] = {kCVPixelBufferIOSurfacePropertiesKey};
    CFDictionaryRef empty = CFDictionaryCreate(NULL, NULL, NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *values[] = {empty};
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const bool full = source->color_range == AVCOL_RANGE_JPEG;
    const bool ten = source->format == AV_PIX_FMT_P010;
    assert(CVPixelBufferCreate(NULL, source->width, source->height,
        ten ? (full ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange) :
              (full ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
        attributes, &buffer) == kCVReturnSuccess);
    CFRelease(attributes);
    CFRelease(empty);
    assert(CVPixelBufferLockBaseAddress(buffer, 0) == kCVReturnSuccess);
    for (int plane = 0; plane < 2; plane++) {
        const int rows = plane ? (source->height + 1) / 2 : source->height;
        for (int y = 0; y < rows; y++)
            memcpy((uint8_t *) CVPixelBufferGetBaseAddressOfPlane(buffer, plane) +
                y * CVPixelBufferGetBytesPerRowOfPlane(buffer, plane),
                source->data[plane] + y * source->linesize[plane], source->width * (ten ? 2 : 1));
    }
    CVPixelBufferUnlockBaseAddress(buffer, 0);
    AVFrame *frame = av_frame_alloc();
    assert(frame && av_frame_copy_props(frame, source) == 0);
    frame->width = source->width; frame->height = source->height;
    frame->format = AV_PIX_FMT_VIDEOTOOLBOX;
    frame->data[3] = (uint8_t *) buffer;
    frame->buf[0] = av_buffer_create((uint8_t *) buffer, 0, release_pixel_buffer, buffer, 0);
    assert(frame->buf[0]);
    return frame;
}

static void mapped_decoder_luma(void)
{
    for (int ten = 0; ten < 2; ten++) {
        for (int full = 0; full < 2; full++) {
            AVFrame *source = av_frame_alloc();
            assert(source);
            source->format = ten ? AV_PIX_FMT_P010 : AV_PIX_FMT_NV12;
            source->width = source->height = 16;
            source->color_range = full ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
            assert(av_frame_get_buffer(source, 32) == 0);
            for (int y = 0; y < 16; y++) {
                for (int x = 0; x < 16; x++) {
                    if (ten) ((uint16_t *) (source->data[0] + y * source->linesize[0]))[x] = (64 + x + y) << 6;
                    else source->data[0][y * source->linesize[0] + x] = 16 + x + y;
                }
            }
            AVFrame *hardware = hardware_picture(source);
            UpLumaView view;
            assert(up_luma_view(hardware, &view));
            // The view points at the original decoder plane, with no transfer
            // AVFrame, and keeps it alive after the caller releases its frame.
            assert(view.data == CVPixelBufferGetBaseAddressOfPlane((CVPixelBufferRef) hardware->data[3], 0));
            av_frame_free(&hardware);
            assert(view.width == 16 && view.height == 16 && view.full_range == full);
            assert(view.depth == (ten ? 10 : 8) && view.shift == (ten ? 6 : 0));
            const uint8_t *pixel = view.data + 7 * view.stride + 9 * view.step;
            assert(ten ? (*(const uint16_t *) pixel >> 6) == 80 : *pixel == 32);
            up_luma_view_free(&view);
            assert(!view.frame && !view.data);
            av_frame_free(&source);
        }
    }
}

static void rounded_window_alpha(void)
{
    const float radii[] = {0, 1, 15.5f, 32, 500};
    for (size_t i = 0; i < sizeof(radii) / sizeof(radii[0]); i++) {
        SDL_Surface *shape = up_corner_shape(128, 96, radii[i]);
        assert(shape);
        const uint8_t *pixels = shape->pixels;
        int antialiased = 0;
        for (int y = 0; y < shape->h; y++) {
            for (int x = 0; x < shape->w; x++) {
                const uint8_t *pixel = pixels + y * shape->pitch + x * 4;
                assert(pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255);
                assert(pixel[3] == pixels[y * shape->pitch + (127 - x) * 4 + 3]);
                assert(pixel[3] == pixels[(95 - y) * shape->pitch + x * 4 + 3]);
                antialiased += pixel[3] > 0 && pixel[3] < 255;
            }
        }
        assert(pixels[48 * shape->pitch + 64 * 4 + 3] == 255);
        assert(pixels[64 * 4 + 3] == 255);
        // At the maximum radius the two arcs meet at the side midpoint.
        assert(pixels[48 * shape->pitch + 3] >= 254);
        if (radii[i] == 0) assert(pixels[3] == 255 && antialiased == 0);
        if (radii[i] >= 15) assert(pixels[3] == 0 && antialiased > 0);
        SDL_DestroySurface(shape);
    }
}

static void expect_pixel(SDL_Renderer *renderer, int x, int y, int r, int g, int b)
{
    SDL_Surface *pixels = SDL_RenderReadPixels(renderer, NULL);
    assert(pixels);
    uint8_t actual_r, actual_g, actual_b, alpha;
    assert(SDL_ReadSurfacePixel(pixels, x, y, &actual_r, &actual_g, &actual_b, &alpha));
    if (abs(actual_r - r) > 3 || abs(actual_g - g) > 3 || abs(actual_b - b) > 3) {
        fprintf(stderr, "pixel %d,%d: got %d,%d,%d, expected %d,%d,%d\n",
                x, y, actual_r, actual_g, actual_b, r, g, b);
        abort();
    }
    SDL_DestroySurface(pixels);
}

static void aspect_crop_and_rotation(void)
{
    const AVFrame frame = {.width = 720, .height = 480};
    const UpVideoCrop crop = {720, 480, 0, 108, 720, 376};
    SDL_FRect source = video_source_rect(&frame, &crop);
    assert(source.y == 108 && source.h == 268);
    const UpVideoCrop stale = {1920, 1080, 0, 100, 1920, 980};
    source = video_source_rect(&frame, &stale);
    assert(source.w == 720 && source.h == 480);
    for (uint32_t rotation = 0; rotation < 4; rotation++) {
        SDL_FRect rectangle = fit_rect(720, 480, (AVRational) {8, 9}, rotation, 1280, 720, 0);
        const double expected = rotation % 2 ? 480.0 / 640 : 640.0 / 480;
        assert(fabs(rectangle.w / rectangle.h - expected) < 0.00001);
        assert(fabs(2 * rectangle.x + rectangle.w - 1280) < 0.001);
        assert(fabs(2 * rectangle.y + rectangle.h - 720) < 0.001);
        AVFrame *rotated = av_frame_alloc();
        AVFrameSideData *matrix = av_frame_new_side_data(rotated, AV_FRAME_DATA_DISPLAYMATRIX,
                                                         9 * sizeof(int32_t));
        assert(matrix);
        av_display_rotation_set((int32_t *) matrix->data, rotation * 90);
        assert(video_rotation(rotated) == rotation);
        av_frame_free(&rotated);
    }
    const SDL_FRect integer = fit_rect(248, 248, (AVRational) {1, 1}, 0, 1281, 721, 2);
    assert(integer.x == 392 && integer.y == 112 && integer.w == 496 && integer.h == 496);
}

static void conversion_and_overlays(UpVideoRenderer *renderer)
{
    AVFrame *frame = av_frame_alloc();
    assert(frame);
    frame->width = frame->height = 16;
    frame->format = AV_PIX_FMT_YUV420P;
    frame->colorspace = AVCOL_SPC_BT709;
    assert(av_frame_get_buffer(frame, 32) == 0);
    // Limited-range white, then black. Changes in range and source dimensions
    // must update the cached conversion and texture rather than retaining pixels.
    for (int value = 0; value < 2; value++) {
        frame->color_range = value ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
        for (int y = 0; y < 16; y++)
            memset(frame->data[0] + y * frame->linesize[0], value ? 0 : 235, 16);
        for (int y = 0; y < 8; y++) {
            memset(frame->data[1] + y * frame->linesize[1], 128, 8);
            memset(frame->data[2] + y * frame->linesize[2], 128, 8);
        }
        assert(upload_video(renderer, frame, 0));
        SDL_FRect destination = {0, 0, 128, 96};
        assert(draw_rotated(renderer, renderer->video, NULL, destination, 0));
        int expected = value ? 0 : 255;
        expect_pixel(renderer->renderer, 64, 48, expected, expected, expected);
    }
    const uint8_t mask[] = {255};
    const UpOverlayPart parts[] = {
        {.texture = 0, .dst = {0, 0, 32, 32}, .color = {1, 0, 0, 0.5}},
        {.texture = 1, .src = {0, 0, 1, 1}, .dst = {32, 0, 64, 32}, .color = {0, 1, 0, 1}},
    };
    UpOverlayFrame overlays = {.text = {mask, 1, 1, 1}, .parts = parts, .count = 2};
    assert(draw_overlays(renderer, &overlays));
    expect_pixel(renderer->renderer, 16, 16, 128, 0, 0);
    expect_pixel(renderer->renderer, 48, 16, 0, 255, 0);
    overlays.text.serial++;
    const uint8_t transparent[] = {0};
    overlays.text.pixels = transparent;
    assert(SDL_SetRenderDrawColor(renderer->renderer, 0, 0, 0, 255));
    assert(SDL_RenderClear(renderer->renderer));
    assert(draw_overlays(renderer, &overlays));
    // A new mask revision must replace previously uploaded opaque glyphs.
    expect_pixel(renderer->renderer, 48, 16, 0, 0, 0);
    assert(upload_subtitle(renderer, "Hello — Rīga", 100, 1));
    assert(renderer->subtitle_visible && renderer->subtitle.texture);
    assert(upload_subtitle(renderer, "Hello — Rīga", 120, 1));
    assert(renderer->subtitle_layout_width == 120);
    assert(upload_subtitle(renderer, "\342\200\213", 120, 2));
    assert(!renderer->subtitle_visible);
    av_frame_free(&frame);
}

static void rotated_pixels_and_bob(UpVideoRenderer *renderer)
{
    AVFrame *frame = av_frame_alloc();
    assert(frame);
    frame->width = 16; frame->height = 8;
    frame->format = AV_PIX_FMT_RGBA;
    frame->color_range = AVCOL_RANGE_JPEG;
    assert(av_frame_get_buffer(frame, 32) == 0);
    for (int y = 0; y < frame->height; y++) {
        uint8_t *row = frame->data[0] + y * frame->linesize[0];
        for (int x = 0; x < frame->width; x++) {
            row[4 * x] = x < 8 ? 255 : 0;
            row[4 * x + 1] = 0;
            row[4 * x + 2] = x < 8 ? 0 : 255;
            row[4 * x + 3] = 255;
        }
    }
    assert(upload_video(renderer, frame, 0));
    for (uint32_t rotation = 0; rotation < 4; rotation++) {
        const SDL_FRect destination = {32, 16, 64, 64};
        assert(draw_rotated(renderer, renderer->video, NULL, destination, rotation));
        expect_pixel(renderer->renderer, rotation % 2 ? 64 : 40, rotation % 2 ? 24 : 48,
                      rotation >= 2 ? 0 : 255, 0, rotation >= 2 ? 255 : 0);
    }
    // Only the selected field contributes to reconstructed rows.
    for (int y = 0; y < frame->height; y++)
        memset(frame->data[0] + y * frame->linesize[0], y % 2 ? 240 : 40, frame->width * 4);
    deinterlace_rgba(frame, 1);
    for (int y = 0; y < frame->height; y++)
        assert(frame->data[0][y * frame->linesize[0]] == 40);
    av_frame_free(&frame);
}

static void hdr_metadata(void)
{
    AVFrame *frame = av_frame_alloc();
    assert(frame && hdr_peak(frame) == 1000);
    AVMasteringDisplayMetadata *master = av_mastering_display_metadata_create_side_data(frame);
    assert(master);
    master->has_luminance = 1;
    master->max_luminance = (AVRational) {4000, 1};
    assert(hdr_peak(frame) == 4000);
    AVContentLightMetadata *light = av_content_light_metadata_create_side_data(frame);
    assert(light);
    light->MaxCLL = 1000;
    assert(hdr_peak(frame) == 1000);
    light->MaxCLL = 0;
    assert(hdr_peak(frame) == 4000);
    av_frame_free(&frame);
}

static float pq_code(float nits)
{
    const float linear = powf(nits / 10000.0f, 2610.0f / 16384);
    return powf((3424.0f / 4096 + (2413.0f / 128) * linear) /
                 (1 + (2392.0f / 128) * linear), 2523.0f / 32);
}

static void uniform_p010(AVFrame *frame, float code)
{
    // Model a new decoded picture even when the fixture reuses its allocation.
    frame->pts++;
    const int value = lroundf(frame->color_range == AVCOL_RANGE_JPEG ? code * 1023 : 64 + code * 876);
    for (int y = 0; y < frame->height; y++) {
        uint16_t *row = (uint16_t *) (frame->data[0] + y * frame->linesize[0]);
        for (int x = 0; x < frame->width; x++) row[x] = value << 6;
    }
    for (int y = 0; y < (frame->height + 1) / 2; y++) {
        uint16_t *row = (uint16_t *) (frame->data[1] + y * frame->linesize[1]);
        for (int x = 0; x < frame->width; x++) row[x] = 512 << 6;
    }
}

static void read_rgb(SDL_Renderer *renderer, int x, int y, float rgb[3])
{
    SDL_Surface *pixels = SDL_RenderReadPixels(renderer, NULL);
    assert(pixels);
    float a;
    assert(SDL_ReadSurfacePixelFloat(pixels, x, y, &rgb[0], &rgb[1], &rgb[2], &a));
    SDL_DestroySurface(pixels);
}

static float read_linear(SDL_Renderer *renderer)
{
    float rgb[3];
    read_rgb(renderer, 8, 8, rgb);
    const float r = rgb[0], g = rgb[1], b = rgb[2];
    assert(isfinite(r) && fabsf(r - g) < 0.02f && fabsf(g - b) < 0.02f);
    return r;
}

static void direct_geometry_and_color(UpVideoRenderer *renderer, AVFrame *frame)
{
    frame->color_range = AVCOL_RANGE_MPEG;
    uniform_p010(frame, pq_code(100));
    for (int y = 0; y < 16; y++) {
        uint16_t *row = (uint16_t *) (frame->data[0] + y * frame->linesize[0]);
        for (int x = 8; x < 16; x++) row[x] = lroundf(64 + pq_code(1000) * 876) << 6;
    }
    // Asymmetric luminance, non-neutral chroma, cropped/scaled quarter turns,
    // and selected fields must agree with the established conversion path.
    for (int test = 0; test < 10; test++) {
        const int field = test >= 8 ? test - 7 : 0;
        const uint32_t rotation = test % 4;
        const SDL_FRect source = {0, 0, test >= 4 && test < 8 ? 8 : 16, 16};
        const SDL_FRect destination = {0, 0, 16, 16};
        if (test == 8) {
            for (int y = 0; y < 8; y++) {
                uint16_t *row = (uint16_t *) (frame->data[1] + y * frame->linesize[1]);
                for (int x = 0; x < 16; x++) row[x] = (x % 2 ? 480 : 540) << 6;
            }
        }
        frame->pts++;
        renderer->render_width = renderer->render_height = 16;
        assert(upload_video(renderer, frame, field));
        assert(draw_rotated(renderer, renderer->video, &source, destination, rotation));
        float expected[3], actual[3];
        read_rgb(renderer->renderer, 4, 4, expected);
        AVFrame *hardware = hardware_picture(frame);
        assert(up_metal_color_draw(renderer->color, renderer->renderer, hardware, field, hdr_peak(hardware),
            source, destination, rotation, 16, 16, renderer->error, sizeof(renderer->error)));
        read_rgb(renderer->renderer, 4, 4, actual);
        up_metal_color_after_present(renderer->color);
        for (int component = 0; component < 3; component++)
            assert(fabsf(actual[component] - expected[component]) < fmaxf(0.02f, fabsf(expected[component]) * 0.02f));
        av_frame_free(&hardware);
    }
}

static void scaled_bob_fields(UpVideoRenderer *renderer)
{
    AVFrame *frame = av_frame_alloc();
    assert(frame);
    frame->width = frame->height = 16;
    frame->format = AV_PIX_FMT_P010;
    frame->colorspace = AVCOL_SPC_BT709;
    frame->color_primaries = AVCOL_PRI_BT709;
    frame->color_trc = AVCOL_TRC_BT709;
    frame->color_range = AVCOL_RANGE_MPEG;
    assert(av_frame_get_buffer(frame, 32) == 0);
    uniform_p010(frame, 0);
    // Opposing fields must remain black/white at fractional sampling positions,
    // including the edges. Comparing two paths alone misses their shared bug.
    for (int y = 1; y < 16; y += 2)
        for (int x = 0; x < 16; x++)
            ((uint16_t *) (frame->data[0] + y * frame->linesize[0]))[x] = 940 << 6;
    AVFrame *hardware = hardware_picture(frame);
    for (int size = 8; size <= 32; size *= 2) {
        const SDL_FRect destination = {0, 0, size, size};
        for (int field = 1; field <= 2; field++) {
            for (int direct = 0; direct <= 1; direct++) {
                if (direct) {
                    assert(up_metal_color_draw(renderer->color, renderer->renderer, hardware,
                        field, 1000, (SDL_FRect) {0, 0, 16, 16}, destination, 0, 16, 16,
                        renderer->error, sizeof(renderer->error)));
                } else {
                    renderer->render_width = renderer->render_height = size;
                    assert(upload_video(renderer, hardware, field));
                    assert(SDL_RenderTexture(renderer->renderer, renderer->video, NULL, &destination));
                }
                SDL_Surface *pixels = SDL_RenderReadPixels(renderer->renderer, NULL);
                assert(pixels);
                for (int y = 0; y < (size < 16 ? size : 16); y++) {
                    float r, g, b, a;
                    assert(SDL_ReadSurfacePixelFloat(pixels, 4, y, &r, &g, &b, &a));
                    assert(fabsf(r - (field - 1)) < 0.02f);
                    assert(fabsf(g - r) < 0.02f && fabsf(b - r) < 0.02f);
                }
                SDL_DestroySurface(pixels);
                if (direct) up_metal_color_after_present(renderer->color);
            }
        }
    }
    av_frame_free(&hardware);
    av_frame_free(&frame);
    puts("Metal bob: independent fields survive downscaling, native size and upscaling");
}

static int metal_hdr(void)
{
    assert(SDL_Init(SDL_INIT_VIDEO));
    SDL_Window *window = SDL_CreateWindow("HDR pixel validation", 128, 96,
        SDL_WINDOW_METAL | SDL_WINDOW_TRANSPARENT | SDL_WINDOW_HIDDEN);
    assert(window);
    SDL_Surface *shape = up_corner_shape(128, 96, 15);
    assert(shape && SDL_SetWindowShape(window, shape));
    SDL_DestroySurface(shape);
    UpVideoRenderer *renderer = up_video_renderer_create(window);
    assert(up_video_renderer_ready(renderer));
    SDL_PropertiesID properties = SDL_CreateProperties();
    SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, SDL_PIXELFORMAT_RGBA128_FLOAT);
    SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_TARGET);
    SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, 16);
    SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, 16);
    SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, SDL_COLORSPACE_SRGB_LINEAR);
    SDL_SetFloatProperty(properties, SDL_PROP_TEXTURE_CREATE_HDR_HEADROOM_FLOAT, 50.0f);
    SDL_Texture *target = SDL_CreateTextureWithProperties(renderer->renderer, properties);
    assert(target && SDL_SetRenderTarget(renderer->renderer, target));
    AVFrame *frame = av_frame_alloc();
    assert(frame);
    frame->width = frame->height = 16;
    frame->format = AV_PIX_FMT_P010;
    frame->colorspace = AVCOL_SPC_BT2020_NCL;
    frame->color_primaries = AVCOL_PRI_BT2020;
    frame->color_trc = AVCOL_TRC_SMPTE2084;
    assert(av_frame_get_buffer(frame, 32) == 0);
    for (int full = 0; full < 2; full++) {
        frame->color_range = full ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
        const float nits[] = {0, 100, 203, 1000};
        for (size_t i = 0; i < sizeof(nits) / sizeof(nits[0]); i++) {
            uniform_p010(frame, pq_code(nits[i]));
            assert(upload_video(renderer, frame, 0));
            assert(SDL_RenderTexture(renderer->renderer, renderer->video, NULL, NULL));
            const float actual = read_linear(renderer->renderer), expected = nits[i] / 203;
            fprintf(stderr, "PQ %s %.0f nits -> %.4f linear (expected %.4f)\n",
                    full ? "full" : "limited", nits[i], actual, expected);
            assert(fabsf(actual - expected) < fmaxf(0.02f, 0.02f * expected));
            AVFrame *hardware = hardware_picture(frame);
            assert(up_metal_color_can_draw(hardware));
            assert(up_metal_color_draw(renderer->color, renderer->renderer, hardware, 0, hdr_peak(hardware),
                (SDL_FRect) {0, 0, 16, 16}, (SDL_FRect) {0, 0, 16, 16}, 0, 16, 16,
                renderer->error, sizeof(renderer->error)));
            const float direct = read_linear(renderer->renderer);
            up_metal_color_after_present(renderer->color);
            fprintf(stderr, "Direct PQ %s %.0f nits -> %.4f linear\n", full ? "full" : "limited", nits[i], direct);
            assert(fabsf(direct - expected) < fmaxf(0.02f, 0.02f * expected));
            av_frame_free(&hardware);
        }
    }
    direct_geometry_and_color(renderer, frame);
    scaled_bob_fields(renderer);
    renderer->render_width = renderer->render_height = 16;
    frame->color_range = AVCOL_RANGE_MPEG;
    frame->color_trc = AVCOL_TRC_ARIB_STD_B67;
    uniform_p010(frame, 0.75f);
    assert(upload_video(renderer, frame, 0));
    assert(SDL_RenderTexture(renderer->renderer, renderer->video, NULL, NULL));
    const float hlg = read_linear(renderer->renderer);
    fprintf(stderr, "HLG reference white -> %.4f linear (expected 1.0)\n", hlg);
    assert(fabsf(hlg - 1) < 0.02f);
    AVFrame *hardware = hardware_picture(frame);
    assert(up_metal_color_draw(renderer->color, renderer->renderer, hardware, 0, hdr_peak(hardware),
        (SDL_FRect) {0, 0, 16, 16}, (SDL_FRect) {0, 0, 16, 16}, 0, 16, 16,
        renderer->error, sizeof(renderer->error)));
    assert(fabsf(read_linear(renderer->renderer) - 1) < 0.02f);
    up_metal_color_after_present(renderer->color);
    av_frame_free(&hardware);
    renderer->render_width = renderer->render_height = 8;
    assert(upload_video(renderer, frame, 0));
    assert(renderer->video->w == 8 && renderer->video->h == 8);
    SDL_Texture *cached = renderer->video;
    assert(upload_video(renderer, frame, 0) && renderer->video == cached);
    assert(SDL_RenderTexture(renderer->renderer, renderer->video, NULL, NULL));
    assert(fabsf(read_linear(renderer->renderer) - 1) < 0.02f);
    // A one-headroom target models an SDR display and must compress HDR peaks
    // without losing the ordering of diffuse white and specular highlights.
    SDL_SetFloatProperty(properties, SDL_PROP_TEXTURE_CREATE_HDR_HEADROOM_FLOAT, 1.0f);
    SDL_Texture *sdr = SDL_CreateTextureWithProperties(renderer->renderer, properties);
    assert(sdr && SDL_SetRenderTarget(renderer->renderer, sdr));
    frame->color_trc = AVCOL_TRC_SMPTE2084;
    float previous = -1;
    for (int i = 0; i < 4; i++) {
        const float nits[] = {0, 100, 203, 1000};
        uniform_p010(frame, pq_code(nits[i]));
        assert(upload_video(renderer, frame, 0));
        assert(SDL_RenderTexture(renderer->renderer, renderer->video, NULL, NULL));
        const float actual = read_linear(renderer->renderer);
        fprintf(stderr, "SDR mapping %.0f nits -> %.4f linear\n", nits[i], actual);
        assert(actual > previous && actual <= 1.01f);
        previous = actual;
        hardware = hardware_picture(frame);
        assert(up_metal_color_draw(renderer->color, renderer->renderer, hardware, 0, hdr_peak(hardware),
            (SDL_FRect) {0, 0, 16, 16}, (SDL_FRect) {0, 0, 16, 16}, 0, 16, 16,
            renderer->error, sizeof(renderer->error)));
        const float direct = read_linear(renderer->renderer);
        assert(fabsf(direct - actual) < 0.02f);
        up_metal_color_after_present(renderer->color);
        av_frame_free(&hardware);
    }
    assert(SDL_SetRenderTarget(renderer->renderer, NULL));
    // Metadata/format changes must replace a float texture before an RGBA8
    // software upload, including when the old and new dimensions match.
    frame->color_trc = AVCOL_TRC_BT709;
    assert(upload_video(renderer, frame, 0));
    assert(renderer->video->format == SDL_PIXELFORMAT_RGBA32);
    assert(SDL_RenderPresent(renderer->renderer));
    SDL_DestroyTexture(target);
    SDL_DestroyTexture(sdr);
    SDL_DestroyProperties(properties);
    av_frame_free(&frame);
    up_video_renderer_destroy(renderer);
    SDL_DestroyWindow(window);
    SDL_Quit();
    puts("Metal HDR: 10-bit PQ limited/full, HLG white and SDR tone mapping passed");
    return 0;
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "--metal") == 0) return metal_hdr();
    mapped_decoder_luma();
    rounded_window_alpha();
    hdr_metadata();
    aspect_crop_and_rotation();
    SDL_Surface *surface = SDL_CreateSurface(128, 96, SDL_PIXELFORMAT_RGBA32);
    assert(surface);
    UpVideoRenderer *renderer = av_mallocz(sizeof(*renderer));
    assert(renderer);
    renderer->renderer = SDL_CreateSoftwareRenderer(surface);
    assert(renderer->renderer);
    assert(SDL_SetRenderDrawBlendMode(renderer->renderer, SDL_BLENDMODE_BLEND));
    assert(up_video_renderer_ready(renderer));
    assert(!up_video_renderer_device(renderer));
    conversion_and_overlays(renderer);
    rotated_pixels_and_bob(renderer);
    up_video_renderer_destroy(renderer);
    SDL_DestroySurface(surface);
    puts("macOS renderer: conversion, alpha, crop, rotation, subtitles and bob passed");
    return 0;
}
