#include "playback_macos.m"
#include <assert.h>

static void pump_until_at(BOOL (^condition)(void), int line)
{
    const double deadline = NSProcessInfo.processInfo.systemUptime + 15;
    while (!condition() && NSProcessInfo.processInfo.systemUptime < deadline) {
        SDL_PumpEvents();
        [NSRunLoop.mainRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
    }
    if (!condition()) {
        fprintf(stderr, "Native playback timed out at test line %d\n", line);
        abort();
    }
}
#define pump_until(condition) pump_until_at(condition, __LINE__)

static void playback_controls(SDL_Renderer *renderer, const char *path)
{
    UpNativePlayback *playback = up_native_playback_create(renderer, path);
    assert(playback);
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    pump_until(^BOOL { return up_native_playback_status(playback) != 0; });
    assert(up_native_playback_status(playback) == 1);
    up_native_playback_mute(playback, 1);
    assert(state.player.muted);
    up_native_playback_mute(playback, 0);
    assert(!state.player.muted);
    up_native_playback_pause(playback, 0);
    pump_until(^BOOL { return up_native_playback_time(playback) > 0.15; });
    up_native_playback_pause(playback, 1);
    const double paused = up_native_playback_time(playback);
    [NSRunLoop.mainRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.1]];
    assert(fabs(up_native_playback_time(playback) - paused) < 0.02);
    up_native_playback_seek(playback, 1.0);
    up_native_playback_seek(playback, 0.5);
    pump_until(^BOOL { return up_native_playback_status(playback) == 1; });
    assert(fabs(up_native_playback_time(playback) - 0.5) < 0.05);
    assert(state.player.rate == 0);
    const double duration = CMTimeGetSeconds(state.player.currentItem.duration);
    up_native_playback_seek(playback, duration - 0.05);
    pump_until(^BOOL { return up_native_playback_status(playback) == 1; });
    up_native_playback_pause(playback, 0);
    pump_until(^BOOL { return up_native_playback_status(playback) == 2; });
    // Replay while the caller still requests playback, as for loop-at-EOF.
    up_native_playback_seek(playback, 0);
    pump_until(^BOOL { return up_native_playback_status(playback) == 1; });
    up_native_playback_pause(playback, 0);
    pump_until(^BOOL { return up_native_playback_time(playback) > 0.15; });
    up_native_playback_seek(playback, 1);
    up_native_playback_destroy(playback);
    [NSRunLoop.mainRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.1]];
    assert(!state.view.superview && !state.player.currentItem);
    puts("Native playback: pause, mute, rapid seeks, EOF replay and pending-seek cleanup passed");
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        assert(SDL_Init(SDL_INIT_VIDEO));
        SDL_Window *window = SDL_CreateWindow("Native playback layout", 640, 480,
            SDL_WINDOW_METAL | SDL_WINDOW_TRANSPARENT | SDL_WINDOW_HIDDEN | SDL_WINDOW_RESIZABLE);
        assert(window);
        SDL_Renderer *renderer = SDL_CreateRenderer(window, "metal");
        assert(renderer);
        CAMetalLayer *overlay = (__bridge CAMetalLayer *)SDL_GetRenderMetalLayer(renderer);
        const BOOL opaque = overlay.opaque, hidden = overlay.hidden;
        assert(argc == 2);
        const NSUInteger subviews = ((__bridge NSWindow *)SDL_GetPointerProperty(
            SDL_GetWindowProperties(window), SDL_PROP_WINDOW_COCOA_WINDOW_POINTER, NULL)).contentView.subviews.count;
        assert(!up_native_playback_create(renderer, "/nonexistent/layout-test.mp4"));
        assert(overlay.opaque == opaque && overlay.hidden == hidden);
        UpNativePlayback *playback = up_native_playback_create(renderer, argv[1]);
        assert(playback);
        UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
        AVPlayerLayer *video = (AVPlayerLayer *)state.view.layer;
        assert(CGColorEqualToColor(video.backgroundColor, NSColor.blackColor.CGColor));
        assert(CGColorGetAlpha(video.backgroundColor) == 1);
        up_native_playback_layout(playback, 0, 24);
        assert(video.masksToBounds && video.cornerRadius > 0);
        assert([video.videoGravity isEqual:AVLayerVideoGravityResizeAspect]);
        // Exercise the same content-view resize and square corners as fullscreen,
        // including a taller viewport with letterboxing, while controls are hidden.
        up_native_playback_overlay(playback, 0);
        assert(overlay.hidden);
        NSView *parent = state.view.superview;
        [parent setFrameSize:NSMakeSize(1280, 832)];
        up_native_playback_layout(playback, 0, 0);
        assert(NSEqualRects(state.view.frame, parent.bounds));
        assert(video.cornerRadius == 0);
        assert(CGColorGetAlpha(video.backgroundColor) == 1);
        up_native_playback_layout(playback, 1, 0);
        assert([video.videoGravity isEqual:AVLayerVideoGravityResizeAspectFill]);
        up_native_playback_overlay(playback, 1);
        assert(!overlay.hidden);
        up_native_playback_overlay(playback, 0);
        up_native_playback_destroy(playback);
        assert(overlay.opaque == opaque && overlay.hidden == hidden);
        assert(!state.view.superview);
        assert(parent.subviews.count == subviews);
        playback_controls(renderer, argv[1]);
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(window);
        SDL_Quit();
    }
    puts("Native playback: opaque letterboxing, resize, corners and overlay cleanup passed");
    return 0;
}
