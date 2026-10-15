#include "platform.h"
#include "input_geometry.h"

#include <SDL3/SDL.h>

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
    switch (up_input_hit_test_rounded(point->x, point->y, width, height, pixel_width, pixel_height,
            SDL_GetFloatProperty(SDL_GetWindowProperties(window), UP_CORNER_RADIUS_PROPERTY, 0.0f))) {
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

int up_platform_init(void)
{
    return SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO);
}

void up_platform_quit(void)
{
    SDL_Quit();
}

const char *up_platform_error(void)
{
    return SDL_GetError();
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
    SDL_Window *window = SDL_CreateWindow(
        title, width, height,
        SDL_WINDOW_VULKAN | SDL_WINDOW_RESIZABLE |
        SDL_WINDOW_HIGH_PIXEL_DENSITY | SDL_WINDOW_BORDERLESS | SDL_WINDOW_TRANSPARENT);
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
