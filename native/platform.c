#include "platform.h"

#include <SDL3/SDL.h>

#include <stdlib.h>

#define WINDOW(value) ((SDL_Window *) (value))
#define AUDIO(value) ((SDL_AudioStream *) (value))

static enum UpKey translate_key(SDL_Keycode key)
{
    switch (key) {
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
    default:
        return UP_KEY_OTHER;
    }
}

static SDL_HitTestResult resize_hit_test(SDL_Window *window,
                                         const SDL_Point *point, void *data)
{
    (void) data;
    const int border = 10;
    const float top_bar_pixels = 42.0f;
    int width = 0, height = 0, pixel_width = 0, pixel_height = 0;
    SDL_GetWindowSize(window, &width, &height);
    SDL_GetWindowSizeInPixels(window, &pixel_width, &pixel_height);
    if (width > 0 && height > 0 && pixel_width > 0 && pixel_height > 0) {
        const float button_width = top_bar_pixels * width / pixel_width;
        const float button_height = top_bar_pixels * height / pixel_height;
        if (point->x >= width - button_width && point->x < width &&
            point->y >= 0 && point->y < button_height)
            return SDL_HITTEST_NORMAL;
    }
    const int left = point->x <= border;
    const int right = point->x >= width - border;
    const int top = point->y <= border;
    const int bottom = point->y >= height - border;
    if (top && left)
        return SDL_HITTEST_RESIZE_TOPLEFT;
    if (top && right)
        return SDL_HITTEST_RESIZE_TOPRIGHT;
    if (bottom && left)
        return SDL_HITTEST_RESIZE_BOTTOMLEFT;
    if (bottom && right)
        return SDL_HITTEST_RESIZE_BOTTOMRIGHT;
    if (top)
        return SDL_HITTEST_RESIZE_TOP;
    if (bottom)
        return SDL_HITTEST_RESIZE_BOTTOM;
    if (left)
        return SDL_HITTEST_RESIZE_LEFT;
    if (right)
        return SDL_HITTEST_RESIZE_RIGHT;
    return SDL_HITTEST_NORMAL;
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

UpWindow *up_window_create(const char *title, int width, int height)
{
    SDL_Window *window = SDL_CreateWindow(
        title, width, height,
        SDL_WINDOW_VULKAN | SDL_WINDOW_RESIZABLE |
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

int up_audio_stream_clear(UpAudioStream *stream)
{
    return SDL_ClearAudioStream(AUDIO(stream));
}
