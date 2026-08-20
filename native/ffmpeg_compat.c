#include "ffmpeg_compat.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/channel_layout.h>
#include <libavutil/dict.h>
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>

#if LIBAVFORMAT_VERSION_MAJOR < 62 || LIBAVCODEC_VERSION_MAJOR < 62 || \
    LIBAVUTIL_VERSION_MAJOR < 60
#error "undefined-player requires FFmpeg 8 or newer development headers"
#endif

#include <errno.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct UpAvDecoder {
    AVCodecContext *context;
    int stream_index;
    AVRational time_base;
    int uses_vulkan;
};

struct UpAvAudioConverter {
    SwrContext *context;
    AVChannelLayout output_layout;
};

struct UpAvSubtitle {
    AVSubtitle value;
};

static _Thread_local char decoder_error[256];

#define FORMAT(value) ((AVFormatContext *) (value))
#define PACKET(value) ((AVPacket *) (value))
#define FRAME(value) ((AVFrame *) (value))

static AVStream *stream_at(const UpAvFormat *format, unsigned int index)
{
    AVFormatContext *native = FORMAT(format);
    return native && index < native->nb_streams ? native->streams[index] : NULL;
}

static enum AVMediaType media_type(enum UpMediaType type)
{
    switch (type) {
    case UP_MEDIA_TYPE_AUDIO:
        return AVMEDIA_TYPE_AUDIO;
    case UP_MEDIA_TYPE_SUBTITLE:
        return AVMEDIA_TYPE_SUBTITLE;
    default:
        return AVMEDIA_TYPE_VIDEO;
    }
}

static enum UpMediaType up_media_type(enum AVMediaType type)
{
    switch (type) {
    case AVMEDIA_TYPE_AUDIO:
        return UP_MEDIA_TYPE_AUDIO;
    case AVMEDIA_TYPE_SUBTITLE:
        return UP_MEDIA_TYPE_SUBTITLE;
    default:
        return UP_MEDIA_TYPE_VIDEO;
    }
}

static enum AVPixelFormat choose_vulkan_format(AVCodecContext *context,
                                                const enum AVPixelFormat *formats)
{
    (void) context;
    for (const enum AVPixelFormat *format = formats;
         format && *format != AV_PIX_FMT_NONE; format++) {
        if (*format == AV_PIX_FMT_VULKAN)
            return *format;
    }
    return AV_PIX_FMT_NONE;
}

static int decoder_supports_vulkan(const AVCodec *codec)
{
    for (int index = 0;; index++) {
        const AVCodecHWConfig *config = avcodec_get_hw_config(codec, index);
        if (!config)
            return 0;
        if (config->device_type == AV_HWDEVICE_TYPE_VULKAN &&
            config->pix_fmt == AV_PIX_FMT_VULKAN &&
            (config->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX))
            return 1;
    }
}

static void set_decoder_error(const char *context, int error)
{
    char detail[128] = "unknown error";
    if (error < 0)
        av_strerror(error, detail, sizeof(detail));
    snprintf(decoder_error, sizeof(decoder_error), "%s: %s", context, detail);
}

int up_av_error_string(int code, char *buffer, size_t buffer_size)
{
    return av_strerror(code, buffer, buffer_size);
}

int up_av_format_open(UpAvFormat **format, const char *path)
{
    AVFormatContext *native = NULL;
    int result = avformat_open_input(&native, path, NULL, NULL);
    *format = (UpAvFormat *) native;
    return result;
}

int up_av_format_find_stream_info(UpAvFormat *format)
{
    return avformat_find_stream_info(FORMAT(format), NULL);
}

void up_av_format_close(UpAvFormat **format)
{
    AVFormatContext *native = FORMAT(*format);
    avformat_close_input(&native);
    *format = (UpAvFormat *) native;
}

int up_av_find_best_stream(UpAvFormat *format, enum UpMediaType type,
                           int related_stream)
{
    return av_find_best_stream(FORMAT(format), media_type(type), -1,
                               related_stream, NULL, 0);
}

unsigned int up_av_stream_count(const UpAvFormat *format)
{
    return format ? FORMAT(format)->nb_streams : 0;
}

