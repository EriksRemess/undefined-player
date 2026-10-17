#include "platform.h"
#include "input_geometry.h"

#include <SDL3/SDL.h>
#ifdef __APPLE__
#include "window_shape.h"
#include "app_macos.h"
#endif

#include <stdlib.h>

#define WINDOW(value) ((SDL_Window *) (value))
#define AUDIO(value) ((SDL_AudioStream *) (value))

static enum UpKey translate_key(SDL_Keycode key)
{
    switch (key) {
    case SDLK_COMMA:
        return UP_KEY_COMMA;
    case SDLK_PERIOD:
        return UP_KEY_PERIOD;
    case SDLK_Z:
        return UP_KEY_Z;
    case SDLK_D:
        return UP_KEY_D;
    case SDLK_C:
        return UP_KEY_C;
    case SDLK_Q:
        return UP_KEY_Q;
    case SDLK_J:
        return UP_KEY_J;
    case SDLK_LEFT:
        return UP_KEY_LEFT;
    case SDLK_RIGHT:
        return UP_KEY_RIGHT;
    case SDLK_F:
        return UP_KEY_F;
    case SDLK_I:
        return UP_KEY_I;
    case SDLK_SPACE:
        return UP_KEY_SPACE;
    case SDLK_S:
        return UP_KEY_S;
    case SDLK_A:
        return UP_KEY_A;
    case SDLK_M:
        return UP_KEY_M;
    case SDLK_L:
        return UP_KEY_L;
    case SDLK_P: return UP_KEY_P;
    case SDLK_UP: return UP_KEY_UP;
    case SDLK_DOWN: return UP_KEY_DOWN;
    case SDLK_RETURN: return UP_KEY_RETURN;
    case SDLK_ESCAPE: return UP_KEY_ESCAPE;
    case SDLK_BACKSPACE: return UP_KEY_BACKSPACE;
    case SDLK_Y: return UP_KEY_Y;
    case SDLK_N: return UP_KEY_N;
    case SDLK_U: return UP_KEY_U;
    default:
        return UP_KEY_OTHER;
    }
}

static SDL_HitTestResult resize_hit_test(SDL_Window *window,
                                         const SDL_Point *point, void *data)
{
    (void) data;
    int width = 0, height = 0, pixel_width = 0, pixel_height = 0;
    if (!SDL_GetWindowSize(window, &width, &height) ||
        !SDL_GetWindowSizeInPixels(window, &pixel_width, &pixel_height))
        return SDL_HITTEST_NORMAL;
    const uint32_t hit = up_input_hit_test_rounded(point->x, point->y, width, height, pixel_width, pixel_height,
            SDL_GetFloatProperty(SDL_GetWindowProperties(window), UP_CORNER_RADIUS_PROPERTY, 0.0f));
    switch (hit) {
    case UP_HIT_TOP_LEFT: return SDL_HITTEST_RESIZE_TOPLEFT;
    case UP_HIT_TOP_RIGHT: return SDL_HITTEST_RESIZE_TOPRIGHT;
    case UP_HIT_BOTTOM_LEFT: return SDL_HITTEST_RESIZE_BOTTOMLEFT;
    case UP_HIT_BOTTOM_RIGHT: return SDL_HITTEST_RESIZE_BOTTOMRIGHT;
    case UP_HIT_TOP: return SDL_HITTEST_RESIZE_TOP;
    case UP_HIT_BOTTOM: return SDL_HITTEST_RESIZE_BOTTOM;
    case UP_HIT_LEFT: return SDL_HITTEST_RESIZE_LEFT;
    case UP_HIT_RIGHT: return SDL_HITTEST_RESIZE_RIGHT;
    default: return SDL_HITTEST_NORMAL;
    }
}

#ifdef __APPLE__
static struct {
    SDL_WindowID window;
    float mouse_x, mouse_y;
    int window_x, window_y;
    bool moving;
} window_drag;

static void cancel_window_drag(void)
{
    if (window_drag.window) SDL_CaptureMouse(false);
    window_drag.window = 0;
    window_drag.moving = false;
}

