#include "video_decoder_macos.m"
#include <libavformat/avformat.h>
#include <assert.h>
#include <stdio.h>

static void bypass_policy(void)
{
    AVCodecParameters *p = avcodec_parameters_alloc();
    assert(p);
    p->codec_id = AV_CODEC_ID_AV1;
    p->format = AV_PIX_FMT_YUV420P;
    p->width = 7680; p->height = 4320;
    assert(!up_async_decoder_create(p, (AVRational){1, 30000}));
    p->extradata = av_mallocz(4 + AV_INPUT_BUFFER_PADDING_SIZE);
    p->extradata_size = 4; p->extradata[0] = 0x81;
    p->width = 3840; p->height = 2160;
    assert(!up_async_decoder_create(p, (AVRational){1, 30000}));
    p->width = 7680; p->height = 4320;
    p->color_trc = AVCOL_TRC_SMPTE2084;
    assert(!up_async_decoder_create(p, (AVRational){1, 30000}));
    p->color_trc = AVCOL_TRC_BT709; p->format = AV_PIX_FMT_YUV420P10LE;
    assert(!up_async_decoder_create(p, (AVRational){1, 30000}));
    avcodec_parameters_free(&p);
}

static void ordered_callbacks(void)
{
    UpAsyncDecoder *decoder = calloc(1, sizeof(*decoder));
    pthread_mutex_init(&decoder->mutex, NULL);
    pthread_cond_init(&decoder->changed, NULL);
    struct DecodeJob *first = calloc(1, sizeof(*first)), *second = calloc(1, sizeof(*second));
    first->frame = av_frame_alloc(); second->frame = av_frame_alloc();
    first->next = second; decoder->head = first; decoder->tail = second; decoder->count = 2;
    // A later completed packet must not overtake the head. Dropped outputs
    // still retire their slots; a callback error must reach the caller.
    decoded(decoder, second, -1, 0, NULL, kCMTimeInvalid, kCMTimeInvalid);
    AVFrame *frame = NULL;
    assert(up_async_decoder_receive(decoder, &frame) == AVERROR(EAGAIN));
    decoded(decoder, first, noErr, kVTDecodeInfo_FrameDropped, NULL, kCMTimeInvalid, kCMTimeInvalid);
    assert(up_async_decoder_receive(decoder, &frame) == AVERROR_EXTERNAL);
    assert(!frame && !decoder->count);
    up_async_decoder_destroy(decoder);
}

static void receive_all(UpAsyncDecoder *decoder, int64_t *timestamps, int sent, int *received, AVFrame **held)
{
    AVFrame *frame = NULL;
    int status;
    while ((status = up_async_decoder_receive(decoder, &frame)) == 0) {
        assert(*received < sent);
        assert(frame->pts == timestamps[(*received)++]);
        assert(frame->best_effort_timestamp == frame->pts && frame->duration > 0);
        assert(frame->format == AV_PIX_FMT_VIDEOTOOLBOX && frame->data[3]);
        assert(frame->width == 7680 && frame->height == 4320);
        assert(frame->colorspace == AVCOL_SPC_BT709 && frame->color_range == AVCOL_RANGE_MPEG);
        assert(frame->sample_aspect_ratio.num == 1 && frame->sample_aspect_ratio.den == 1);
        av_frame_free(held); *held = frame;
    }
    assert(status == AVERROR(EAGAIN) || status == AVERROR_EOF);
}

static void live_decode(const char *path)
{
    AVFormatContext *input = NULL;
    assert(avformat_open_input(&input, path, NULL, NULL) >= 0);
    assert(avformat_find_stream_info(input, NULL) >= 0);
    int index = av_find_best_stream(input, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    assert(index >= 0);
    AVStream *stream = input->streams[index];
    UpAsyncDecoder *decoder = up_async_decoder_create(stream->codecpar, stream->time_base);
    assert(decoder);
    AVPacket *packet = av_packet_alloc(); AVFrame *held = NULL;
    double started = NSProcessInfo.processInfo.systemUptime;
    for (int pass = 0; pass < 4; pass++) {
        // Flush with submissions still in flight, then seek both forward and back.
        if (pass) {
            up_async_decoder_flush(decoder);
            int64_t target = pass == 1 ? (input->duration - 2000000) : 0;
            assert(av_seek_frame(input, -1, target, AVSEEK_FLAG_BACKWARD) >= 0);
            assert(!decoder->head && !decoder->error && !decoder->draining);
        }
        int64_t timestamps[400]; int sent = 0, received = 0;
        while (sent < (pass == 0 ? 180 : pass == 1 ? 400 : 3) && av_read_frame(input, packet) >= 0) {
            if (packet->stream_index == index) {
                timestamps[sent++] = packet->pts;
                assert(up_async_decoder_send(decoder, packet) == 0);
                assert(decoder->count <= DECODE_DEPTH);
                if (pass < 2) receive_all(decoder, timestamps, sent, &received, &held);
            }
            av_packet_unref(packet);
        }
        if (pass < 2) {
            assert(up_async_decoder_send(decoder, NULL) == 0);
            receive_all(decoder, timestamps, sent, &received, &held);
            assert(received == sent && received > 0);
            AVFrame *frame = NULL;
            assert(up_async_decoder_receive(decoder, &frame) == AVERROR_EOF);
        }
        if (!pass) printf("Async AV1: %d frames in %.2f s\n", received, NSProcessInfo.processInfo.systemUptime - started);
    }
    up_async_decoder_destroy(decoder); // pending callbacks must finish before free
    CVPixelBufferRef buffer = (CVPixelBufferRef)held->data[3];
    assert(CVPixelBufferLockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess);
    assert(CVPixelBufferGetBaseAddressOfPlane(buffer, 0));
    CVPixelBufferUnlockBaseAddress(buffer, kCVPixelBufferLock_ReadOnly);
    av_frame_free(&held); av_packet_free(&packet); avformat_close_input(&input);
    puts("Async AV1: ordering, timestamps, bounded queue, EOF, seeks and surface lifetime passed");
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        bypass_policy(); ordered_callbacks();
        if (argc == 2) live_decode(argv[1]);
    }
    puts("Async AV1 policy and callback ordering passed");
    return 0;
}