enum UpMediaType up_av_stream_type(const UpAvFormat *format,
                                   unsigned int stream_index)
{
    AVStream *stream = stream_at(format, stream_index);
    return stream ? up_media_type(stream->codecpar->codec_type) : UP_MEDIA_TYPE_VIDEO;
}

int up_av_stream_is_default(const UpAvFormat *format,
                            unsigned int stream_index)
{
    AVStream *stream = stream_at(format, stream_index);
    return stream && (stream->disposition & AV_DISPOSITION_DEFAULT);
}

const char *up_av_stream_codec_name(const UpAvFormat *format,
                                    unsigned int stream_index)
{
    AVStream *stream = stream_at(format, stream_index);
    return stream ? avcodec_get_name(stream->codecpar->codec_id) : "unknown";
}

double up_av_format_duration(const UpAvFormat *format)
{
    const int64_t duration = format ? FORMAT(format)->duration : AV_NOPTS_VALUE;
    return duration != AV_NOPTS_VALUE && duration > 0
        ? (double) duration / AV_TIME_BASE : NAN;
}

int up_av_read_frame(UpAvFormat *format, UpAvPacket *packet)
{
    return av_read_frame(FORMAT(format), PACKET(packet));
}

int up_av_seek(UpAvFormat *format, int stream_index, double target_seconds)
{
    AVStream *stream = stream_at(format, (unsigned int) stream_index);
    if (!stream || stream->time_base.num <= 0 || stream->time_base.den <= 0)
        return AVERROR(EINVAL);
    int64_t timestamp = llround(target_seconds * stream->time_base.den /
                                stream->time_base.num);
    return av_seek_frame(FORMAT(format), stream_index, timestamp,
                         AVSEEK_FLAG_BACKWARD);
}

int up_av_index_entry_time(const UpAvFormat *format, int stream_index,
                           double target_seconds, int backward,
                           double *entry_seconds)
{
    AVStream *stream = stream_at(format, (unsigned int) stream_index);
    if (!stream || stream->time_base.num <= 0 || stream->time_base.den <= 0)
        return 0;
    int64_t timestamp = llround(target_seconds * stream->time_base.den /
                                stream->time_base.num);
    const AVIndexEntry *entry = avformat_index_get_entry_from_timestamp(
        stream, timestamp, backward ? AVSEEK_FLAG_BACKWARD : 0);
    if (!entry)
        return 0;
    *entry_seconds = entry->timestamp * av_q2d(stream->time_base);
    return 1;
}

UpAvDecoder *up_av_decoder_open(UpAvFormat *format, int stream_index,
                                void *vulkan_device, int prefer_vulkan)
{
    decoder_error[0] = '\0';
    AVStream *stream = stream_at(format, (unsigned int) stream_index);
    if (!stream) {
        snprintf(decoder_error, sizeof(decoder_error), "invalid stream index");
        return NULL;
    }
    AVCodecParameters *parameters = stream->codecpar;
    const AVCodec *codec;
    if (prefer_vulkan && vulkan_device) {
        const char *name = avcodec_get_name(parameters->codec_id);
        codec = avcodec_find_decoder_by_name(name);
    } else {
        codec = avcodec_find_decoder(parameters->codec_id);
    }
    if (!codec) {
        snprintf(decoder_error, sizeof(decoder_error),
                 "no decoder is available for the selected stream");
        return NULL;
    }

    UpAvDecoder *decoder = calloc(1, sizeof(*decoder));
    if (!decoder) {
        snprintf(decoder_error, sizeof(decoder_error), "out of memory");
        return NULL;
    }
    decoder->context = avcodec_alloc_context3(codec);
    decoder->stream_index = stream_index;
    decoder->time_base = stream->time_base;
    if (!decoder->context) {
        snprintf(decoder_error, sizeof(decoder_error), "out of memory");
        free(decoder);
        return NULL;
    }
    int result = avcodec_parameters_to_context(decoder->context, parameters);
    if (result < 0) {
        set_decoder_error("could not configure decoder", result);
        goto fail;
    }
    decoder->context->pkt_timebase = stream->time_base;
    decoder->uses_vulkan = prefer_vulkan && vulkan_device &&
        decoder_supports_vulkan(codec);
    if (decoder->uses_vulkan) {
        decoder->context->get_format = choose_vulkan_format;
        decoder->context->hw_device_ctx = av_buffer_ref(vulkan_device);
        decoder->context->extra_hw_frames = 16;
        if (!decoder->context->hw_device_ctx) {
            snprintf(decoder_error, sizeof(decoder_error),
                     "could not retain the Vulkan decoder device");
            goto fail;
        }
    }
    result = avcodec_open2(decoder->context, codec, NULL);
    if (result < 0) {
        set_decoder_error("could not open decoder", result);
        goto fail;
    }
    return decoder;

fail:
    avcodec_free_context(&decoder->context);
    free(decoder);
    return NULL;
}

