#include "video_color_macos.h"
#include "video_color_macos_shader.h"

#import <CoreVideo/CoreVideo.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_videotoolbox.h>
#include <libswscale/swscale.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>

@interface UpMetalColorState : NSObject
@property(nonatomic, strong) id<MTLDevice> device;
@property(nonatomic, strong) id<MTLCommandQueue> queue;
@property(nonatomic, strong) id<MTLComputePipelineState> pipeline;
@property(nonatomic, strong) id<MTLCommandBuffer> retirement;
@property(nonatomic, strong) id<MTLLibrary> library;
@property(nonatomic, strong) NSMutableDictionary<NSNumber *, id<MTLRenderPipelineState>> *drawPipelines;
@end
@implementation UpMetalColorState
@end

struct UpMetalColor {
    CFTypeRef state;
    CVMetalTextureCacheRef textures;
    struct SwsContext *conversion;
};

typedef struct ColorParams {
    uint32_t depth, full_range, transfer, primaries;
    float kr, kb, peak_nits, white_nits;
    uint32_t field, padding0, padding1, padding2;
} ColorParams;

static void color_error(char *error, size_t size, const char *message)
{
    snprintf(error, size, "Metal color conversion: %s", message);
}

UpMetalColor *up_metal_color_create(SDL_Renderer *renderer, char *error, size_t size)
{
    @autoreleasepool {
        CAMetalLayer *layer = (__bridge CAMetalLayer *) SDL_GetRenderMetalLayer(renderer);
        if (!layer.device) { color_error(error, size, "no Metal device"); return NULL; }
        UpMetalColorState *state = [UpMetalColorState new];
        state.device = layer.device;
        state.queue = [state.device newCommandQueue];
        NSError *failure = nil;
        id<MTLLibrary> library = [state.device newLibraryWithSource:@(video_color_shader)
                                                           options:nil error:&failure];
        if (!library) { color_error(error, size, failure.localizedDescription.UTF8String); return NULL; }
        state.library = library;
        state.drawPipelines = [NSMutableDictionary new];
        state.pipeline = [state.device newComputePipelineStateWithFunction:
            [library newFunctionWithName:@"video_linear"] error:&failure];
        if (!state.queue || !state.pipeline) {
            color_error(error, size, "could not create the compute pipeline"); return NULL;
        }
        UpMetalColor *color = calloc(1, sizeof(*color));
        if (!color) { color_error(error, size, "out of memory"); return NULL; }
        color->state = CFBridgingRetain(state);
        if (CVMetalTextureCacheCreate(NULL, NULL, state.device, NULL, &color->textures) != kCVReturnSuccess) {
            up_metal_color_destroy(color);
            color_error(error, size, "could not create a CoreVideo texture cache"); return NULL;
        }
        return color;
    }
}

static CVPixelBufferRef pixel_buffer(int width, int height, OSType format)
{
    CVPixelBufferRef buffer = NULL;
    NSDictionary *attributes = @{(__bridge id)kCVPixelBufferIOSurfacePropertiesKey: @{},
                                 (__bridge id)kCVPixelBufferMetalCompatibilityKey: @YES};
    if (CVPixelBufferCreate(NULL, width, height, format,
            (__bridge CFDictionaryRef) attributes, &buffer) != kCVReturnSuccess)
        return NULL;
    return buffer;
}

