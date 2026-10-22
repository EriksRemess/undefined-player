#pragma once
#include <SDL3/SDL.h>

typedef struct UpNativePlayback UpNativePlayback;
// All calls, including destruction, belong to the SDL main thread.
UpNativePlayback *up_native_playback_create(SDL_Renderer *renderer, const char *path);
void up_native_playback_destroy(UpNativePlayback *playback);
void up_native_playback_layout(UpNativePlayback *playback, int fill, float corner_radius);
void up_native_playback_overlay(UpNativePlayback *playback, int visible);
void up_native_playback_pause(UpNativePlayback *playback, int paused);
void up_native_playback_mute(UpNativePlayback *playback, int muted);
void up_native_playback_seek(UpNativePlayback *playback, double seconds);
double up_native_playback_time(UpNativePlayback *playback);
// 0 loading/seeking, 1 ready, 2 ended, -1 failed.
int up_native_playback_status(UpNativePlayback *playback);
const char *up_native_playback_error(UpNativePlayback *playback);