const char *up_av_decoder_error(void)
{
    return decoder_error[0] ? decoder_error : "unknown decoder error";
}

void up_av_decoder_free(UpAvDecoder **decoder)
{
    if (!decoder || !*decoder)
        return;
    avcodec_free_context(&(*decoder)->context);
    free(*decoder);
    *decoder = NULL;
}

int up_av_decoder_stream_index(const UpAvDecoder *decoder)
{
    return decoder->stream_index;
}

double up_av_decoder_time_base(const UpAvDecoder *decoder)
{
    return av_q2d(decoder->time_base);
}

int up_av_decoder_uses_vulkan(const UpAvDecoder *decoder)
{
    return decoder->uses_vulkan;
}

int up_av_decoder_width(const UpAvDecoder *decoder)
{
    return decoder->context->width;
}

int up_av_decoder_height(const UpAvDecoder *decoder)
{
    return decoder->context->height;
}

int up_av_decoder_send_packet(UpAvDecoder *decoder, const UpAvPacket *packet)
{
    return avcodec_send_packet(decoder->context, PACKET(packet));
}

int up_av_decoder_receive_frame(UpAvDecoder *decoder, UpAvFrame **frame)
{
    AVFrame *native = av_frame_alloc();
    if (!native)
        return AVERROR(ENOMEM);
    int result = avcodec_receive_frame(decoder->context, native);
    if (result < 0) {
        av_frame_free(&native);
        return result;
    }
    *frame = (UpAvFrame *) native;
    return 0;
}

void up_av_decoder_flush(UpAvDecoder *decoder)
{
    avcodec_flush_buffers(decoder->context);
}

UpAvPacket *up_av_packet_alloc(void)
{
    return (UpAvPacket *) av_packet_alloc();
}

void up_av_packet_free(UpAvPacket **packet)
{
    AVPacket *native = PACKET(*packet);
    av_packet_free(&native);
    *packet = (UpAvPacket *) native;
}

void up_av_packet_unref(UpAvPacket *packet)
{
    av_packet_unref(PACKET(packet));
}

int up_av_packet_stream_index(const UpAvPacket *packet)
{
    return PACKET(packet)->stream_index;
}

int64_t up_av_packet_pts(const UpAvPacket *packet)
{
    return PACKET(packet)->pts;
}

int64_t up_av_packet_duration(const UpAvPacket *packet)
{
    return PACKET(packet)->duration;
}

void up_av_frame_free(UpAvFrame **frame)
{
    AVFrame *native = FRAME(*frame);
    av_frame_free(&native);
    *frame = (UpAvFrame *) native;
}

int up_av_frame_is_vulkan(const UpAvFrame *frame)
{
    return FRAME(frame)->format == AV_PIX_FMT_VULKAN;
}

int64_t up_av_frame_timestamp(const UpAvFrame *frame)
{
    return FRAME(frame)->best_effort_timestamp;
}

int64_t up_av_frame_duration(const UpAvFrame *frame)
{
    return FRAME(frame)->duration;
}

static int64_t metadata_bitrate(AVStream *stream)
{
    const AVDictionaryEntry *entry = av_dict_get(stream->metadata, "BPS", NULL,
                                                  AV_DICT_IGNORE_SUFFIX);
    if (!entry || !entry->value)
        return 0;
    errno = 0;
    char *end = NULL;
    long long value = strtoll(entry->value, &end, 10);
    return errno == 0 && end != entry->value ? value : 0;
}

