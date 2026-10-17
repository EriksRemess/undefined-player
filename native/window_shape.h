#pragma once

#include <SDL3/SDL.h>
#include <math.h>

// Alpha is shared by SDL's Metal presentation mask and native mouse shape.
// Only the corner squares need per-pixel work; the interior stays opaque.
static inline SDL_Surface *up_corner_shape(int width, int height, float radius)
{
    SDL_Surface *shape = SDL_CreateSurface(width, height, SDL_PIXELFORMAT_RGBA32);
    if (!shape)
        return NULL;
    if (!SDL_FillSurfaceRect(shape, NULL, 0xffffffff)) {
        SDL_DestroySurface(shape);
        return NULL;
    }
    radius = fminf(radius, fminf(width, height) * 0.5f);
    const int extent = (int) ceilf(radius);
    for (int y = 0; y < extent; y++) {
        for (int x = 0; x < extent; x++) {
            const float dx = radius - (x + 0.5f), dy = radius - (y + 0.5f);
            const float distance = hypotf(fmaxf(dx, 0), fmaxf(dy, 0)) +
                fminf(fmaxf(dx, dy), 0) - radius;
            const uint8_t alpha = (uint8_t) lroundf(255 * SDL_clamp(0.5f - distance, 0, 1));
            uint8_t *pixels = shape->pixels;
            pixels[y * shape->pitch + x * 4 + 3] = alpha;
            pixels[y * shape->pitch + (width - 1 - x) * 4 + 3] = alpha;
            pixels[(height - 1 - y) * shape->pitch + x * 4 + 3] = alpha;
            pixels[(height - 1 - y) * shape->pitch + (width - 1 - x) * 4 + 3] = alpha;
        }
    }
    return shape;
}
