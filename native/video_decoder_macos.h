#pragma once
#include <libavcodec/avcodec.h>

typedef struct UpAsyncDecoder UpAsyncDecoder;
// Returns NULL when the stream is outside this path or native setup fails.
UpAsyncDecoder *up_async_decoder_create(const AVCodecParameters *parameters, AVRational time_base);
int up_async_decoder_send(UpAsyncDecoder *decoder, const AVPacket *packet);
int up_async_decoder_receive(UpAsyncDecoder *decoder, AVFrame **frame);
void up_async_decoder_flush(UpAsyncDecoder *decoder);
void up_async_decoder_destroy(UpAsyncDecoder *decoder);