// Software codecs still preserve ten-bit data and use the same HDR GPU path.
static CVPixelBufferRef upload_software(UpMetalColor *color, const AVFrame *source)
{
    AVFrame *downloaded = NULL;
    const AVFrame *frame = source;
    if (source->format == AV_PIX_FMT_VIDEOTOOLBOX) {
        downloaded = av_frame_alloc();
        if (!downloaded || av_hwframe_transfer_data(downloaded, source, 0) < 0) {
            av_frame_free(&downloaded); return NULL;
        }
        frame = downloaded;
    }
    const bool full = source->color_range == AVCOL_RANGE_JPEG;
    CVPixelBufferRef buffer = pixel_buffer(frame->width, frame->height,
        full ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange);
    if (!buffer) { av_frame_free(&downloaded); return NULL; }
    color->conversion = sws_getCachedContext(color->conversion,
        frame->width, frame->height, frame->format, frame->width, frame->height,
        AV_PIX_FMT_P010, SWS_BILINEAR, NULL, NULL, NULL);
    if (!color->conversion || CVPixelBufferLockBaseAddress(buffer, 0) != kCVReturnSuccess) {
        CVPixelBufferRelease(buffer); av_frame_free(&downloaded); return NULL;
    }
    const int space = source->colorspace == AVCOL_SPC_BT2020_NCL ? SWS_CS_BT2020 :
                      source->colorspace == AVCOL_SPC_SMPTE170M || source->colorspace == AVCOL_SPC_BT470BG ?
                      SWS_CS_ITU601 : SWS_CS_ITU709;
    const int *coefficients = sws_getCoefficients(space);
    int result = sws_setColorspaceDetails(color->conversion, coefficients, full, coefficients,
                                          full, 0, 1 << 16, 1 << 16);
    uint8_t *planes[4] = {CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                          CVPixelBufferGetBaseAddressOfPlane(buffer, 1), NULL, NULL};
    int strides[4] = {(int) CVPixelBufferGetBytesPerRowOfPlane(buffer, 0),
                      (int) CVPixelBufferGetBytesPerRowOfPlane(buffer, 1), 0, 0};
    if (result >= 0)
        result = sws_scale(color->conversion, (const uint8_t *const *) frame->data,
            frame->linesize, 0, frame->height, planes, strides);
    const int height = frame->height;
    CVPixelBufferUnlockBaseAddress(buffer, 0);
    av_frame_free(&downloaded);
    if (result != height) { CVPixelBufferRelease(buffer); return NULL; }
    return buffer;
}

static CVMetalTextureRef metal_plane(UpMetalColor *color, CVPixelBufferRef buffer,
                                      MTLPixelFormat format, int width, int height, int plane)
{
    CVMetalTextureRef texture = NULL;
    const MTLTextureUsage usage = format == MTLPixelFormatRGBA16Float ?
        MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite : MTLTextureUsageShaderRead;
    NSDictionary *attributes = @{(__bridge id)kCVMetalTextureUsage: @(usage)};
    if (CVMetalTextureCacheCreateTextureFromImage(NULL, color->textures, buffer,
            (__bridge CFDictionaryRef) attributes, format, width, height, plane, &texture) != kCVReturnSuccess)
        return NULL;
    return texture;
}

