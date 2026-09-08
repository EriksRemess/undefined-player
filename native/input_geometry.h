#pragma once
#include <stdint.h>

// Implemented in Rust. SDL and Wayland use the same hit regions as playback.
enum UpHitRegion {
    UP_HIT_CONTENT = 0,
    UP_HIT_CLOSE,
    UP_HIT_SCRUBBER,
    UP_HIT_TOP_LEFT,
    UP_HIT_TOP_RIGHT,
    UP_HIT_BOTTOM_LEFT,
    UP_HIT_BOTTOM_RIGHT,
    UP_HIT_TOP,
    UP_HIT_BOTTOM,
    UP_HIT_LEFT,
    UP_HIT_RIGHT,
    UP_HIT_OUTSIDE,
};

uint32_t up_input_hit_test(double x, double y, int width, int height,
                           int pixel_width, int pixel_height);
