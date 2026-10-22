#pragma once

#include "ffmpeg_compat.h"
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>
#include <stdlib.h>
#ifdef __APPLE__
#include <CoreVideo/CoreVideo.h>
#include <libavutil/hwcontext_videotoolbox.h>
#endif

static void up_luma_view_free(UpLumaView *view);

// Expose the luma plane without conversion. Readback runs in the crop/decode
// workers; the presentation thread keeps its original frame.
static int up_luma_view(const AVFrame *source, UpLumaView *view)
{
    *view = (UpLumaView) {0};
    const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(source->format);
    if (!desc)
        return 0;
    AVFrame *frame = NULL;
#ifdef __APPLE__
    if (source->format == AV_PIX_FMT_VIDEOTOOLBOX && source->data[3]) {
        CVPixelBufferRef buffer = (CVPixelBufferRef) source->data[3];
        const enum AVPixelFormat format = av_map_videotoolbox_format_to_pixfmt(CVPixelBufferGetPixelFormatType(buffer));
        if ((format == AV_PIX_FMT_NV12 || format == AV_PIX_FMT_P010) &&
            CVPixelBufferIsPlanar(buffer) && source->width > 0 && source->height > 0 &&
            source->width <= (int) CVPixelBufferGetWidthOfPlane(buffer, 0) &&
            source->height <= (int) CVPixelBufferGetHeightOfPlane(buffer, 0)) {
            frame = av_frame_clone(source);
            if (!frame) return 0;
            if (CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess)
                goto fail;
            // Apple Silicon decoder surfaces are CPU-addressable. Keep the
            // decoded frame alive and locked while the crop worker samples it;
            // do not copy every plane of a 4K/8K picture into another AVFrame.
            *view = (UpLumaView) {
                .frame = (UpAvFrame *) frame, .data = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                .width = source->width, .height = source->height,
                .stride = (int) CVPixelBufferGetBytesPerRowOfPlane(buffer, 0),
                .step = format == AV_PIX_FMT_P010 ? 2 : 1,
                .depth = format == AV_PIX_FMT_P010 ? 10 : 8,
                .shift = format == AV_PIX_FMT_P010 ? 6 : 0,
                .full_range = source->color_range == AVCOL_RANGE_JPEG,
            };
            if (view->data) return 1;
            up_luma_view_free(view);
            return 0;
        }
    }
#endif
    if (desc->flags & AV_PIX_FMT_FLAG_HWACCEL) {
        frame = av_frame_alloc();
        if (!frame || av_hwframe_transfer_data(frame, source, 0) < 0)
            goto fail;
        desc = av_pix_fmt_desc_get(frame->format);
    } else {
        frame = av_frame_clone(source);
    }
    if (!frame || !desc || !desc->nb_components ||
        (desc->flags & (AV_PIX_FMT_FLAG_RGB | AV_PIX_FMT_FLAG_PAL |
                       AV_PIX_FMT_FLAG_BITSTREAM | AV_PIX_FMT_FLAG_FLOAT | AV_PIX_FMT_FLAG_HWACCEL)))
        goto fail;
    const AVComponentDescriptor *luma = &desc->comp[0];
    if (frame->width <= 0 || frame->height <= 0 ||
        frame->width > 32768 || frame->height > 32768 ||
        luma->depth < 8 || luma->depth > 16 || luma->shift < 0 ||
        luma->depth + luma->shift > 16 || luma->step < 1 || luma->step > 8 ||
        luma->offset < 0 || !frame->data[luma->plane])
        goto fail;
    const int bytes = (luma->depth + luma->shift + 7) / 8;
    const int64_t row_size = (int64_t) (frame->width - 1) * luma->step + luma->offset + bytes;
    if (llabs((int64_t) frame->linesize[luma->plane]) < row_size)
        goto fail;
    *view = (UpLumaView) {
        .frame = (UpAvFrame *) frame,
        .data = frame->data[luma->plane] + luma->offset,
        .width = frame->width, .height = frame->height,
        .stride = frame->linesize[luma->plane], .step = luma->step,
        .depth = luma->depth, .shift = luma->shift,
        .big_endian = !!(desc->flags & AV_PIX_FMT_FLAG_BE),
        .full_range = source->color_range == AVCOL_RANGE_JPEG,
    };
    return 1;
fail:
    av_frame_free(&frame);
    return 0;
}

static void up_luma_view_free(UpLumaView *view)
{
    AVFrame *frame = (AVFrame *) view->frame;
#ifdef __APPLE__
    if (frame && frame->format == AV_PIX_FMT_VIDEOTOOLBOX)
        CVPixelBufferUnlockBaseAddress((CVPixelBufferRef) frame->data[3], kCVPixelBufferLock_ReadOnly);
#endif
    av_frame_free(&frame);
    *view = (UpLumaView) {0};
}