SDL_Texture *up_metal_color_convert(UpMetalColor *color, SDL_Renderer *renderer,
                                     const AVFrame *frame, int field, float peak_nits,
                                     int width, int height,
                                     char *error, size_t size)
{
    @autoreleasepool {
        UpMetalColorState *state = (__bridge UpMetalColorState *) color->state;
        CVPixelBufferRef input = NULL;
        enum AVPixelFormat format = AV_PIX_FMT_NONE;
        if (frame->format == AV_PIX_FMT_VIDEOTOOLBOX) {
            input = (CVPixelBufferRef) frame->data[3];
            if (input) format = av_map_videotoolbox_format_to_pixfmt(CVPixelBufferGetPixelFormatType(input));
        }
        bool uploaded = format != AV_PIX_FMT_NV12 && format != AV_PIX_FMT_P010;
        if (uploaded) { input = upload_software(color, frame); format = AV_PIX_FMT_P010; }
        if (!input) { color_error(error, size, "could not prepare the source planes"); return NULL; }
        // Each output owns a fresh IOSurface. SDL/Metal retain that surface
        // until GPU presentation completes, so no decoder/pixel-buffer pool
        // can recycle memory still being read by another command queue.
        CVPixelBufferRef output = pixel_buffer(width, height, kCVPixelFormatType_64RGBAHalf);
        const bool ten = format == AV_PIX_FMT_P010;
        CVMetalTextureRef y = metal_plane(color, input, ten ? MTLPixelFormatR16Unorm : MTLPixelFormatR8Unorm,
                                           frame->width, frame->height, 0);
        CVMetalTextureRef uv = metal_plane(color, input, ten ? MTLPixelFormatRG16Unorm : MTLPixelFormatRG8Unorm,
                                            (frame->width + 1) / 2, (frame->height + 1) / 2, 1);
        CVMetalTextureRef rgba = output ? metal_plane(color, output, MTLPixelFormatRGBA16Float,
                                                       width, height, 0) : NULL;
        SDL_Texture *texture = NULL;
        if (!y || !uv || !rgba) {
            color_error(error, size, "could not map the GPU planes");
        } else {
            const bool hdr = frame->color_trc == AVCOL_TRC_SMPTE2084 || frame->color_trc == AVCOL_TRC_ARIB_STD_B67;
            uint32_t primaries = frame->color_primaries;
            if (primaries == AVCOL_PRI_UNSPECIFIED) primaries = hdr ? AVCOL_PRI_BT2020 : AVCOL_PRI_BT709;
            float kr = 0.2126f, kb = 0.0722f;
            if (frame->colorspace == AVCOL_SPC_BT2020_NCL ||
                (frame->colorspace == AVCOL_SPC_UNSPECIFIED && hdr)) { kr = 0.2627f; kb = 0.0593f; }
            else if (frame->colorspace == AVCOL_SPC_SMPTE170M || frame->colorspace == AVCOL_SPC_BT470BG ||
                (frame->colorspace == AVCOL_SPC_UNSPECIFIED && frame->width < 1280 && frame->height <= 576)) {
                kr = 0.299f; kb = 0.114f;
            }
            ColorParams params = {.depth = ten ? 10 : 8, .full_range = frame->color_range == AVCOL_RANGE_JPEG,
                .transfer = frame->color_trc, .primaries = primaries, .kr = kr, .kb = kb,
                .peak_nits = peak_nits, .white_nits = 203.0f, .field = field};
            id<MTLCommandBuffer> command = [state.queue commandBuffer];
            id<MTLComputeCommandEncoder> encoder = [command computeCommandEncoder];
            [encoder setComputePipelineState:state.pipeline];
            [encoder setTexture:CVMetalTextureGetTexture(y) atIndex:0];
            [encoder setTexture:CVMetalTextureGetTexture(uv) atIndex:1];
            [encoder setTexture:CVMetalTextureGetTexture(rgba) atIndex:2];
            [encoder setBytes:&params length:sizeof(params) atIndex:0];
            [encoder dispatchThreads:MTLSizeMake(width, height, 1)
                threadsPerThreadgroup:MTLSizeMake(8, 8, 1)];
            [encoder endEncoding];
            [command commit];
            // Complete the conversion before SDL's separate Metal queue samples
            // the result, and before returning the decoded buffer to its pool.
            [command waitUntilCompleted];
            if (command.status == MTLCommandBufferStatusError) {
                color_error(error, size, command.error.localizedDescription.UTF8String);
            } else {
                SDL_PropertiesID properties = SDL_CreateProperties();
                SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, SDL_PIXELFORMAT_RGBA64_FLOAT);
                SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, SDL_COLORSPACE_SRGB_LINEAR);
                SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, width);
                SDL_SetNumberProperty(properties, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, height);
                SDL_SetFloatProperty(properties, SDL_PROP_TEXTURE_CREATE_SDR_WHITE_POINT_FLOAT, 1.0f);
                SDL_SetFloatProperty(properties, SDL_PROP_TEXTURE_CREATE_HDR_HEADROOM_FLOAT,
                                      hdr ? fmaxf(1, peak_nits / 203.0f) : 1.0f);
                SDL_SetPointerProperty(properties, SDL_PROP_TEXTURE_CREATE_METAL_PIXELBUFFER_POINTER, output);
                texture = SDL_CreateTextureWithProperties(renderer, properties);
                SDL_DestroyProperties(properties);
                if (texture) SDL_SetTextureBlendMode(texture, SDL_BLENDMODE_NONE);
                else color_error(error, size, SDL_GetError());
            }
        }
        if (y) CFRelease(y);
        if (uv) CFRelease(uv);
        if (rgba) CFRelease(rgba);
        if (output) CVPixelBufferRelease(output);
        if (uploaded) CVPixelBufferRelease(input);
        CVMetalTextureCacheFlush(color->textures, 0);
        return texture;
    }
}

