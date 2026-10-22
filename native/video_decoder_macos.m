#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#include "video_decoder_macos.h"
#include <pthread.h>

// Keep submissions in packet order even if callbacks arrive out of order.
// Four in flight overlap the hardware work without an unbounded surface queue.
#define DECODE_DEPTH 4
struct DecodeJob {
    struct DecodeJob *next;
    AVFrame *frame;
    bool complete;
    int error;
};
struct UpAsyncDecoder {
    VTDecompressionSessionRef session;
    CMVideoFormatDescriptionRef format;
    AVFrame *properties;
    AVRational time_base;
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    struct DecodeJob *head, *tail;
    int count, error;
    bool draining;
};

static void release_surface(void *opaque, uint8_t *data)
{
    (void)data;
    CVPixelBufferRelease(opaque);
}

static void decoded(void *opaque, void *reference, OSStatus status, VTDecodeInfoFlags flags,
                    CVImageBufferRef image, CMTime pts, CMTime duration)
{
    (void)pts; (void)duration;
    UpAsyncDecoder *decoder = opaque;
    struct DecodeJob *job = reference;
    int error = status ? AVERROR_EXTERNAL : 0;
    if (!error && image && !(flags & kVTDecodeInfo_FrameDropped)) {
        AVFrame *frame = job->frame;
        frame->format = AV_PIX_FMT_VIDEOTOOLBOX;
        frame->width = (int)CVPixelBufferGetWidth(image);
        frame->height = (int)CVPixelBufferGetHeight(image);
        CVPixelBufferRetain(image);
        frame->buf[0] = av_buffer_create((uint8_t *)image, 0, release_surface, image, 0);
        if (!frame->buf[0]) { CVPixelBufferRelease(image); error = AVERROR(ENOMEM); }
        else frame->data[3] = (uint8_t *)image;
    } else if (!error && !(flags & kVTDecodeInfo_FrameDropped)) {
        error = AVERROR_EXTERNAL;
    }
    pthread_mutex_lock(&decoder->mutex);
    job->error = error;
    job->complete = true;
    pthread_cond_signal(&decoder->changed);
    pthread_mutex_unlock(&decoder->mutex);
}

