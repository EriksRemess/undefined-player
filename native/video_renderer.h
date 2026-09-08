#pragma once

#include <stdint.h>
#include <stddef.h>

typedef struct UpVideoRenderer UpVideoRenderer;

// Rust chooses when small sources use integer scaling; zero means normal fit.
uint32_t up_video_integer_scale(double source_width, double source_height,
                                int sar_num, int sar_den, uint32_t rotation,
                                int width, int height);

// Borrowed A8 images and player-owned geometry, valid for one display call.
typedef struct UpOverlayImage {
    const uint8_t *pixels;
    int width, height;
    uint64_t serial;
} UpOverlayImage;

typedef struct UpOverlayPart {
    unsigned int texture; // 0: solid white, 1: controls, 2: title
    float src[4], dst[4], color[4];
} UpOverlayPart;

typedef struct UpOverlayFrame {
    UpOverlayImage text, title;
    const UpOverlayPart *parts;
    size_t count;
} UpOverlayFrame;

UpVideoRenderer *up_video_renderer_create(void *window);
void *up_video_renderer_device(UpVideoRenderer *renderer);
int up_video_renderer_display(UpVideoRenderer *renderer, void *frame,
                              int width, int height, const UpOverlayFrame *overlay,
                              const char *subtitle_text,
                              const uint8_t *subtitle_pixels,
                              int subtitle_width, int subtitle_height,
                              uint64_t subtitle_serial);
int up_video_renderer_resize(UpVideoRenderer *renderer, int width, int height);
const char *up_video_renderer_error(const UpVideoRenderer *renderer);
void up_video_renderer_destroy(UpVideoRenderer *renderer);
