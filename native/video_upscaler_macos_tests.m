#include "video_upscaler_macos.m"
#include "macos_logging.h"
#include <assert.h>

static AVFrame *dvd_picture(void)
{
    AVFrame *frame = av_frame_alloc();
    assert(frame);
    frame->width = 720; frame->height = 576;
    frame->format = AV_PIX_FMT_YUV420P;
    frame->color_range = AVCOL_RANGE_MPEG;
    frame->sample_aspect_ratio = (AVRational) {64, 45};
    frame->time_base = (AVRational) {1, 25}; frame->pts = 10;
    assert(av_frame_get_buffer(frame, 32) == 0);
    for (int y = 0; y < 576; y++)
        memset(frame->data[0] + y * frame->linesize[0], y < 100 ? 16 : y < 400 ? 128 : 235, 720);
    for (int plane = 1; plane < 3; plane++)
        for (int y = 0; y < 288; y++) memset(frame->data[plane] + y * frame->linesize[plane], 128, 360);
    return frame;
}

static void bypass_policy(void)
{
    UpVideoUpscaler *upscaler = up_video_upscaler_create(0);
    AVFrame *frame = dvd_picture();
    frame->width = 3840; frame->height = 2160;
    assert(up_video_upscaler_frame(upscaler, frame, 0, true, true) == frame);
    assert(!upscaler->session && !upscaler->base);
    frame->width = 720; frame->height = 576;
    frame->color_trc = AVCOL_TRC_SMPTE2084;
    assert(up_video_upscaler_frame(upscaler, frame, 0, true, true) == frame);
    assert(!upscaler->session);
    frame->color_trc = AVCOL_TRC_UNSPECIFIED;
    assert(up_video_upscaler_frame(upscaler, frame, 1, true, true) == frame);
    assert(!upscaler->session);
    const AVFrame *base = up_video_upscaler_frame(upscaler, frame, 0, false, true);
    assert(base != frame && base->width == 720 && base->height == 576);
    assert(base->colorspace == AVCOL_SPC_SMPTE170M);
    assert(av_q2d(base->sample_aspect_ratio) == av_q2d(frame->sample_aspect_ratio));
    assert(!upscaler->session); // U off does not load a model.
    base = up_video_upscaler_frame(upscaler, frame, 0, true, false);
    assert(base->width == 720 && !upscaler->session); // no enhancement when shrinking
    up_video_upscaler_destroy(upscaler);
    av_frame_free(&frame);
}

static void apple_ml_frames(void)
{
    if (@available(macOS 26.0, *)) {
        if (!VTLowLatencySuperResolutionScalerConfiguration.isSupported ||
            ![[VTLowLatencySuperResolutionScalerConfiguration supportedScaleFactorsForFrameWidth:720 frameHeight:576] containsObject:@2]) {
            puts("Apple ML: skipped, 720x576 2x unsupported on this machine");
            return;
        }
        UpVideoUpscaler *upscaler = up_video_upscaler_create(0);
        AVFrame *frame = dvd_picture();
        const double deadline = NSProcessInfo.processInfo.systemUptime + 15;
        const AVFrame *scaled = NULL;
        do {
            scaled = up_video_upscaler_frame(upscaler, frame, 0, true, true);
            if (scaled->width == 1440) break;
            SDL_Delay(10);
        } while (NSProcessInfo.processInfo.systemUptime < deadline);
        assert(scaled && scaled->width == 1440 && scaled->height == 1152);
        assert(frame->width == 720 && frame->height == 576 && frame->colorspace == AVCOL_SPC_UNSPECIFIED);
        assert(scaled->pts == frame->pts && scaled->sample_aspect_ratio.num == 64 && scaled->sample_aspect_ratio.den == 45);
        assert(scaled->colorspace == AVCOL_SPC_SMPTE170M);
        assert(up_video_upscaler_frame(upscaler, frame, 0, true, true) == scaled); // paused-frame cache
        AVFrame *held = av_frame_clone(scaled);
        assert(held);
        const double started = NSProcessInfo.processInfo.systemUptime;
        for (int i = 0; i < 50; i++) {
            frame->pts++;
            scaled = up_video_upscaler_frame(upscaler, frame, 0, true, true);
            assert(scaled->width == 1440 && scaled->pts == frame->pts);
        }
        printf("Apple ML: %.2f ms/frame over 50 frames\n", (NSProcessInfo.processInfo.systemUptime - started) * 1000 / 50);
        const AVFrame *base = up_video_upscaler_frame(upscaler, frame, 0, false, true);
        assert(base->width == 720 && strcmp(up_video_upscaler_status(upscaler), "OFF") == 0);
        assert(up_video_upscaler_frame(upscaler, frame, 0, true, true)->width == 1440);
        up_video_upscaler_destroy(upscaler);
        // An in-flight renderer reference must outlive the processor/pools.
        CVPixelBufferRef buffer = (CVPixelBufferRef) held->data[3];
        assert(CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
        const uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
        const size_t stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
        assert(y[50 * stride + 100] < y[400 * stride + 100]);
        assert(y[400 * stride + 100] < y[1000 * stride + 100]);
        CVPixelBufferUnlockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly);
        av_frame_free(&held); av_frame_free(&frame);
        puts("Apple ML: live 2x processing, DVD aspect/color metadata, cache, toggle and buffer lifetime passed");
    }
}

