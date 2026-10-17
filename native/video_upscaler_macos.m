#include "video_upscaler_macos.h"
#import <VideoToolbox/VideoToolbox.h>
#include <libavutil/hwcontext_videotoolbox.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

@interface UpUpscaleSession : NSObject
@property(nonatomic, strong) VTFrameProcessor *processor;
@property(nonatomic, strong) NSDictionary *sourceRequirements;
@property(nonatomic) CVPixelBufferPoolRef sourcePool, destinationPool;
@property(nonatomic) int width, height, factor, readiness;
@property(nonatomic) SDL_WindowID window;
@property(nonatomic) bool active;
@property(nonatomic) bool reportedCopy;
@end
@implementation UpUpscaleSession
- (void)dealloc
{
    [_processor endSession];
    if (_sourcePool) CVPixelBufferPoolRelease(_sourcePool);
    if (_destinationPool) CVPixelBufferPoolRelease(_destinationPool);
}
@end

struct UpVideoUpscaler {
    CFTypeRef session;
    SDL_WindowID window;
    struct SwsContext *conversion;
    AVFrame *source, *base, *scaled;
    bool cached_enabled, cached_larger;
    int cached_readiness;
    char status[96];
};

static void clear_cached_frames(UpVideoUpscaler *upscaler)
{
    av_frame_free(&upscaler->source);
    av_frame_free(&upscaler->base);
    av_frame_free(&upscaler->scaled);
}

static bool eligible(const AVFrame *frame, int field)
{
    if (!frame || field || (frame->flags & AV_FRAME_FLAG_INTERLACED) ||
        frame->width < 16 || frame->height < 16 ||
        frame->width > 1920 || frame->height > 1080 ||
        frame->color_trc == AVCOL_TRC_SMPTE2084 || frame->color_trc == AVCOL_TRC_ARIB_STD_B67)
        return false;
    if (frame->format == AV_PIX_FMT_VIDEOTOOLBOX)
        return frame->data[3] && CVPixelBufferGetPixelFormatType((CVPixelBufferRef) frame->data[3]) ==
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    const AVPixFmtDescriptor *description = av_pix_fmt_desc_get(frame->format);
    return description && description->nb_components && description->comp[0].depth == 8 &&
        !(description->flags & (AV_PIX_FMT_FLAG_HWACCEL | AV_PIX_FMT_FLAG_RGB | AV_PIX_FMT_FLAG_PAL));
}

static CVPixelBufferPoolRef make_pool(NSDictionary *requirements, int width, int height)
{
    NSMutableDictionary *attributes = [requirements mutableCopy];
    attributes[(__bridge id)kCVPixelBufferWidthKey] = @(width);
    attributes[(__bridge id)kCVPixelBufferHeightKey] = @(height);
    attributes[(__bridge id)kCVPixelBufferPixelFormatTypeKey] = @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
    attributes[(__bridge id)kCVPixelBufferIOSurfacePropertiesKey] = @{};
    attributes[(__bridge id)kCVPixelBufferMetalCompatibilityKey] = @YES;
    CVPixelBufferPoolRef pool = NULL;
    CVPixelBufferPoolCreate(NULL, NULL, (__bridge CFDictionaryRef) attributes, &pool);
    return pool;
}

static void retire_session(UpVideoUpscaler *upscaler)
{
    if (!upscaler->session) return;
    UpUpscaleSession *session = (__bridge UpUpscaleSession *) upscaler->session;
    @synchronized(session) { session.active = false; }
    CFRelease(upscaler->session);
    upscaler->session = NULL;
}

static int session_readiness(UpUpscaleSession *session)
{
    if (!session) return 0;
    @synchronized(session) { return session.readiness; }
}

