#include "ffmpeg_compat.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/channel_layout.h>
#include <libavutil/dict.h>
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

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
    double frame_duration;
};

struct UpAvAudioConverter {
    SwrContext *context;
    AVChannelLayout output_layout;
    AVChannelLayout input_layout;
    enum AVSampleFormat input_format;
    int input_rate;
    int output_rate;
};

struct UpAvSubtitle {
    AVSubtitle value;
};

static _Thread_local char decoder_error[256];

int up_av_frame_field(const UpAvFrame *pointer)
{
    const AVFrame *frame = (const AVFrame *) pointer;
    if (!(frame->flags & AV_FRAME_FLAG_INTERLACED))
        return 0;
    return frame->flags & AV_FRAME_FLAG_TOP_FIELD_FIRST ? 1 : 2;
}

void up_av_frame_dimensions(const UpAvFrame *pointer, int *width, int *height)
{
    const AVFrame *frame = (const AVFrame *) pointer;
    *width = frame->width;
    *height = frame->height;
}

UpAvFrame *up_av_frame_clone(const UpAvFrame *frame)
{
    return (UpAvFrame *) av_frame_clone((const AVFrame *) frame);
}

// Expose the luma plane without conversion. Hardware readback is called only
// by the opt-in crop worker; the presentation thread keeps its original frame.
int up_av_frame_luma(const UpAvFrame *pointer, UpLumaView *view)
{
    const AVFrame *source = (const AVFrame *) pointer;
    *view = (UpLumaView) {0};
    const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(source->format);
    if (!desc)
        return 0;
    AVFrame *frame = NULL;
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

void up_av_frame_luma_free(UpLumaView *view)
{
    AVFrame *frame = (AVFrame *) view->frame;
    av_frame_free(&frame);
    *view = (UpLumaView) {0};
}

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

size_t up_av_stream_attached_picture(const UpAvFormat *format,
                                    unsigned int index, const uint8_t **data)
{
    const AVStream *stream = stream_at(format, index);
    *data = NULL;
    if (!stream || !(stream->disposition & AV_DISPOSITION_ATTACHED_PIC) ||
        !stream->attached_pic.data || stream->attached_pic.size <= 0)
        return 0;
    *data = stream->attached_pic.data;
    return (size_t) stream->attached_pic.size;
}

uint8_t *up_av_artwork_png(const UpAvFormat *format, unsigned int index,
                         size_t *png_size)
{
    *png_size = 0;
    const AVStream *stream = stream_at(format, index);
    if (!stream || !(stream->disposition & AV_DISPOSITION_ATTACHED_PIC))
        return NULL;
    const AVCodec *decoder_codec = avcodec_find_decoder(stream->codecpar->codec_id);
    const AVCodec *encoder_codec = avcodec_find_encoder(AV_CODEC_ID_PNG);
    if (!decoder_codec || !encoder_codec)
        return NULL;
    AVCodecContext *decoder = avcodec_alloc_context3(decoder_codec);
    AVCodecContext *encoder = avcodec_alloc_context3(encoder_codec);
    AVFrame *source = av_frame_alloc(), *canvas = av_frame_alloc();
    AVFrame *scaled = av_frame_alloc();
    AVPacket *packet = av_packet_alloc();
    struct SwsContext *scale = NULL;
    uint8_t *result = NULL;
    if (!decoder || !encoder || !source || !canvas || !scaled || !packet ||
        avcodec_parameters_to_context(decoder, stream->codecpar) < 0)
        goto out;
    decoder->thread_count = 1;
    decoder->max_pixels = 16 * 1024 * 1024;
    if (avcodec_open2(decoder, decoder_codec, NULL) < 0 ||
        avcodec_send_packet(decoder, &stream->attached_pic) < 0 ||
        avcodec_receive_frame(decoder, source) < 0 || source->width <= 0 || source->height <= 0)
        goto out;
    double aspect = (double) source->width / source->height;
    if (source->sample_aspect_ratio.num > 0 && source->sample_aspect_ratio.den > 0)
        aspect *= av_q2d(source->sample_aspect_ratio);
    const int width = aspect >= 1.0 ? 512 : FFMAX(1, (int) lrint(512 * aspect));
    const int height = aspect <= 1.0 ? 512 : FFMAX(1, (int) lrint(512 / aspect));
    canvas->format = AV_PIX_FMT_RGBA;
    canvas->width = canvas->height = 512;
    if (av_frame_get_buffer(canvas, 0) < 0)
        goto out;
    memset(canvas->data[0], 0, (size_t) canvas->linesize[0] * 512);
    scaled->format = AV_PIX_FMT_RGBA;
    scaled->width = width;
    scaled->height = height;
    if (av_frame_get_buffer(scaled, 0) < 0)
        goto out;
    enum AVPixelFormat pixel_format = source->format;
    // YUVJ formats are deprecated aliases for full-range YUV. Normalize the
    // format while retaining the range explicitly for the scaler.
    switch (pixel_format) {
    case AV_PIX_FMT_YUVJ420P: pixel_format = AV_PIX_FMT_YUV420P; break;
    case AV_PIX_FMT_YUVJ422P: pixel_format = AV_PIX_FMT_YUV422P; break;
    case AV_PIX_FMT_YUVJ444P: pixel_format = AV_PIX_FMT_YUV444P; break;
    case AV_PIX_FMT_YUVJ440P: pixel_format = AV_PIX_FMT_YUV440P; break;
    case AV_PIX_FMT_YUVJ411P: pixel_format = AV_PIX_FMT_YUV411P; break;
    default: break;
    }
    const int full_range = source->color_range == AVCOL_RANGE_JPEG ||
                           pixel_format != source->format;
    scale = sws_getContext(source->width, source->height, pixel_format,
                          width, height, AV_PIX_FMT_RGBA, SWS_BILINEAR, NULL, NULL, NULL);
    if (!scale)
        goto out;
    const int *coefficients = sws_getCoefficients(source->colorspace);
    if (sws_setColorspaceDetails(scale, coefficients, full_range,
                                 coefficients, 1, 0, 1 << 16, 1 << 16) < 0)
        goto out;
    if (sws_scale(scale, (const uint8_t *const *) source->data, source->linesize,
                  0, source->height, scaled->data, scaled->linesize) != height)
        goto out;
    // Keep scaler row padding separate from the visible transparent canvas.
    for (int row = 0; row < height; row++)
        memcpy(canvas->data[0] + (row + (512 - height) / 2) * canvas->linesize[0] +
               ((512 - width) / 2) * 4,
               scaled->data[0] + row * scaled->linesize[0], (size_t) width * 4);
    encoder->width = encoder->height = 512;
    encoder->pix_fmt = AV_PIX_FMT_RGBA;
    encoder->time_base = (AVRational) {1, 1};
    encoder->thread_count = 1;
    if (avcodec_open2(encoder, encoder_codec, NULL) < 0 ||
        avcodec_send_frame(encoder, canvas) < 0 ||
        avcodec_receive_packet(encoder, packet) < 0 || packet->size <= 0)
        goto out;
    result = malloc((size_t) packet->size);
    if (result) {
        memcpy(result, packet->data, (size_t) packet->size);
        *png_size = (size_t) packet->size;
    }
out:
    sws_freeContext(scale);
    av_packet_free(&packet);
    av_frame_free(&canvas);
    av_frame_free(&source);
    av_frame_free(&scaled);
    avcodec_free_context(&encoder);
    avcodec_free_context(&decoder);
    return result;
}

void up_av_artwork_free(uint8_t *data)
{
    free(data);
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

int up_av_error_is_again(int code)
{
    return code == AVERROR(EAGAIN);
}

int up_av_error_is_eof(int code)
{
    return code == AVERROR_EOF;
}

int up_av_format_open(UpAvFormat **format, const char *path)
{
    AVFormatContext *native = NULL;
    /* MP4 cover art has no media timescale, but the MOV demuxer warns when
     * assigning its fallback. Use the same startup log policy as stream
     * discovery below; keep errors visible and restore playback logging. */
    int log_level = av_log_get_level();
    if (log_level > AV_LOG_ERROR)
        av_log_set_level(AV_LOG_ERROR);
    int result = avformat_open_input(&native, path, NULL, NULL);
    av_log_set_level(log_level);
    *format = (UpAvFormat *) native;
    return result;
}

int up_av_format_find_stream_info(UpAvFormat *format)
{
    /* Some containers cannot provide bitmap-subtitle dimensions until the
     * first subtitle packet. FFmpeg reports that expected condition as a
     * warning even though discovery succeeds and decoding later supplies the
     * dimensions. Keep startup output useful while preserving actual errors. */
    int log_level = av_log_get_level();
    if (log_level > AV_LOG_ERROR)
        av_log_set_level(AV_LOG_ERROR);
    int result = avformat_find_stream_info(FORMAT(format), NULL);
    av_log_set_level(log_level);
    return result;
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

const char *up_av_stream_metadata(const UpAvFormat *format,
                                  unsigned int stream_index,
                                  const char *key)
{
    AVStream *stream = stream_at(format, stream_index);
    if (!stream || !key)
        return NULL;
    const AVDictionaryEntry *entry = av_dict_get(stream->metadata, key, NULL, 0);
    return entry ? entry->value : NULL;
}

double up_av_format_duration(const UpAvFormat *format)
{
    const int64_t duration = format ? FORMAT(format)->duration : AV_NOPTS_VALUE;
    return duration != AV_NOPTS_VALUE && duration > 0
        ? (double) duration / AV_TIME_BASE : NAN;
}

unsigned int up_av_chapter_count(const UpAvFormat *format)
{
    return format ? FORMAT(format)->nb_chapters : 0;
}

const char *up_av_format_metadata(const UpAvFormat *format, const char *key)
{
    const AVDictionaryEntry *entry = format && key
        ? av_dict_get(FORMAT(format)->metadata, key, NULL, 0) : NULL;
    return entry ? entry->value : NULL;
}

double up_av_chapter_start(const UpAvFormat *format, unsigned int index)
{
    if (index >= up_av_chapter_count(format))
        return NAN;
    const AVChapter *chapter = FORMAT(format)->chapters[index];
    if (!chapter || chapter->start == AV_NOPTS_VALUE ||
        chapter->time_base.num <= 0 || chapter->time_base.den <= 0)
        return NAN;
    return (double) chapter->start * av_q2d(chapter->time_base);
}

const char *up_av_chapter_title(const UpAvFormat *format, unsigned int index)
{
    if (index >= up_av_chapter_count(format) || !FORMAT(format)->chapters[index])
        return NULL;
    const AVDictionaryEntry *title = av_dict_get(
        FORMAT(format)->chapters[index]->metadata, "title", NULL, 0);
    return title ? title->value : NULL;
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
    AVRational rate = av_guess_frame_rate(FORMAT(format), stream, NULL);
    decoder->frame_duration = rate.num > 0 && rate.den > 0
        ? (double) rate.den / rate.num : 1.0 / 60.0;
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

double up_av_decoder_frame_duration(const UpAvDecoder *decoder)
{
    return decoder->frame_duration;
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

static int unusable_color_name(const char *name)
{
    return !name || !strcmp(name, "unknown") || !strcmp(name, "reserved");
}

int up_av_video_info(const UpAvFormat *format, const UpAvDecoder *decoder,
                     const UpAvFrame *frame, UpVideoInfo *info)
{
    AVFormatContext *native_format = FORMAT(format);
    AVStream *stream = stream_at(format, (unsigned int) decoder->stream_index);
    AVFrame *native_frame = FRAME(frame);
    if (!native_format || !stream || !info)
        return 0;
    AVCodecParameters *parameters = stream->codecpar;
    AVFrame fallback = {
        .width = parameters->width, .height = parameters->height,
        .colorspace = parameters->color_space,
        .color_primaries = parameters->color_primaries,
        .color_trc = parameters->color_trc,
        .color_range = parameters->color_range,
    };
    if (!native_frame)
        native_frame = &fallback;
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

    const int assume_hd = info->width >= 1280 || info->height > 576;
    const AVPixFmtDescriptor *pixel_description =
        av_pix_fmt_desc_get(pixel_format);
    const int is_rgb = pixel_description &&
        (pixel_description->flags & AV_PIX_FMT_FLAG_RGB);
    enum AVColorSpace space = native_frame->colorspace;
    const char *space_name = av_color_space_name(space);
    if (space == AVCOL_SPC_UNSPECIFIED || unusable_color_name(space_name) ||
        (space == AVCOL_SPC_RGB && !is_rgb)) {
        space = parameters->color_space;
        space_name = av_color_space_name(space);
    }
    if (space == AVCOL_SPC_UNSPECIFIED || unusable_color_name(space_name) ||
        (space == AVCOL_SPC_RGB && !is_rgb)) {
        info->color_space = is_rgb ? "rgb" : assume_hd ? "bt709" : "bt601";
        info->color_space_assumed = 1;
    } else {
        info->color_space = space_name;
    }

    enum AVColorPrimaries primaries = native_frame->color_primaries;
    const char *primaries_name = av_color_primaries_name(primaries);
    if (primaries == AVCOL_PRI_UNSPECIFIED ||
        unusable_color_name(primaries_name)) {
        primaries = parameters->color_primaries;
        primaries_name = av_color_primaries_name(primaries);
    }
    if (primaries == AVCOL_PRI_UNSPECIFIED ||
        unusable_color_name(primaries_name)) {
        if (assume_hd)
            info->color_primaries = "bt709";
        else if (info->height == 576)
            info->color_primaries = "bt601-625";
        else if (info->height == 480 || info->height == 486)
            info->color_primaries = "bt601-525";
        else
            info->color_primaries = "bt709";
        info->color_primaries_assumed = 1;
    } else {
        info->color_primaries = primaries_name;
    }

    enum AVColorTransferCharacteristic transfer = native_frame->color_trc;
    const char *transfer_name = av_color_transfer_name(transfer);
    if (transfer == AVCOL_TRC_UNSPECIFIED || unusable_color_name(transfer_name)) {
        transfer = parameters->color_trc;
        transfer_name = av_color_transfer_name(transfer);
    }
    if (transfer == AVCOL_TRC_UNSPECIFIED || unusable_color_name(transfer_name)) {
        /* This is libplacebo's effective default for unknown SDR transfer. */
        info->color_transfer = "bt1886";
        info->color_transfer_assumed = 1;
    } else {
        info->color_transfer = transfer_name;
    }
    if (transfer == AVCOL_TRC_SMPTE2084)
        info->hdr_kind = UP_HDR_KIND_PQ;
    else if (transfer == AVCOL_TRC_ARIB_STD_B67)
        info->hdr_kind = UP_HDR_KIND_HLG;
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

double up_av_frame_audio_duration(const UpAvFrame *frame)
{
    const AVFrame *native = FRAME(frame);
    return native->sample_rate > 0
        ? (double) native->nb_samples / native->sample_rate : 0.0;
}

int up_av_audio_converter_matches(const UpAvAudioConverter *converter,
                                   const UpAvFrame *frame)
{
    const AVFrame *native = FRAME(frame);
    return converter->input_rate == native->sample_rate &&
        converter->input_format == native->format &&
        av_channel_layout_compare(&converter->input_layout,
                                   &native->ch_layout) == 0;
}

int up_av_audio_converter_drain_capacity(UpAvAudioConverter *converter)
{
    return swr_get_out_samples(converter->context, 0);
}

int up_av_audio_converter_drain(UpAvAudioConverter *converter, float *output,
                                 int output_frames)
{
    uint8_t *planes[] = { (uint8_t *) output };
    return swr_convert(converter->context, planes, output_frames, NULL, 0);
}

/* Reject changed input until the caller drains and replaces the converter.
 * swr_convert assumes its input planes match the configured layout. */
static int prepare_audio_converter(UpAvAudioConverter *converter,
                                    const AVFrame *frame)
{
    if (converter->context)
        return up_av_audio_converter_matches(converter, (const UpAvFrame *) frame)
            ? 0 : AVERROR_INPUT_CHANGED;

    SwrContext *context = NULL;
    AVChannelLayout layout = {0};
    int result = av_channel_layout_copy(&layout, &frame->ch_layout);
    if (result >= 0)
        result = swr_alloc_set_opts2(&context, &converter->output_layout,
                                     AV_SAMPLE_FMT_FLT, converter->output_rate,
                                     &frame->ch_layout, frame->format,
                                     frame->sample_rate, 0, NULL);
    if (result >= 0)
        result = swr_init(context);
    if (result < 0) {
        swr_free(&context);
        av_channel_layout_uninit(&layout);
        return result;
    }
    swr_free(&converter->context);
    av_channel_layout_uninit(&converter->input_layout);
    converter->context = context;
    converter->input_layout = layout;
    converter->input_format = frame->format;
    converter->input_rate = frame->sample_rate;
    return 0;
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
    converter->output_rate = output_rate;
    int result = prepare_audio_converter(converter, FRAME(frame));
    if (error)
        *error = result;
    if (result < 0) {
        up_av_audio_converter_free(&converter);
        return NULL;
    }
    return converter;
}

void up_av_audio_converter_free(UpAvAudioConverter **converter)
{
    if (!converter || !*converter)
        return;
    swr_free(&(*converter)->context);
    av_channel_layout_uninit(&(*converter)->output_layout);
    av_channel_layout_uninit(&(*converter)->input_layout);
    free(*converter);
    *converter = NULL;
}

int up_av_audio_converter_capacity(UpAvAudioConverter *converter,
                                   const UpAvFrame *frame)
{
    int result = prepare_audio_converter(converter, FRAME(frame));
    if (result < 0)
        return result;
    return swr_get_out_samples(converter->context, FRAME(frame)->nb_samples);
}

int up_av_audio_converter_convert(UpAvAudioConverter *converter,
                                  const UpAvFrame *frame, float *output,
                                  int output_frames)
{
    uint8_t *output_planes[] = { (uint8_t *) output };
    AVFrame *native = FRAME(frame);
    int result = prepare_audio_converter(converter, native);
    if (result < 0)
        return result;
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
