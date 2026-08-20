#pragma once

#include <stdbool.h>

typedef struct UpWaylandInput UpWaylandInput;

UpWaylandInput *up_wayland_input_create(void *window);
bool up_wayland_input_ready(const UpWaylandInput *input);
const char *up_wayland_input_error(const UpWaylandInput *input);
void up_wayland_input_destroy(UpWaylandInput *input);
