#include "wayland_input.h"

#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define BTN_LEFT 0x110
#define DOUBLE_CLICK_MS 400
#define DOUBLE_CLICK_DISTANCE 8.0
#define RESIZE_BORDER 10.0
#define TOP_BAR_HEIGHT_PIXELS 42.0
#define SCRUBBER_HIT_HEIGHT_PIXELS 42.0

struct UpWaylandInput {
    SDL_Window *window;
    struct wl_display *display;
    struct wl_registry *registry;
    struct wl_seat *seat;
    struct wl_pointer *pointer;
    struct wl_surface *surface;
    struct xdg_toplevel *toplevel;
    double x;
    double y;
    uint32_t last_click_time;
    double last_click_x;
    double last_click_y;
    bool focused;
    bool ready;
    char error[256];
};

static bool is_resize_edge(UpWaylandInput *input)
{
    int width = 0, height = 0;
    SDL_GetWindowSize(input->window, &width, &height);
    return input->x <= RESIZE_BORDER || input->x >= width - RESIZE_BORDER ||
           input->y <= RESIZE_BORDER || input->y >= height - RESIZE_BORDER;
}

static bool is_close_button(UpWaylandInput *input)
{
    int width = 0, height = 0;
    int pixel_width = 0, pixel_height = 0;
    if (!SDL_GetWindowSize(input->window, &width, &height) ||
        !SDL_GetWindowSizeInPixels(input->window, &pixel_width, &pixel_height) ||
        width <= 0 || height <= 0 || pixel_width <= 0 || pixel_height <= 0)
        return false;
    const double button_width = TOP_BAR_HEIGHT_PIXELS * width / pixel_width;
    const double button_height = TOP_BAR_HEIGHT_PIXELS * height / pixel_height;
    return input->x >= width - button_width && input->x < width &&
           input->y >= 0.0 && input->y < button_height;
}

static bool is_scrubber(UpWaylandInput *input)
{
    int width = 0, height = 0;
    int pixel_width = 0, pixel_height = 0;
    if (!SDL_GetWindowSize(input->window, &width, &height) ||
        !SDL_GetWindowSizeInPixels(input->window, &pixel_width, &pixel_height) ||
        height <= 0 || pixel_height <= 0)
        return false;
    const double hit_height = SCRUBBER_HIT_HEIGHT_PIXELS * height / pixel_height;
    return input->y >= height - hit_height;
}

static void pointer_enter(void *data, struct wl_pointer *pointer,
                          uint32_t serial, struct wl_surface *surface,
                          wl_fixed_t x, wl_fixed_t y)
{
    (void) pointer;
    (void) serial;
    UpWaylandInput *input = data;
    input->focused = surface == input->surface;
    input->x = wl_fixed_to_double(x);
    input->y = wl_fixed_to_double(y);
}

static void pointer_leave(void *data, struct wl_pointer *pointer,
                          uint32_t serial, struct wl_surface *surface)
{
    (void) pointer;
    (void) serial;
    UpWaylandInput *input = data;
    if (surface == input->surface)
        input->focused = false;
}

static void pointer_motion(void *data, struct wl_pointer *pointer,
                           uint32_t time, wl_fixed_t x, wl_fixed_t y)
{
    (void) pointer;
    (void) time;
    UpWaylandInput *input = data;
    input->x = wl_fixed_to_double(x);
    input->y = wl_fixed_to_double(y);
}

static void pointer_button(void *data, struct wl_pointer *pointer,
                           uint32_t serial, uint32_t time, uint32_t button,
                           uint32_t state)
{
    (void) pointer;
    UpWaylandInput *input = data;
    if (!input->focused || button != BTN_LEFT ||
        state != WL_POINTER_BUTTON_STATE_PRESSED)
        return;

    // Leave the close-button click to SDL instead of starting a window move.
    if (is_close_button(input)) {
        input->last_click_time = 0;
        return;
    }

    // SDL's edge-only hit test consumes this click and sends the corresponding
    // xdg_toplevel.resize request. Do not race it with a move request.
    if (is_resize_edge(input)) {
        input->last_click_time = 0;
        return;
    }

    // Leave timeline clicks and drags to SDL instead of asking the compositor
    // to move the window.
    if (is_scrubber(input)) {
        input->last_click_time = 0;
        return;
    }

    const uint32_t elapsed = time - input->last_click_time;
    const double dx = input->x - input->last_click_x;
    const double dy = input->y - input->last_click_y;
    const bool double_click = input->last_click_time &&
                              elapsed <= DOUBLE_CLICK_MS &&
                              hypot(dx, dy) <= DOUBLE_CLICK_DISTANCE;
    input->last_click_time = double_click ? 0 : time;
    input->last_click_x = input->x;
    input->last_click_y = input->y;

    if (!double_click)
        xdg_toplevel_move(input->toplevel, input->seat, serial);
}

static void pointer_axis(void *data, struct wl_pointer *pointer, uint32_t time,
                         uint32_t axis, wl_fixed_t value)
{
    (void) data; (void) pointer; (void) time; (void) axis; (void) value;
}

