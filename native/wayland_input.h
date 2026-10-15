#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct UpWaylandInput UpWaylandInput;

UpWaylandInput *up_wayland_input_create(void *window);
bool up_wayland_input_ready(const UpWaylandInput *input);
bool up_wayland_input_set_region(UpWaylandInput *input, const int32_t *rectangles,
                                 size_t count, float radius);
const char *up_wayland_input_error(const UpWaylandInput *input);
void up_wayland_input_destroy(UpWaylandInput *input);