int up_av_video_info(const UpAvFormat *format, const UpAvDecoder *decoder,
                     const UpAvFrame *frame, UpVideoInfo *info)
{
    AVFormatContext *native_format = FORMAT(format);
    AVStream *stream = stream_at(format, (unsigned int) decoder->stream_index);
    AVFrame *native_frame = FRAME(frame);
    if (!native_format || !stream || !native_frame || !info)
        return 0;
    AVCodecParameters *parameters = stream->codecpar;
    memset(info, 0, sizeof(*info));
    info->codec = avcodec_get_name(parameters->codec_id);
    info->profile = avcodec_profile_name(parameters->codec_id, parameters->profile);
    enum AVPixelFormat pixel_format = decoder->context->sw_pix_fmt != AV_PIX_FMT_NONE
        ? decoder->context->sw_pix_fmt : parameters->format;
    info->pixel_format = av_get_pix_fmt_name(pixel_format);
    info->width = native_frame->width;
    info->height = native_frame->height;
    info->declared_bitrate = parameters->bit_rate;
    info->metadata_bitrate = metadata_bitrate(stream);
    info->container_bitrate = native_format->bit_rate;
    info->frame_rate = av_q2d(av_guess_frame_rate(native_format, stream, NULL));

    const int assume_hd = info->width >= 1280 || info->height >= 720;
    enum AVColorSpace space = native_frame->colorspace;
    if (space == AVCOL_SPC_UNSPECIFIED)
        space = parameters->color_space;
    if (space == AVCOL_SPC_UNSPECIFIED && assume_hd) {
        space = AVCOL_SPC_BT709;
        info->color_space_assumed = 1;
    }
    info->color_space = av_color_space_name(space);

    enum AVColorPrimaries primaries = native_frame->color_primaries;
    if (primaries == AVCOL_PRI_UNSPECIFIED)
        primaries = parameters->color_primaries;
    if (primaries == AVCOL_PRI_UNSPECIFIED && assume_hd) {
        primaries = AVCOL_PRI_BT709;
        info->color_primaries_assumed = 1;
    }
    info->color_primaries = av_color_primaries_name(primaries);

    enum AVColorTransferCharacteristic transfer = native_frame->color_trc;
    if (transfer == AVCOL_TRC_UNSPECIFIED)
        transfer = parameters->color_trc;
    if (transfer == AVCOL_TRC_UNSPECIFIED && assume_hd) {
        transfer = AVCOL_TRC_BT709;
        info->color_transfer_assumed = 1;
    }
    info->color_transfer = av_color_transfer_name(transfer);
    if (transfer == AVCOL_TRC_SMPTE2084)
        info->hdr_kind = UP_HDR_KIND_PQ;
    else if (transfer == AVCOL_TRC_ARIB_STD_B67)
        info->hdr_kind = UP_HDR_KIND_HLG;
    else if (transfer == AVCOL_TRC_UNSPECIFIED)
        info->hdr_kind = UP_HDR_KIND_UNKNOWN;
    else
        info->hdr_kind = UP_HDR_KIND_SDR;

    enum AVColorRange range = native_frame->color_range;
    if (range == AVCOL_RANGE_UNSPECIFIED)
        range = parameters->color_range;
    if (range == AVCOL_RANGE_UNSPECIFIED) {
        const char *name = info->pixel_format ? info->pixel_format : "";
        int full = !strncmp(name, "yuvj", 4) || !strncmp(name, "rgb", 3) ||
            !strncmp(name, "gbr", 3);
        range = full ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
        info->color_range_assumed = 1;
    }
    info->color_range = av_color_range_name(range);
    return 1;
}