static void update_window_drag(const SDL_Event *event)
{
    // Cocoa consumes both clicks and mouse motion in SDL_HITTEST_DRAGGABLE
    // regions. Move the borderless window from ordinary SDL mouse events so
    // double-click fullscreen, hover controls and cursor activity still work.
    if (event->type == SDL_EVENT_MOUSE_BUTTON_DOWN && event->button.button == SDL_BUTTON_LEFT) {
        cancel_window_drag();
        SDL_Window *window = SDL_GetWindowFromID(event->button.windowID);
        int w, h, pw, ph;
        if (!window || event->button.clicks != 1 ||
            (SDL_GetWindowFlags(window) & (SDL_WINDOW_FULLSCREEN | SDL_WINDOW_MAXIMIZED)) ||
            !SDL_GetWindowSize(window, &w, &h) || !SDL_GetWindowSizeInPixels(window, &pw, &ph) ||
            up_input_hit_test_rounded(event->button.x, event->button.y, w, h, pw, ph,
                SDL_GetFloatProperty(SDL_GetWindowProperties(window), UP_CORNER_RADIUS_PROPERTY, 0)) != UP_HIT_CONTENT)
            return;
        if (!SDL_GetWindowPosition(window, &window_drag.window_x, &window_drag.window_y)) return;
        // Anchor to the press event, not the current pointer position: an
        // entire quick drag may already be queued by the time SDL polls it.
        window_drag.mouse_x = window_drag.window_x + event->button.x;
        window_drag.mouse_y = window_drag.window_y + event->button.y;
        window_drag.window = event->button.windowID;
        SDL_CaptureMouse(true);
    } else if (event->type == SDL_EVENT_MOUSE_MOTION && event->motion.windowID == window_drag.window) {
        SDL_Window *window = SDL_GetWindowFromID(window_drag.window);
        float x, y;
        SDL_GetGlobalMouseState(&x, &y);
        if (!window || !(event->motion.state & SDL_BUTTON_LMASK) ||
            (SDL_GetWindowFlags(window) & (SDL_WINDOW_FULLSCREEN | SDL_WINDOW_MAXIMIZED))) {
            cancel_window_drag();
            return;
        }
        const float dx = x - window_drag.mouse_x, dy = y - window_drag.mouse_y;
        window_drag.moving |= hypotf(dx, dy) >= 4;
        if (window_drag.moving)
            SDL_SetWindowPosition(window, window_drag.window_x + (int) lroundf(dx),
                                  window_drag.window_y + (int) lroundf(dy));
    } else if ((event->type == SDL_EVENT_MOUSE_BUTTON_UP && event->button.button == SDL_BUTTON_LEFT) ||
               (event->type == SDL_EVENT_WINDOW_FOCUS_LOST && event->window.windowID == window_drag.window) ||
               event->type == SDL_EVENT_QUIT) {
        cancel_window_drag();
    }
}
#endif

int up_platform_init(void)
{
#ifdef __APPLE__
    // Cocoa caches this during SDL's video initialization. Native fullscreen
    // Spaces scale a snapshot of the window, enlarging pixel-font controls
    // temporarily. Desktop fullscreen keeps their physical size stable.
    SDL_SetHint(SDL_HINT_VIDEO_MAC_FULLSCREEN_SPACES, "0");
#endif
    const bool initialized = SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO);
#ifdef __APPLE__
    if (initialized) up_mac_app_init();
#endif
    return initialized;
}

void up_platform_quit(void)
{
#ifdef __APPLE__
    cancel_window_drag();
    up_mac_app_quit();
#endif
    SDL_Quit();
}

const char *up_platform_error(void)
{
    return SDL_GetError();
}

const char *up_platform_audio_driver(void)
{
    const char *driver = SDL_GetCurrentAudioDriver();
    return driver ? driver : "unknown";
}

void up_platform_delay(uint32_t milliseconds)
{
    SDL_Delay(milliseconds);
}

