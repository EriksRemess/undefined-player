#pragma once

#include <stddef.h>
#include <stdint.h>

typedef struct UpWindow UpWindow;
typedef struct UpAudioStream UpAudioStream;

enum UpEventType {
    UP_EVENT_NONE = 0,
    UP_EVENT_QUIT,
    UP_EVENT_WINDOW_CLOSE,
    UP_EVENT_WINDOW_RESIZED,
    UP_EVENT_WINDOW_EXPOSED,
    UP_EVENT_WINDOW_FOCUS_GAINED,
    UP_EVENT_WINDOW_FOCUS_LOST,
    UP_EVENT_MOUSE_MOTION,
    UP_EVENT_MOUSE_BUTTON_DOWN,
    UP_EVENT_MOUSE_BUTTON_UP,
    UP_EVENT_KEY_DOWN,
};

enum UpKey {
    UP_KEY_OTHER = 0,
    UP_KEY_Q,
    UP_KEY_J,
    UP_KEY_LEFT,
    UP_KEY_RIGHT,
    UP_KEY_F,
    UP_KEY_I,
    UP_KEY_SPACE,
    UP_KEY_S,
    UP_KEY_A,
};

typedef struct UpEvent {
    enum UpEventType type;
    enum UpKey key;
    int repeat;
    float x;
    float y;
    uint8_t button;
    uint8_t clicks;
} UpEvent;

#define UP_MOUSE_BUTTON_LEFT 1

int up_platform_init(void);
void up_platform_quit(void);
const char *up_platform_error(void);
void up_platform_delay(uint32_t milliseconds);
int up_platform_poll_event(UpEvent *event);
void up_platform_capture_mouse(int captured);

UpWindow *up_window_create(const char *title, int width, int height);
void up_window_destroy(UpWindow *window);
int up_window_size(UpWindow *window, int *width, int *height);
int up_window_pixel_size(UpWindow *window, int *width, int *height);
int up_window_set_minimum_size(UpWindow *window, int width, int height);
int up_window_set_fullscreen(UpWindow *window, int fullscreen);

UpAudioStream *up_audio_stream_create(int rate, int channels);
void up_audio_stream_destroy(UpAudioStream *stream);
int up_audio_stream_put(UpAudioStream *stream, const void *data, int bytes);
int up_audio_stream_queued(UpAudioStream *stream);
int up_audio_stream_resume(UpAudioStream *stream);
int up_audio_stream_pause(UpAudioStream *stream);
int up_audio_stream_clear(UpAudioStream *stream);