static UpUpscaleSession *session_for(UpVideoUpscaler *upscaler, int width, int height)
{
    UpUpscaleSession *session = (__bridge UpUpscaleSession *) upscaler->session;
    if (session && session.width == width && session.height == height) return session;
    retire_session(upscaler);
    session = [UpUpscaleSession new];
    session.width = width; session.height = height;
    session.window = upscaler->window;
    session.active = true;
    upscaler->session = CFBridgingRetain(session);
    // Loading ML weights can take longer than a frame. Keep rendering and
    // decoding normal frames until the background session is ready.
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        @autoreleasepool {
            if (@available(macOS 26.0, *)) {
                NSArray<NSNumber *> *factors = VTLowLatencySuperResolutionScalerConfiguration.isSupported ?
                    [VTLowLatencySuperResolutionScalerConfiguration supportedScaleFactorsForFrameWidth:width frameHeight:height] : @[];
                int factor = [factors containsObject:@2] ? 2 : 0;
                NSError *error = nil;
                if (factor) {
                    VTLowLatencySuperResolutionScalerConfiguration *configuration =
                        [[VTLowLatencySuperResolutionScalerConfiguration alloc] initWithFrameWidth:width frameHeight:height scaleFactor:factor];
                    VTFrameProcessor *processor = [VTFrameProcessor new];
                    if ([processor startSessionWithConfiguration:configuration error:&error]) {
                        session.processor = processor;
                        session.sourceRequirements = configuration.sourcePixelBufferAttributes;
                        session.sourcePool = make_pool(configuration.sourcePixelBufferAttributes, width, height);
                        session.destinationPool = make_pool(configuration.destinationPixelBufferAttributes, width * factor, height * factor);
                    }
                }
                @synchronized(session) {
                    session.factor = factor;
                    session.readiness = session.processor && session.sourcePool && session.destinationPool ? 1 : -1;
                    if (session.readiness == 1)
                        fprintf(stderr, "upscale: Apple ML ready, %dx%d -> %dx%d\n", width, height, width * factor, height * factor);
                    else if (error)
                        fprintf(stderr, "upscale: unavailable: %s\n", error.localizedDescription.UTF8String);
                    if (session.active && session.window) {
                        SDL_Event event = {0};
                        event.type = SDL_EVENT_WINDOW_EXPOSED;
                        event.window.windowID = session.window;
                        SDL_PushEvent(&event);
                    }
                }
            } else {
                @synchronized(session) { session.readiness = -1; }
            }
        }
    });
    return session;
}

static void release_buffer(void *opaque, uint8_t *data)
{
    (void) data;
    CVPixelBufferRelease((CVPixelBufferRef) opaque);
}

static AVFrame *wrap_buffer(const AVFrame *source, CVPixelBufferRef buffer, int factor)
{
    AVFrame *frame = av_frame_alloc();
    if (!frame || av_frame_copy_props(frame, source) < 0) { av_frame_free(&frame); return NULL; }
    frame->format = AV_PIX_FMT_VIDEOTOOLBOX;
    frame->width = source->width * factor; frame->height = source->height * factor;
    frame->color_range = AVCOL_RANGE_MPEG;
    // Infer the YUV matrix before increasing dimensions: PAL/NTSC DVD must
    // not accidentally become BT.709 just because its scaled frame is HD.
    if (frame->colorspace == AVCOL_SPC_UNSPECIFIED)
        frame->colorspace = source->width < 1280 && source->height <= 576 ? AVCOL_SPC_SMPTE170M : AVCOL_SPC_BT709;
    frame->data[3] = (uint8_t *) buffer;
    CVPixelBufferRetain(buffer);
    frame->buf[0] = av_buffer_create((uint8_t *) buffer, 0, release_buffer, buffer, 0);
    if (!frame->buf[0]) { CVPixelBufferRelease(buffer); av_frame_free(&frame); }
    return frame;
}

