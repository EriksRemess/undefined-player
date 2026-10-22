#pragma once
#include <SDL3/SDL.h>
#include <libavutil/frame.h>

typedef struct UpVideoUpscaler UpVideoUpscaler;
UpVideoUpscaler *up_video_upscaler_create(SDL_WindowID window);
// Borrowed until the next call. Original frames are never modified. Unsupported
// formats, HDR and 4K bypass processing. Enlarged output is progressive: the
// selected field is reconstructed before ML, so it must not be filtered again.
const AVFrame *up_video_upscaler_frame(UpVideoUpscaler *upscaler, const AVFrame *frame,
                                     int field, bool enabled, bool larger);
const char *up_video_upscaler_status(const UpVideoUpscaler *upscaler);
void up_video_upscaler_destroy(UpVideoUpscaler *upscaler);