static OSStatus start_session(UpAsyncDecoder *decoder)
{
    NSDictionary *spec = @{(__bridge id)kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder:@YES};
    NSDictionary *attributes = @{
        (__bridge id)kCVPixelBufferPixelFormatTypeKey:@(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
        (__bridge id)kCVPixelBufferIOSurfacePropertiesKey:@{},
        (__bridge id)kCVPixelBufferMetalCompatibilityKey:@YES,
    };
    VTDecompressionOutputCallbackRecord callback = {decoded, decoder};
    return VTDecompressionSessionCreate(NULL, decoder->format, (__bridge CFDictionaryRef)spec,
        (__bridge CFDictionaryRef)attributes, &callback, &decoder->session);
}

static void clear_jobs(UpAsyncDecoder *decoder)
{
    while (decoder->head) {
        struct DecodeJob *job = decoder->head;
        decoder->head = job->next;
        av_frame_free(&job->frame);
        free(job);
    }
    decoder->tail = NULL;
    decoder->count = 0;
}

void up_async_decoder_destroy(UpAsyncDecoder *decoder)
{
    if (!decoder) return;
    if (decoder->session) {
        VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
        VTDecompressionSessionInvalidate(decoder->session);
        CFRelease(decoder->session);
    }
    clear_jobs(decoder);
    if (decoder->format) CFRelease(decoder->format);
    av_frame_free(&decoder->properties);
    pthread_cond_destroy(&decoder->changed);
    pthread_mutex_destroy(&decoder->mutex);
    free(decoder);
}

UpAsyncDecoder *up_async_decoder_create(const AVCodecParameters *p, AVRational time_base)
{
    // Start with high-resolution 8-bit SDR AV1 in av1C containers. Other codecs,
    // HDR and raw sequence headers retain FFmpeg's metadata/format handling.
    if (p->codec_id != AV_CODEC_ID_AV1 || p->format != AV_PIX_FMT_YUV420P ||
        (p->width <= 3840 && p->height <= 2160) ||
        p->color_range == AVCOL_RANGE_JPEG ||
        p->color_trc == AVCOL_TRC_SMPTE2084 || p->color_trc == AVCOL_TRC_ARIB_STD_B67 ||
        p->extradata_size < 4 || p->extradata[0] != 0x81 ||
        time_base.num <= 0 || time_base.den <= 0)
        return NULL;
    @autoreleasepool {
        UpAsyncDecoder *decoder = calloc(1, sizeof(*decoder));
        if (!decoder) return NULL;
        pthread_mutex_init(&decoder->mutex, NULL);
        pthread_cond_init(&decoder->changed, NULL);
        decoder->time_base = time_base;
        NSDictionary *extensions = nil;
        decoder->properties = av_frame_alloc();
        if (!decoder->properties) goto fail;
        AVFrame *frame = decoder->properties;
        frame->sample_aspect_ratio = p->sample_aspect_ratio;
        frame->color_range = AVCOL_RANGE_MPEG;
        frame->colorspace = p->color_space;
        frame->color_primaries = p->color_primaries;
        frame->color_trc = p->color_trc;
        frame->chroma_location = p->chroma_location;
        frame->time_base = time_base;
        for (int i = 0; i < p->nb_coded_side_data; i++) {
            const AVPacketSideData *side = &p->coded_side_data[i];
            // Preserve rotation metadata; leave unfamiliar metadata to FFmpeg.
            if (side->type != AV_PKT_DATA_DISPLAYMATRIX) goto fail;
            AVFrameSideData *copy = av_frame_new_side_data(frame, AV_FRAME_DATA_DISPLAYMATRIX, side->size);
            if (!copy) goto fail;
            memcpy(copy->data, side->data, side->size);
        }
        extensions = @{(__bridge id)kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms:
            @{@"av1C":[NSData dataWithBytes:p->extradata length:p->extradata_size]}};
        if (CMVideoFormatDescriptionCreate(NULL, kCMVideoCodecType_AV1, p->width, p->height,
                (__bridge CFDictionaryRef)extensions, &decoder->format) != noErr ||
            start_session(decoder) != noErr)
            goto fail;
        return decoder;
fail:
        up_async_decoder_destroy(decoder);
        return NULL;
    }
}

int up_async_decoder_send(UpAsyncDecoder *decoder, const AVPacket *packet)
{
    if (decoder->error) return decoder->error;
    if (decoder->draining) return AVERROR_EOF;
    if (!packet) {
        decoder->draining = true;
        if (VTDecompressionSessionWaitForAsynchronousFrames(decoder->session) != noErr)
            return decoder->error = AVERROR_EXTERNAL;
        return 0;
    }
    if (decoder->count >= DECODE_DEPTH) return AVERROR(EAGAIN);
    if (packet->size <= 0) return AVERROR_INVALIDDATA;
    // A changed sequence needs a newly configured session, not stale av1C data.
    if (av_packet_get_side_data(packet, AV_PKT_DATA_NEW_EXTRADATA, NULL))
        return AVERROR_INPUT_CHANGED;
    struct DecodeJob *job = calloc(1, sizeof(*job));
    if (!job) return AVERROR(ENOMEM);
    job->frame = av_frame_alloc();
    if (!job->frame || av_frame_copy_props(job->frame, decoder->properties) < 0) {
        av_frame_free(&job->frame); free(job); return AVERROR(ENOMEM);
    }
    job->frame->pts = job->frame->best_effort_timestamp = packet->pts;
    job->frame->pkt_dts = packet->dts;
    job->frame->duration = packet->duration;
    if (packet->flags & AV_PKT_FLAG_KEY) job->frame->flags |= AV_FRAME_FLAG_KEY;
    CMBlockBufferRef block = NULL;
    CMSampleBufferRef sample = NULL;
    size_t bytes = packet->size;
    OSStatus status = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, bytes, NULL, NULL, 0, bytes, 0, &block);
    if (!status) status = CMBlockBufferReplaceDataBytes(packet->data, block, 0, bytes);
    CMSampleTimingInfo timing = {
        CMTimeMake(packet->duration * (int64_t)decoder->time_base.num, decoder->time_base.den),
        packet->pts == AV_NOPTS_VALUE ? kCMTimeInvalid : CMTimeMake(packet->pts * (int64_t)decoder->time_base.num, decoder->time_base.den),
        kCMTimeInvalid,
    };
    if (!status) status = CMSampleBufferCreateReady(NULL, block, decoder->format, 1, 1, &timing, 1, &bytes, &sample);
    if (!status) {
        if (decoder->tail) decoder->tail->next = job;
        else decoder->head = job;
        decoder->tail = job;
        decoder->count++;
        status = VTDecompressionSessionDecodeFrame(decoder->session, sample,
            kVTDecodeFrame_EnableAsynchronousDecompression, job, NULL);
        if (status) {
            // Failed submissions do not invoke the callback.
            pthread_mutex_lock(&decoder->mutex);
            job->error = AVERROR_EXTERNAL;
            job->complete = true;
            pthread_mutex_unlock(&decoder->mutex);
        }
    } else {
        av_frame_free(&job->frame);
        free(job);
    }
    if (sample) CFRelease(sample);
    if (block) CFRelease(block);
    return status ? (decoder->error = AVERROR_EXTERNAL) : 0;
}

int up_async_decoder_receive(UpAsyncDecoder *decoder, AVFrame **frame)
{
    if (decoder->error) return decoder->error;
    while (decoder->head) {
        struct DecodeJob *job = decoder->head;
        pthread_mutex_lock(&decoder->mutex);
        while (!job->complete && decoder->count >= DECODE_DEPTH)
            pthread_cond_wait(&decoder->changed, &decoder->mutex);
        bool ready = job->complete;
        pthread_mutex_unlock(&decoder->mutex);
        if (!ready) return AVERROR(EAGAIN);
        decoder->head = job->next;
        if (!decoder->head) decoder->tail = NULL;
        decoder->count--;
        int error = job->error;
        AVFrame *result = job->frame;
        free(job);
        if (error || !result->data[3]) {
            av_frame_free(&result);
            if (error) return decoder->error = error;
            continue;
        }
        *frame = result;
        return 0;
    }
    return decoder->draining ? AVERROR_EOF : AVERROR(EAGAIN);
}

void up_async_decoder_flush(UpAsyncDecoder *decoder)
{
    @autoreleasepool {
        if (decoder->session) {
            VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
            VTDecompressionSessionInvalidate(decoder->session);
            CFRelease(decoder->session);
            decoder->session = NULL;
        }
        clear_jobs(decoder);
        decoder->draining = false;
        decoder->error = start_session(decoder) == noErr ? 0 : AVERROR_EXTERNAL;
    }
}