static AVFrame *prepare_frame(UpVideoUpscaler *upscaler, UpUpscaleSession *session, int ready, const AVFrame *source)
{
    CVPixelBufferRef decoded = source->format == AV_PIX_FMT_VIDEOTOOLBOX ? (CVPixelBufferRef) source->data[3] : NULL;
    if (decoded && source->color_range != AVCOL_RANGE_JPEG) {
        if (ready != 1) return wrap_buffer(source, decoded, 1);
        size_t left = 0, right = 0, top = 0, bottom = 0;
        CVPixelBufferGetExtendedPixels(decoded, &left, &right, &top, &bottom);
        NSDictionary *required = session.sourceRequirements;
        const bool compatible = CVPixelBufferGetWidth(decoded) == (size_t) source->width &&
            CVPixelBufferGetHeight(decoded) == (size_t) source->height &&
            left >= [required[(__bridge id)kCVPixelBufferExtendedPixelsLeftKey] unsignedIntegerValue] &&
            right >= [required[(__bridge id)kCVPixelBufferExtendedPixelsRightKey] unsignedIntegerValue] &&
            top >= [required[(__bridge id)kCVPixelBufferExtendedPixelsTopKey] unsignedIntegerValue] &&
            bottom >= [required[(__bridge id)kCVPixelBufferExtendedPixelsBottomKey] unsignedIntegerValue];
        if (compatible) return wrap_buffer(source, decoded, 1);
        // Row stride alone doesn't declare valid extended pixels. For example,
        // a 696-wide decoder surface can have stride 704 and zero declared
        // padding, while Apple ML requires eight replicated pixels on the right.
        // Normalize only these small, incompatible surfaces into the ML pool.
        if (CVPixelBufferGetWidthOfPlane(decoded, 0) < (size_t) source->width ||
            CVPixelBufferGetHeightOfPlane(decoded, 0) < (size_t) source->height)
            return NULL;
        CVPixelBufferRef buffer = NULL;
        if (CVPixelBufferPoolCreatePixelBuffer(NULL, session.sourcePool, &buffer) != kCVReturnSuccess)
            return NULL;
        if (CVPixelBufferLockBaseAddress(decoded, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
            CVPixelBufferRelease(buffer); return NULL;
        }
        if (CVPixelBufferLockBaseAddress(buffer, 0) != kCVReturnSuccess) {
            CVPixelBufferUnlockBaseAddress(decoded, kCVPixelBufferLock_ReadOnly);
            CVPixelBufferRelease(buffer); return NULL;
        }
        for (int plane = 0; plane < 2; plane++) {
            const size_t rows = plane ? ((size_t) source->height + 1) / 2 : (size_t) source->height;
            const size_t bytes = plane ? ((size_t) source->width + 1) / 2 * 2 : (size_t) source->width;
            for (size_t y = 0; y < rows; y++)
                memcpy((uint8_t *) CVPixelBufferGetBaseAddressOfPlane(buffer, plane) +
                        y * CVPixelBufferGetBytesPerRowOfPlane(buffer, plane),
                    (const uint8_t *) CVPixelBufferGetBaseAddressOfPlane(decoded, plane) +
                        y * CVPixelBufferGetBytesPerRowOfPlane(decoded, plane), bytes);
        }
        CVBufferPropagateAttachments(decoded, buffer);
        const CVReturn filled = CVPixelBufferFillExtendedPixels(buffer);
        CVPixelBufferUnlockBaseAddress(buffer, 0);
        CVPixelBufferUnlockBaseAddress(decoded, kCVPixelBufferLock_ReadOnly);
        AVFrame *frame = filled == kCVReturnSuccess ? wrap_buffer(source, buffer, 1) : NULL;
        CVPixelBufferRelease(buffer);
        if (frame && !session.reportedCopy) {
            fprintf(stderr, "upscale: normalized decoder surface to Apple's required padding\n");
            session.reportedCopy = true;
        }
        return frame;
    }
    // Full-range hardware surfaces retain their existing rendering path.
    if (source->format == AV_PIX_FMT_VIDEOTOOLBOX) return NULL;
    CVPixelBufferRef buffer = NULL;
    if (ready == 1) CVPixelBufferPoolCreatePixelBuffer(NULL, session.sourcePool, &buffer);
    else {
        CVPixelBufferPoolRef pool = make_pool(@{}, source->width, source->height);
        if (pool) { CVPixelBufferPoolCreatePixelBuffer(NULL, pool, &buffer); CVPixelBufferPoolRelease(pool); }
    }
    if (!buffer) return NULL;
    upscaler->conversion = sws_getCachedContext(upscaler->conversion,
        source->width, source->height, source->format, source->width, source->height,
        AV_PIX_FMT_NV12, SWS_BILINEAR, NULL, NULL, NULL);
    if (!upscaler->conversion || CVPixelBufferLockBaseAddress(buffer, 0) != kCVReturnSuccess) {
        CVPixelBufferRelease(buffer); return NULL;
    }
    const int space = source->colorspace == AVCOL_SPC_BT2020_NCL ? SWS_CS_BT2020 :
        source->colorspace == AVCOL_SPC_SMPTE170M || source->colorspace == AVCOL_SPC_BT470BG ||
        (source->colorspace == AVCOL_SPC_UNSPECIFIED && source->width < 1280 && source->height <= 576) ? SWS_CS_ITU601 : SWS_CS_ITU709;
    const int *coefficients = sws_getCoefficients(space);
    int result = sws_setColorspaceDetails(upscaler->conversion, coefficients,
        source->color_range == AVCOL_RANGE_JPEG, coefficients, 0, 0, 1 << 16, 1 << 16);
    uint8_t *planes[4] = {CVPixelBufferGetBaseAddressOfPlane(buffer, 0), CVPixelBufferGetBaseAddressOfPlane(buffer, 1)};
    int strides[4] = {(int) CVPixelBufferGetBytesPerRowOfPlane(buffer, 0), (int) CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)};
    if (result >= 0) result = sws_scale(upscaler->conversion, (const uint8_t *const *) source->data,
        source->linesize, 0, source->height, planes, strides);
    if (result == source->height && CVPixelBufferFillExtendedPixels(buffer) != kCVReturnSuccess)
        result = -1;
    CVPixelBufferUnlockBaseAddress(buffer, 0);
    AVFrame *frame = result == source->height ? wrap_buffer(source, buffer, 1) : NULL;
    CVPixelBufferRelease(buffer);
    return frame;
}

