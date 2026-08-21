#pragma once

#include <stddef.h>
#include <stdint.h>

typedef struct UpAvFormat UpAvFormat;
typedef struct UpAvDecoder UpAvDecoder;
typedef struct UpAvPacket UpAvPacket;
typedef struct UpAvFrame UpAvFrame;
typedef struct UpAvAudioConverter UpAvAudioConverter;
typedef struct UpAvSubtitle UpAvSubtitle;

enum UpMediaType {
    UP_MEDIA_TYPE_VIDEO = 0,
    UP_MEDIA_TYPE_AUDIO,
    UP_MEDIA_TYPE_SUBTITLE,
};

enum UpHdrKind {
    UP_HDR_KIND_SDR = 0,
    UP_HDR_KIND_PQ,
    UP_HDR_KIND_HLG,
    UP_HDR_KIND_UNKNOWN,
};

enum UpSubtitleRectType {
    UP_SUBTITLE_RECT_OTHER = 0,
    UP_SUBTITLE_RECT_BITMAP,
    UP_SUBTITLE_RECT_TEXT,
    UP_SUBTITLE_RECT_ASS,
};

typedef struct UpVideoInfo {
    const char *codec;
    const char *profile;
    const char *pixel_format;
    const char *color_space;
    const char *color_primaries;
    const char *color_transfer;
    const char *color_range;
    int width;
    int height;
    int64_t declared_bitrate;
    int64_t metadata_bitrate;
    int64_t container_bitrate;
    double frame_rate;
    enum UpHdrKind hdr_kind;
    int color_space_assumed;
    int color_primaries_assumed;
    int color_transfer_assumed;
    int color_range_assumed;
} UpVideoInfo;

typedef struct UpSubtitleInfo {
    int64_t pts;
    uint32_t start_display_time;
    uint32_t end_display_time;
    unsigned int rect_count;
} UpSubtitleInfo;

typedef struct UpSubtitleRectView {
    enum UpSubtitleRectType type;
    int x;
    int y;
    int width;
    int height;
    int line_size;
    int color_count;
    const uint8_t *pixels;
    const uint8_t *palette;
    const char *text;
} UpSubtitleRectView;

int up_av_error_string(int code, char *buffer, size_t buffer_size);
int up_av_error_is_again(int code);
int up_av_error_is_eof(int code);

int up_av_format_open(UpAvFormat **format, const char *path);
int up_av_format_find_stream_info(UpAvFormat *format);
void up_av_format_close(UpAvFormat **format);
int up_av_find_best_stream(UpAvFormat *format, enum UpMediaType type,
                           int related_stream);
unsigned int up_av_stream_count(const UpAvFormat *format);
enum UpMediaType up_av_stream_type(const UpAvFormat *format,
                                   unsigned int stream_index);
int up_av_stream_is_default(const UpAvFormat *format,
                            unsigned int stream_index);
const char *up_av_stream_codec_name(const UpAvFormat *format,
                                    unsigned int stream_index);
const char *up_av_stream_metadata(const UpAvFormat *format,
                                  unsigned int stream_index,
                                  const char *key);
double up_av_format_duration(const UpAvFormat *format);
int up_av_read_frame(UpAvFormat *format, UpAvPacket *packet);
int up_av_seek(UpAvFormat *format, int stream_index, double target_seconds);
UpAvDecoder *up_av_decoder_open(UpAvFormat *format, int stream_index,
                                void *vulkan_device, int prefer_vulkan);
const char *up_av_decoder_error(void);
void up_av_decoder_free(UpAvDecoder **decoder);
int up_av_decoder_stream_index(const UpAvDecoder *decoder);
double up_av_decoder_time_base(const UpAvDecoder *decoder);
int up_av_decoder_uses_vulkan(const UpAvDecoder *decoder);
int up_av_decoder_width(const UpAvDecoder *decoder);
int up_av_decoder_height(const UpAvDecoder *decoder);
int up_av_decoder_send_packet(UpAvDecoder *decoder,
                              const UpAvPacket *packet);
int up_av_decoder_receive_frame(UpAvDecoder *decoder, UpAvFrame **frame);
void up_av_decoder_flush(UpAvDecoder *decoder);

UpAvPacket *up_av_packet_alloc(void);
void up_av_packet_free(UpAvPacket **packet);
void up_av_packet_unref(UpAvPacket *packet);
int up_av_packet_stream_index(const UpAvPacket *packet);
int64_t up_av_packet_pts(const UpAvPacket *packet);
int64_t up_av_packet_duration(const UpAvPacket *packet);

void up_av_frame_free(UpAvFrame **frame);
int up_av_frame_is_vulkan(const UpAvFrame *frame);
int64_t up_av_frame_timestamp(const UpAvFrame *frame);
int64_t up_av_frame_duration(const UpAvFrame *frame);
int up_av_video_info(const UpAvFormat *format, const UpAvDecoder *decoder,
                     const UpAvFrame *frame, UpVideoInfo *info);

UpAvAudioConverter *up_av_audio_converter_create(const UpAvFrame *frame,
                                                  int output_rate,
                                                  int output_channels,
                                                  int *error);
void up_av_audio_converter_free(UpAvAudioConverter **converter);
int up_av_audio_converter_capacity(UpAvAudioConverter *converter,
                                   const UpAvFrame *frame);
int up_av_audio_converter_convert(UpAvAudioConverter *converter,
                                  const UpAvFrame *frame, float *output,
                                  int output_frames);

UpAvSubtitle *up_av_decode_subtitle(UpAvDecoder *decoder,
                                    const UpAvPacket *packet, int *result);
void up_av_subtitle_free(UpAvSubtitle **subtitle);
void up_av_subtitle_info(const UpAvSubtitle *subtitle, UpSubtitleInfo *info);
int up_av_subtitle_rect(const UpAvSubtitle *subtitle, unsigned int index,
                        UpSubtitleRectView *view);
