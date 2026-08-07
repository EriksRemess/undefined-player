#pragma once

#include <SDL3/SDL.h>

typedef struct UpWaylandInput UpWaylandInput;

UpWaylandInput *up_wayland_input_create(SDL_Window *window);
bool up_wayland_input_ready(const UpWaylandInput *input);
const char *up_wayland_input_error(const UpWaylandInput *input);
void up_wayland_input_destroy(UpWaylandInput *input);