static AVFrame *scale_frame(UpUpscaleSession *session, const AVFrame *frame, CVPixelBufferRef input)
{
    if (@available(macOS 26.0, *)) {
        CVPixelBufferRef output = NULL;
        if (CVPixelBufferPoolCreatePixelBuffer(NULL, session.destinationPool, &output) != kCVReturnSuccess)
            return NULL;
        CVBufferPropagateAttachments(input, output);
        const double seconds = frame->pts != AV_NOPTS_VALUE && frame->time_base.num > 0 && frame->time_base.den > 0 ?
            frame->pts * av_q2d(frame->time_base) : 0;
        const CMTime time = CMTimeMakeWithSeconds(seconds, 1000000);
        VTFrameProcessorFrame *source = [[VTFrameProcessorFrame alloc] initWithBuffer:input presentationTimeStamp:time];
        VTFrameProcessorFrame *destination = [[VTFrameProcessorFrame alloc] initWithBuffer:output presentationTimeStamp:time];
        VTLowLatencySuperResolutionScalerParameters *parameters = [[VTLowLatencySuperResolutionScalerParameters alloc]
            initWithSourceFrame:source destinationFrame:destination];
        NSError *error = nil;
        AVFrame *scaled = NULL;
        if ([session.processor processWithParameters:parameters error:&error]) {
            scaled = wrap_buffer(frame, output, session.factor);
        } else {
            fprintf(stderr, "upscale: processing failed, using original frames: %s\n", error.localizedDescription.UTF8String);
            @synchronized(session) { session.readiness = -1; }
        }
        CVPixelBufferRelease(output);
        return scaled;
    }
    return NULL;
}

UpVideoUpscaler *up_video_upscaler_create(SDL_WindowID window)
{
    UpVideoUpscaler *upscaler = calloc(1, sizeof(*upscaler));
    if (upscaler) { upscaler->window = window; snprintf(upscaler->status, sizeof(upscaler->status), "AUTO"); }
    return upscaler;
}

const AVFrame *up_video_upscaler_frame(UpVideoUpscaler *upscaler, const AVFrame *frame,
                                     int field, bool enabled, bool larger)
{
    if (!upscaler) return frame;
    @autoreleasepool {
        if (!eligible(frame, field)) {
            snprintf(upscaler->status, sizeof(upscaler->status), "%s", !enabled ? "OFF" :
                frame->width > 1920 || frame->height > 1080 ? "BYPASS (HIGH RES)" : "BYPASS (FORMAT/INTERLACE)");
            clear_cached_frames(upscaler);
            return frame;
        }
        UpUpscaleSession *session = (__bridge UpUpscaleSession *) upscaler->session;
        if (session && (session.width != frame->width || session.height != frame->height)) {
            retire_session(upscaler);
            session = nil;
        }
        if (enabled && larger) session = session_for(upscaler, frame->width, frame->height);
        int ready = session_readiness(session);
        const AVFrame *previous = upscaler->source;
        const bool same = previous && previous->pts == frame->pts && previous->format == frame->format &&
            previous->width == frame->width && previous->height == frame->height &&
            previous->data[0] == frame->data[0] && previous->data[3] == frame->data[3] &&
            previous->color_range == frame->color_range && previous->colorspace == frame->colorspace &&
            previous->color_trc == frame->color_trc && previous->color_primaries == frame->color_primaries;
        if (!same || enabled != upscaler->cached_enabled || larger != upscaler->cached_larger || ready != upscaler->cached_readiness) {
            clear_cached_frames(upscaler);
            upscaler->source = av_frame_clone(frame);
            upscaler->base = prepare_frame(upscaler, session, ready, frame);
            upscaler->cached_enabled = enabled; upscaler->cached_larger = larger; upscaler->cached_readiness = ready;
            if (enabled && larger && ready == 1 && upscaler->base) {
                upscaler->scaled = scale_frame(session, frame, (CVPixelBufferRef) upscaler->base->data[3]);
                ready = session_readiness(session);
            }
        }
        if (!enabled) snprintf(upscaler->status, sizeof(upscaler->status), "OFF");
        else if (!larger) snprintf(upscaler->status, sizeof(upscaler->status), "BYPASS (WINDOW SIZE)");
        else if (upscaler->scaled) snprintf(upscaler->status, sizeof(upscaler->status), "APPLE ML 2X (%dX%d)", upscaler->scaled->width, upscaler->scaled->height);
        else snprintf(upscaler->status, sizeof(upscaler->status), "%s", ready < 0 ? "UNAVAILABLE" : "WARMING UP");
        return upscaler->scaled ? upscaler->scaled : upscaler->base ? upscaler->base : frame;
    }
}

const char *up_video_upscaler_status(const UpVideoUpscaler *upscaler)
{
    return upscaler ? upscaler->status : "AUTO";
}

void up_video_upscaler_destroy(UpVideoUpscaler *upscaler)
{
    if (!upscaler) return;
    retire_session(upscaler);
    clear_cached_frames(upscaler);
    sws_freeContext(upscaler->conversion);
    free(upscaler);
}
