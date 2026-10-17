#pragma once
#include <SDL3/SDL.h>
#include <libavutil/frame.h>

typedef struct UpVideoUpscaler UpVideoUpscaler;
UpVideoUpscaler *up_video_upscaler_create(SDL_WindowID window);
// Borrowed until the next call. Original frames are never modified. Unsupported
// formats, HDR, 4K and interlaced pictures bypass processing.
const AVFrame *up_video_upscaler_frame(UpVideoUpscaler *upscaler, const AVFrame *frame,
                                     int field, bool enabled, bool larger);
const char *up_video_upscaler_status(const UpVideoUpscaler *upscaler);
void up_video_upscaler_destroy(UpVideoUpscaler *upscaler);