bool up_metal_color_can_draw(const AVFrame *frame)
{
    if (!frame || frame->format != AV_PIX_FMT_VIDEOTOOLBOX || !frame->data[3]) return false;
    const enum AVPixelFormat format = av_map_videotoolbox_format_to_pixfmt(
        CVPixelBufferGetPixelFormatType((CVPixelBufferRef) frame->data[3]));
    return format == AV_PIX_FMT_NV12 || format == AV_PIX_FMT_P010;
}

bool up_metal_color_draw(UpMetalColor *color, SDL_Renderer *renderer,
                        const AVFrame *frame, int field, float peak_nits,
                        SDL_FRect source, SDL_FRect destination, uint32_t rotation,
                        int width, int height, char *error, size_t size)
{
    @autoreleasepool {
        UpMetalColorState *state = (__bridge UpMetalColorState *) color->state;
        CAMetalLayer *layer = (__bridge CAMetalLayer *) SDL_GetRenderMetalLayer(renderer);
        SDL_Texture *target = SDL_GetRenderTarget(renderer);
        MTLPixelFormat output = target && target->format == SDL_PIXELFORMAT_RGBA128_FLOAT ?
            MTLPixelFormatRGBA32Float : layer.pixelFormat;
        id<MTLRenderPipelineState> pipeline = state.drawPipelines[@(output)];
        if (!pipeline) {
            MTLRenderPipelineDescriptor *descriptor = [MTLRenderPipelineDescriptor new];
            descriptor.vertexFunction = [state.library newFunctionWithName:@"video_vertex"];
            descriptor.fragmentFunction = [state.library newFunctionWithName:@"video_fragment"];
            descriptor.colorAttachments[0].pixelFormat = output;
            NSError *failure = nil;
            pipeline = [state.device newRenderPipelineStateWithDescriptor:descriptor error:&failure];
            if (!pipeline) { color_error(error, size, failure.localizedDescription.UTF8String); return false; }
            state.drawPipelines[@(output)] = pipeline;
        }
        CVPixelBufferRef input = (CVPixelBufferRef) frame->data[3];
        const bool ten = av_map_videotoolbox_format_to_pixfmt(CVPixelBufferGetPixelFormatType(input)) == AV_PIX_FMT_P010;
        CVMetalTextureRef y = metal_plane(color, input, ten ? MTLPixelFormatR16Unorm : MTLPixelFormatR8Unorm,
            frame->width, frame->height, 0);
        CVMetalTextureRef uv = metal_plane(color, input, ten ? MTLPixelFormatRG16Unorm : MTLPixelFormatRG8Unorm,
            (frame->width + 1) / 2, (frame->height + 1) / 2, 1);
        AVFrame *held = av_frame_clone(frame);
        if (!y || !uv || !held || !SDL_FlushRenderer(renderer)) {
            if (y) CFRelease(y);
            if (uv) CFRelease(uv);
            av_frame_free(&held);
            color_error(error, size, "could not map the decoder surface"); return false;
        }
        id<MTLRenderCommandEncoder> encoder = (__bridge id<MTLRenderCommandEncoder>) SDL_GetRenderMetalCommandEncoder(renderer);
        id<MTLFence> fence = [state.device newFence];
        id<MTLCommandBuffer> retirement = [state.queue commandBuffer];
        if (!encoder || !fence || !retirement) {
            CFRelease(y); CFRelease(uv); av_frame_free(&held);
            color_error(error, size, "could not synchronize the decoder surface"); return false;
        }
        const bool hdr = frame->color_trc == AVCOL_TRC_SMPTE2084 || frame->color_trc == AVCOL_TRC_ARIB_STD_B67;
        uint32_t primaries = frame->color_primaries;
        if (primaries == AVCOL_PRI_UNSPECIFIED) primaries = hdr ? AVCOL_PRI_BT2020 : AVCOL_PRI_BT709;
        float kr = 0.2126f, kb = 0.0722f;
        if (frame->colorspace == AVCOL_SPC_BT2020_NCL || (frame->colorspace == AVCOL_SPC_UNSPECIFIED && hdr)) {
            kr = 0.2627f; kb = 0.0593f;
        } else if (frame->colorspace == AVCOL_SPC_SMPTE170M || frame->colorspace == AVCOL_SPC_BT470BG ||
            (frame->colorspace == AVCOL_SPC_UNSPECIFIED && frame->width < 1280 && frame->height <= 576)) {
            kr = 0.299f; kb = 0.114f;
        }
        ColorParams params = {.depth = ten ? 10 : 8, .full_range = frame->color_range == AVCOL_RANGE_JPEG,
            .transfer = frame->color_trc, .primaries = primaries, .kr = kr, .kb = kb,
            .peak_nits = peak_nits, .white_nits = 203, .field = field};
        const float input_headroom = hdr ? fmaxf(1, peak_nits / 203) : 1;
        const float output_headroom = SDL_GetFloatProperty(target ? SDL_GetTextureProperties(target) : SDL_GetRendererProperties(renderer),
            target ? SDL_PROP_TEXTURE_HDR_HEADROOM_FLOAT : SDL_PROP_RENDERER_HDR_HEADROOM_FLOAT, 1);
        const float tone[2] = {output_headroom / (input_headroom * input_headroom),
            input_headroom > output_headroom && output_headroom > 0 ? 1 / output_headroom : 0};
        float vertices[4][4];
        for (int i = 0; i < 4; i++) {
            float x = i & 1, ycoord = i >> 1;
            vertices[i][0] = 2 * (destination.x + x * destination.w) / width - 1;
            vertices[i][1] = 1 - 2 * (destination.y + ycoord * destination.h) / height;
            float u = x, v = ycoord;
            switch (rotation % 4) {
            case 1: u = ycoord; v = 1 - x; break;
            case 2: u = 1 - x; v = 1 - ycoord; break;
            case 3: u = 1 - ycoord; v = x; break;
            }
            vertices[i][2] = source.x + u * source.w - 0.5f;
            vertices[i][3] = source.y + v * source.h - 0.5f;
        }
        [encoder setViewport:(MTLViewport) {0, 0, width, height, 0, 1}];
        [encoder setScissorRect:(MTLScissorRect) {0, 0, width, height}];
        [encoder setRenderPipelineState:pipeline];
        [encoder setVertexBytes:vertices length:sizeof(vertices) atIndex:0];
        [encoder setFragmentTexture:CVMetalTextureGetTexture(y) atIndex:0];
        [encoder setFragmentTexture:CVMetalTextureGetTexture(uv) atIndex:1];
        [encoder setFragmentBytes:&params length:sizeof(params) atIndex:0];
        [encoder setFragmentBytes:tone length:sizeof(tone) atIndex:1];
        [encoder drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4];
        [encoder updateFence:fence afterStages:MTLRenderStageFragment];
        id<MTLBlitCommandEncoder> release = [retirement blitCommandEncoder];
        [release waitForFence:fence];
        [release endEncoding];
        [retirement addCompletedHandler:^(id<MTLCommandBuffer> completed) {
            if (completed.status == MTLCommandBufferStatusError)
                fprintf(stderr, "Metal surface retirement: %s\n", completed.error.localizedDescription.UTF8String);
            AVFrame *finished = held;
            av_frame_free(&finished);
            CFRelease(y); CFRelease(uv);
        }];
        state.retirement = retirement;
        // Discard unused mappings without pinning a second set of decoder
        // surfaces in the texture cache. In-flight wrappers are retained above.
        CVMetalTextureCacheFlush(color->textures, 0);
        return true;
    }
}

void up_metal_color_after_present(UpMetalColor *color)
{
    if (!color) return;
    @autoreleasepool {
        UpMetalColorState *state = (__bridge UpMetalColorState *) color->state;
        // Metal 3 fences work across queues on one device. The producing SDL
        // command buffer must be committed first, by SDL_RenderPresent.
        [state.retirement commit];
        state.retirement = nil;
    }
}

void up_metal_color_destroy(UpMetalColor *color)
{
    if (!color) return;
    if (color->textures) CFRelease(color->textures);
    if (color->state) CFRelease(color->state);
    sws_freeContext(color->conversion);
    free(color);
}