static void pointer_frame(void *data, struct wl_pointer *pointer)
{
    (void) data; (void) pointer;
}

static void pointer_axis_source(void *data, struct wl_pointer *pointer,
                                uint32_t source)
{
    (void) data; (void) pointer; (void) source;
}

static void pointer_axis_stop(void *data, struct wl_pointer *pointer,
                              uint32_t time, uint32_t axis)
{
    (void) data; (void) pointer; (void) time; (void) axis;
}

static void pointer_axis_discrete(void *data, struct wl_pointer *pointer,
                                  uint32_t axis, int32_t discrete)
{
    (void) data; (void) pointer; (void) axis; (void) discrete;
}

static void pointer_axis_value120(void *data, struct wl_pointer *pointer,
                                  uint32_t axis, int32_t value120)
{
    (void) data; (void) pointer; (void) axis; (void) value120;
}

static void pointer_axis_relative_direction(void *data,
                                             struct wl_pointer *pointer,
                                             uint32_t axis,
                                             uint32_t direction)
{
    (void) data; (void) pointer; (void) axis; (void) direction;
}

static const struct wl_pointer_listener pointer_listener = {
    .enter = pointer_enter,
    .leave = pointer_leave,
    .motion = pointer_motion,
    .button = pointer_button,
    .axis = pointer_axis,
    .frame = pointer_frame,
    .axis_source = pointer_axis_source,
    .axis_stop = pointer_axis_stop,
    .axis_discrete = pointer_axis_discrete,
    .axis_value120 = pointer_axis_value120,
    .axis_relative_direction = pointer_axis_relative_direction,
};

static void seat_capabilities(void *data, struct wl_seat *seat,
                              uint32_t capabilities)
{
    UpWaylandInput *input = data;
    if ((capabilities & WL_SEAT_CAPABILITY_POINTER) && !input->pointer) {
        input->pointer = wl_seat_get_pointer(seat);
        wl_pointer_add_listener(input->pointer, &pointer_listener, input);
    } else if (!(capabilities & WL_SEAT_CAPABILITY_POINTER) && input->pointer) {
        wl_pointer_destroy(input->pointer);
        input->pointer = NULL;
    }
}

static void seat_name(void *data, struct wl_seat *seat, const char *name)
{
    (void) data; (void) seat; (void) name;
}

static const struct wl_seat_listener seat_listener = {
    .capabilities = seat_capabilities,
    .name = seat_name,
};

static void registry_global(void *data, struct wl_registry *registry,
                            uint32_t name, const char *interface,
                            uint32_t version)
{
    UpWaylandInput *input = data;
    if (!input->seat && !strcmp(interface, wl_seat_interface.name)) {
        const uint32_t supported = version < 9 ? version : 9;
        input->seat = wl_registry_bind(registry, name, &wl_seat_interface,
                                       supported);
        wl_seat_add_listener(input->seat, &seat_listener, input);
    }
}

static void registry_remove(void *data, struct wl_registry *registry,
                            uint32_t name)
{
    (void) data; (void) registry; (void) name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_remove,
};

UpWaylandInput *up_wayland_input_create(SDL_Window *window)
{
    UpWaylandInput *input = calloc(1, sizeof(*input));
    if (!input)
        return NULL;
    input->window = window;
    SDL_PropertiesID properties = SDL_GetWindowProperties(window);
    input->display = SDL_GetPointerProperty(
        properties, SDL_PROP_WINDOW_WAYLAND_DISPLAY_POINTER, NULL);
    input->surface = SDL_GetPointerProperty(
        properties, SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER, NULL);
    input->toplevel = SDL_GetPointerProperty(
        properties, SDL_PROP_WINDOW_WAYLAND_XDG_TOPLEVEL_POINTER, NULL);
    if (!input->display || !input->surface || !input->toplevel) {
        snprintf(input->error, sizeof(input->error),
                 "SDL did not expose the required Wayland window objects");
        return input;
    }

    input->registry = wl_display_get_registry(input->display);
    wl_registry_add_listener(input->registry, &registry_listener, input);
    if (wl_display_roundtrip(input->display) < 0 ||
        wl_display_roundtrip(input->display) < 0 || !input->seat ||
        !input->pointer) {
        snprintf(input->error, sizeof(input->error),
                 "could not initialize the Wayland pointer drag bridge");
        return input;
    }
    input->ready = true;
    return input;
}

bool up_wayland_input_ready(const UpWaylandInput *input)
{
    return input && input->ready;
}

const char *up_wayland_input_error(const UpWaylandInput *input)
{
    if (!input || !input->error[0])
        return "unknown Wayland input error";
    return input->error;
}

void up_wayland_input_destroy(UpWaylandInput *input)
{
    if (!input)
        return;
    if (input->pointer)
        wl_pointer_destroy(input->pointer);
    if (input->seat)
        wl_seat_destroy(input->seat);
    if (input->registry)
        wl_registry_destroy(input->registry);
    free(input);
}
