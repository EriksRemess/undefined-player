#pragma once

#include <SDL3/SDL.h>
#include <libavutil/frame.h>

typedef struct UpMetalColor UpMetalColor;

// NV12/P010 decoder surfaces are sampled directly by Metal. Software frames
// and unusual hardware surface formats use a ten-bit upload/conversion path.
UpMetalColor *up_metal_color_create(SDL_Renderer *renderer, char *error, size_t size);
SDL_Texture *up_metal_color_convert(UpMetalColor *color, SDL_Renderer *renderer,
                                     const AVFrame *frame, int field, float peak_nits,
                                     int width, int height,
                                     char *error, size_t size);
void up_metal_color_destroy(UpMetalColor *color);
// Retain directly sampled decoder surfaces until SDL's GPU pass finishes.
bool up_metal_color_can_draw(const AVFrame *frame);
bool up_metal_color_draw(UpMetalColor *color, SDL_Renderer *renderer,
                        const AVFrame *frame, int field, float peak_nits,
                        SDL_FRect source, SDL_FRect destination, uint32_t rotation,
                        int width, int height, char *error, size_t size);
void up_metal_color_after_present(UpMetalColor *color);