int up_platform_poll_event(UpEvent *event)
{
    SDL_Event native;
    if (!SDL_PollEvent(&native))
        return 0;
#ifdef __APPLE__
    update_window_drag(&native);
#endif
    *event = (UpEvent) {0};
    switch (native.type) {
    case SDL_EVENT_QUIT:
        event->type = UP_EVENT_QUIT;
        break;
    case SDL_EVENT_WINDOW_CLOSE_REQUESTED:
        event->type = UP_EVENT_WINDOW_CLOSE;
        break;
    case SDL_EVENT_WINDOW_RESIZED:
    case SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED:
    case SDL_EVENT_WINDOW_HDR_STATE_CHANGED:
        event->type = UP_EVENT_WINDOW_RESIZED;
        break;
    case SDL_EVENT_WINDOW_EXPOSED:
        event->type = UP_EVENT_WINDOW_EXPOSED;
        break;
    case SDL_EVENT_WINDOW_FOCUS_GAINED:
        event->type = UP_EVENT_WINDOW_FOCUS_GAINED;
        break;
    case SDL_EVENT_WINDOW_FOCUS_LOST:
        event->type = UP_EVENT_WINDOW_FOCUS_LOST;
        break;
    case SDL_EVENT_MOUSE_MOTION:
        event->type = UP_EVENT_MOUSE_MOTION;
        event->x = native.motion.x;
        event->y = native.motion.y;
        break;
    case SDL_EVENT_WINDOW_MOUSE_LEAVE:
        event->type = UP_EVENT_MOUSE_LEAVE;
        break;
    case SDL_EVENT_MOUSE_BUTTON_DOWN:
    case SDL_EVENT_MOUSE_BUTTON_UP:
        event->type = native.type == SDL_EVENT_MOUSE_BUTTON_DOWN
            ? UP_EVENT_MOUSE_BUTTON_DOWN : UP_EVENT_MOUSE_BUTTON_UP;
        event->x = native.button.x;
        event->y = native.button.y;
        event->button = native.button.button;
        event->clicks = native.button.clicks;
        break;
    case SDL_EVENT_KEY_DOWN:
        event->type = UP_EVENT_KEY_DOWN;
        event->key = translate_key(native.key.key);
        event->repeat = native.key.repeat;
        if (native.key.mod & SDL_KMOD_SHIFT) event->modifiers |= UP_MOD_SHIFT;
        if (native.key.mod & SDL_KMOD_CTRL) event->modifiers |= UP_MOD_CTRL;
#ifdef __APPLE__
        if (native.key.mod & SDL_KMOD_GUI) event->modifiers |= UP_MOD_CTRL;
#endif
        break;
    case SDL_EVENT_TEXT_INPUT:
        event->type = UP_EVENT_TEXT_INPUT;
        SDL_strlcpy(event->text, native.text.text, sizeof(event->text));
        break;
    default:
        event->type = UP_EVENT_NONE;
        break;
    }
    return 1;
}

void up_platform_capture_mouse(int captured)
{
    SDL_CaptureMouse(captured != 0);
}

int up_platform_show_cursor(int visible)
{
    return visible ? SDL_ShowCursor() : SDL_HideCursor();
}

int up_window_can_hide_cursor(UpWindow *window)
{
    const SDL_WindowFlags required = SDL_WINDOW_FULLSCREEN |
                                    SDL_WINDOW_INPUT_FOCUS |
                                    SDL_WINDOW_MOUSE_FOCUS;
    return (SDL_GetWindowFlags(WINDOW(window)) & required) == required &&
           SDL_GetMouseState(NULL, NULL) == 0;
}

void up_platform_start_text_input(int started)
{
    if (started) SDL_StartTextInput(SDL_GetKeyboardFocus());
    else SDL_StopTextInput(SDL_GetKeyboardFocus());
}