static void apple_ml_decoder_padding(void)
{
    if (@available(macOS 26.0, *)) {
        if (!VTLowLatencySuperResolutionScalerConfiguration.isSupported ||
            ![[VTLowLatencySuperResolutionScalerConfiguration supportedScaleFactorsForFrameWidth:696 frameHeight:480] containsObject:@2])
            return;
        UpVideoUpscaler *upscaler = up_video_upscaler_create(0);
        AVFrame *picture = dvd_picture();
        picture->width = 696; picture->height = 480;
        picture->sample_aspect_ratio = (AVRational) {103, 87};
        picture->colorspace = AVCOL_SPC_SMPTE170M;
        picture->color_primaries = AVCOL_PRI_BT470BG;
        picture->color_trc = AVCOL_TRC_BT709;
        // Reproduce the H.264 decoder: stride 704, but no declared padding.
        AVFrame *hardware = prepare_frame(upscaler, nil, 0, picture);
        assert(hardware);
        CVPixelBufferRef decoded = (CVPixelBufferRef) hardware->data[3];
        size_t right = 99;
        CVPixelBufferGetExtendedPixels(decoded, NULL, &right, NULL, NULL);
        assert(right == 0);
        CVBufferSetAttachment(decoded, kCVImageBufferYCbCrMatrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4, kCVAttachmentMode_ShouldPropagate);
        const double deadline = NSProcessInfo.processInfo.systemUptime + 15;
        const AVFrame *scaled = NULL;
        do {
            scaled = up_video_upscaler_frame(upscaler, hardware, 0, true, true);
            if (scaled->width == 1392) break;
            SDL_Delay(10);
        } while (NSProcessInfo.processInfo.systemUptime < deadline);
        assert(scaled && scaled->width == 1392 && scaled->height == 960);
        assert(scaled->sample_aspect_ratio.num == 103 && scaled->sample_aspect_ratio.den == 87);
        assert(scaled->color_primaries == AVCOL_PRI_BT470BG && scaled->color_trc == AVCOL_TRC_BT709);
        assert(upscaler->base->data[3] != hardware->data[3]);
        CVPixelBufferRef normalized = (CVPixelBufferRef) upscaler->base->data[3];
        CVPixelBufferGetExtendedPixels(normalized, NULL, &right, NULL, NULL);
        assert(right >= 8);
        assert(CVPixelBufferLockBaseAddress(normalized, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
        const uint8_t *row = (const uint8_t *) CVPixelBufferGetBaseAddressOfPlane(normalized, 0) +
            200 * CVPixelBufferGetBytesPerRowOfPlane(normalized, 0);
        for (int x = 696; x < 704; x++) assert(row[x] == row[695]);
        CVPixelBufferUnlockBaseAddress(normalized, kCVPixelBufferLock_ReadOnly);
        CVPixelBufferGetExtendedPixels(decoded, NULL, &right, NULL, NULL);
        assert(right == 0); // never mutate decoder-owned surfaces
        up_video_upscaler_destroy(upscaler);
        av_frame_free(&hardware); av_frame_free(&picture);
        puts("Apple ML: 696x480 decoder-padding regression, replicated edges and SAR/color metadata passed");
    }
}

int main(int argc, char **argv)
{
    up_macos_configure_logging(argc > 2 && strcmp(argv[2], "--debug") == 0);
    @autoreleasepool {
        if (argc > 1 && strcmp(argv[1], "--log-probe") == 0) {
            NSLog(@"upscale framework diagnostic probe");
            fputs("upscale player error probe\n", stderr);
            return 0;
        }
        bypass_policy();
        if (argc > 1 && strcmp(argv[1], "--apple-ml") == 0) {
            apple_ml_frames();
            apple_ml_decoder_padding();
        }
    }
    puts("Upscale policy: 4K/HDR/interlace and shrinking bypass, no model on U off passed");
    return 0;
}
