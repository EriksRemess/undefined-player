#pragma once

#include <SDL3/SDL.h>
#include <libavutil/buffer.h>
#include <libavutil/frame.h>

typedef struct UpVideoRenderer UpVideoRenderer;

UpVideoRenderer *up_video_renderer_create(SDL_Window *window);
AVBufferRef *up_video_renderer_device(UpVideoRenderer *renderer);
int up_video_renderer_display(UpVideoRenderer *renderer, AVFrame *frame,
                              int width, int height, float top_bar_alpha,
                              const char *title, const char *info,
                              float info_alpha, const char *position,
                              float position_alpha, float scrubber_progress,
                              float scrubber_alpha, const char *subtitle_text,
                              const uint8_t *subtitle_pixels,
                              int subtitle_width, int subtitle_height,
                              uint64_t subtitle_serial);
int up_video_renderer_resize(UpVideoRenderer *renderer, int width, int height);
const char *up_video_renderer_error(const UpVideoRenderer *renderer);
void up_video_renderer_destroy(UpVideoRenderer *renderer);