UpWindow *up_window_create(const char *title, int width, int height)
{
#ifdef __APPLE__
    const SDL_WindowFlags backend = SDL_WINDOW_METAL | SDL_WINDOW_TRANSPARENT;
#else
    const SDL_WindowFlags backend = SDL_WINDOW_VULKAN | SDL_WINDOW_TRANSPARENT;
#endif
    SDL_Window *window = SDL_CreateWindow(
        title, width, height, backend | SDL_WINDOW_RESIZABLE |
        SDL_WINDOW_HIGH_PIXEL_DENSITY | SDL_WINDOW_BORDERLESS);
    if (!window)
        return NULL;
    if (!SDL_SetWindowHitTest(window, resize_hit_test, NULL)) {
        SDL_DestroyWindow(window);
        return NULL;
    }
    return (UpWindow *) window;
}

void up_window_destroy(UpWindow *window)
{
#ifdef __APPLE__
    if (window_drag.window == SDL_GetWindowID(WINDOW(window))) cancel_window_drag();
#endif
    SDL_DestroyWindow(WINDOW(window));
}

int up_window_size(UpWindow *window, int *width, int *height)
{
    return SDL_GetWindowSize(WINDOW(window), width, height);
}

int up_window_pixel_size(UpWindow *window, int *width, int *height)
{
    return SDL_GetWindowSizeInPixels(WINDOW(window), width, height);
}

int up_window_set_minimum_size(UpWindow *window, int width, int height)
{
    return SDL_SetWindowMinimumSize(WINDOW(window), width, height);
}

int up_window_set_fullscreen(UpWindow *window, int fullscreen)
{
    return SDL_SetWindowFullscreen(WINDOW(window), fullscreen != 0);
}

float up_window_corner_scale(UpWindow *window)
{
    if (SDL_GetWindowFlags(WINDOW(window)) & (SDL_WINDOW_FULLSCREEN | SDL_WINDOW_MAXIMIZED))
        return 0.0f;
    return SDL_GetWindowDisplayScale(WINDOW(window));
}

float up_window_corner_radius(UpWindow *window)
{
    return SDL_GetFloatProperty(SDL_GetWindowProperties(WINDOW(window)),
                                UP_CORNER_RADIUS_PROPERTY, 0.0f);
}

#ifdef __APPLE__
int up_window_set_corner_radius(UpWindow *window, float radius)
{
    if (!isfinite(radius) || radius < 0)
        return SDL_SetError("invalid window corner radius");
    SDL_Window *native = WINDOW(window);
    int width, height;
    if (!SDL_GetWindowSizeInPixels(native, &width, &height))
        return 0;
    SDL_Surface *shape = radius > 0 ? up_corner_shape(width, height, radius) : NULL;
    if (radius > 0 && !shape)
        return 0;
    const bool success = SDL_SetWindowShape(native, shape);
    SDL_DestroySurface(shape);
    if (!success)
        return 0;
    return SDL_SetFloatProperty(SDL_GetWindowProperties(native), UP_CORNER_RADIUS_PROPERTY, radius);
}
#endif

UpAudioStream *up_audio_stream_create(int rate, int channels)
{
    SDL_AudioSpec spec = {
        .format = SDL_AUDIO_F32,
        .channels = channels,
        .freq = rate,
    };
    return (UpAudioStream *) SDL_OpenAudioDeviceStream(
        SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK, &spec, NULL, NULL);
}

void up_audio_stream_destroy(UpAudioStream *stream)
{
    SDL_DestroyAudioStream(AUDIO(stream));
}

int up_audio_stream_put(UpAudioStream *stream, const void *data, int bytes)
{
    return SDL_PutAudioStreamData(AUDIO(stream), data, bytes);
}

int up_audio_stream_queued(UpAudioStream *stream)
{
    return SDL_GetAudioStreamQueued(AUDIO(stream));
}

int up_audio_stream_resume(UpAudioStream *stream)
{
    return SDL_ResumeAudioStreamDevice(AUDIO(stream));
}

int up_audio_stream_pause(UpAudioStream *stream)
{
    return SDL_PauseAudioStreamDevice(AUDIO(stream));
}

int up_audio_stream_set_gain(UpAudioStream *stream, float gain)
{
    return SDL_SetAudioStreamGain(AUDIO(stream), gain);
}

int up_audio_stream_clear(UpAudioStream *stream)
{
    return SDL_ClearAudioStream(AUDIO(stream));
}