UpAvAudioConverter *up_av_audio_converter_create(const UpAvFrame *frame,
                                                  int output_rate,
                                                  int output_channels,
                                                  int *error)
{
    UpAvAudioConverter *converter = calloc(1, sizeof(*converter));
    if (!converter) {
        if (error)
            *error = AVERROR(ENOMEM);
        return NULL;
    }
    av_channel_layout_default(&converter->output_layout, output_channels);
    AVFrame *native = FRAME(frame);
    int result = swr_alloc_set_opts2(&converter->context,
                                     &converter->output_layout,
                                     AV_SAMPLE_FMT_FLT, output_rate,
                                     &native->ch_layout, native->format,
                                     native->sample_rate, 0, NULL);
    if (result >= 0)
        result = swr_init(converter->context);
    if (result < 0 || !converter->context) {
        if (error)
            *error = result < 0 ? result : AVERROR(ENOMEM);
        swr_free(&converter->context);
        av_channel_layout_uninit(&converter->output_layout);
        free(converter);
        return NULL;
    }
    if (error)
        *error = 0;
    return converter;
}

void up_av_audio_converter_free(UpAvAudioConverter **converter)
{
    if (!converter || !*converter)
        return;
    swr_free(&(*converter)->context);
    av_channel_layout_uninit(&(*converter)->output_layout);
    free(*converter);
    *converter = NULL;
}

int up_av_audio_converter_capacity(UpAvAudioConverter *converter,
                                   const UpAvFrame *frame)
{
    return swr_get_out_samples(converter->context, FRAME(frame)->nb_samples);
}

int up_av_audio_converter_convert(UpAvAudioConverter *converter,
                                  const UpAvFrame *frame, float *output,
                                  int output_frames)
{
    uint8_t *output_planes[] = { (uint8_t *) output };
    AVFrame *native = FRAME(frame);
    return swr_convert(converter->context, output_planes, output_frames,
                       (const uint8_t **) native->extended_data,
                       native->nb_samples);
}

UpAvSubtitle *up_av_decode_subtitle(UpAvDecoder *decoder,
                                    const UpAvPacket *packet, int *result)
{
    UpAvSubtitle *subtitle = calloc(1, sizeof(*subtitle));
    if (!subtitle) {
        *result = AVERROR(ENOMEM);
        return NULL;
    }
    int got_subtitle = 0;
    *result = avcodec_decode_subtitle2(decoder->context, &subtitle->value,
                                       &got_subtitle, PACKET(packet));
    if (*result < 0 || !got_subtitle) {
        if (got_subtitle)
            avsubtitle_free(&subtitle->value);
        free(subtitle);
        return NULL;
    }
    return subtitle;
}

void up_av_subtitle_free(UpAvSubtitle **subtitle)
{
    if (!subtitle || !*subtitle)
        return;
    avsubtitle_free(&(*subtitle)->value);
    free(*subtitle);
    *subtitle = NULL;
}

void up_av_subtitle_info(const UpAvSubtitle *subtitle, UpSubtitleInfo *info)
{
    info->pts = subtitle->value.pts;
    info->start_display_time = subtitle->value.start_display_time;
    info->end_display_time = subtitle->value.end_display_time;
    info->rect_count = subtitle->value.num_rects;
}

int up_av_subtitle_rect(const UpAvSubtitle *subtitle, unsigned int index,
                        UpSubtitleRectView *view)
{
    if (!subtitle || index >= subtitle->value.num_rects || !view)
        return 0;
    AVSubtitleRect *rect = subtitle->value.rects[index];
    if (!rect)
        return 0;
    memset(view, 0, sizeof(*view));
    switch (rect->type) {
    case SUBTITLE_BITMAP:
        view->type = UP_SUBTITLE_RECT_BITMAP;
        break;
    case SUBTITLE_TEXT:
        view->type = UP_SUBTITLE_RECT_TEXT;
        break;
    case SUBTITLE_ASS:
        view->type = UP_SUBTITLE_RECT_ASS;
        break;
    default:
        view->type = UP_SUBTITLE_RECT_OTHER;
        break;
    }
    view->x = rect->x;
    view->y = rect->y;
    view->width = rect->w;
    view->height = rect->h;
    view->line_size = rect->linesize[0];
    view->color_count = rect->nb_colors;
    view->pixels = rect->data[0];
    view->palette = rect->data[1];
    view->text = rect->type == SUBTITLE_ASS ? rect->ass : rect->text;
    return 1;
}
