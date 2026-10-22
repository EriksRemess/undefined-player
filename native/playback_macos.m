#import <AppKit/AppKit.h>
#import <AVFoundation/AVFoundation.h>
#import <QuartzCore/CAMetalLayer.h>
#include "playback_macos.h"

@interface UpNativeVideoView : NSView
@end
@implementation UpNativeVideoView
- (CALayer *)makeBackingLayer { return [AVPlayerLayer layer]; }
- (NSView *)hitTest:(NSPoint)point { (void)point; return nil; }
@end

@interface UpNativePlayerState : NSObject
@property(strong) AVPlayer *player;
@property(strong) UpNativeVideoView *view;
@property(strong) CAMetalLayer *overlay;
@property BOOL wasOpaque;
@property BOOL wasHidden;
@property BOOL paused;
@property BOOL seeking;
@property BOOL ended;
@property NSUInteger seekSerial;
@property(strong) id endObserver;
@end
@implementation UpNativePlayerState
@end

struct UpNativePlayback {
    CFTypeRef state;
    char error[512];
};

UpNativePlayback *up_native_playback_create(SDL_Renderer *renderer, const char *path)
{
    @autoreleasepool {
        SDL_Window *window = SDL_GetRenderWindow(renderer);
        NSWindow *native = (__bridge NSWindow *)SDL_GetPointerProperty(SDL_GetWindowProperties(window),
            SDL_PROP_WINDOW_COCOA_WINDOW_POINTER, NULL);
        CAMetalLayer *overlay = (__bridge CAMetalLayer *)SDL_GetRenderMetalLayer(renderer);
        NSString *filename = [[NSFileManager defaultManager] stringWithFileSystemRepresentation:path length:strlen(path)];
        if (!native || !overlay || !filename) return NULL;
        UpNativePlayback *result = calloc(1, sizeof(*result));
        if (!result) return NULL;
        UpNativePlayerState *state = [UpNativePlayerState new];
        state.player = [AVPlayer playerWithURL:[NSURL fileURLWithPath:filename]];
        state.paused = YES;
        state.overlay = overlay;
        state.wasOpaque = overlay.opaque;
        state.wasHidden = overlay.hidden;
        state.view = [[UpNativeVideoView alloc] initWithFrame:native.contentView.bounds];
        state.view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        state.view.wantsLayer = YES;
        AVPlayerLayer *video = (AVPlayerLayer *)state.view.layer;
        video.player = state.player;
        video.videoGravity = AVLayerVideoGravityResizeAspect;
        // The SDL window is transparent for rounded corners. AVPlayerLayer's
        // letterbox area must cover the desktop even when the overlay is hidden.
        video.backgroundColor = NSColor.blackColor.CGColor;
        [native.contentView addSubview:state.view positioned:NSWindowBelow relativeTo:nil];
        overlay.opaque = NO;
        __weak UpNativePlayerState *weak = state;
        state.endObserver = [NSNotificationCenter.defaultCenter addObserverForName:AVPlayerItemDidPlayToEndTimeNotification
            object:state.player.currentItem queue:NSOperationQueue.mainQueue usingBlock:^(NSNotification *notification) {
                (void)notification;
                UpNativePlayerState *current = weak;
                if (!current.seeking) current.ended = YES;
            }];
        result->state = CFBridgingRetain(state);
        // AV1 needs the output layer attached to finish preparing its decoder.
        // Keep FFmpeg intact until readiness, rolling back the layer on failure.
        const double deadline = NSProcessInfo.processInfo.systemUptime + 3;
        while (state.player.status != AVPlayerStatusFailed &&
               state.player.currentItem.status == AVPlayerItemStatusUnknown &&
               NSProcessInfo.processInfo.systemUptime < deadline) {
            SDL_PumpEvents();
            [NSRunLoop.mainRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
        if (state.player.status == AVPlayerStatusFailed ||
            state.player.currentItem.status != AVPlayerItemStatusReadyToPlay) {
            fprintf(stderr, "warning: native playback unavailable; using FFmpeg\n");
            up_native_playback_destroy(result);
            return NULL;
        }
        return result;
    }
}

void up_native_playback_destroy(UpNativePlayback *playback)
{
    if (!playback) return;
    @autoreleasepool {
        UpNativePlayerState *state = CFBridgingRelease(playback->state);
        state.paused = YES;
        ++state.seekSerial;
        [state.player pause];
        [state.player.currentItem cancelPendingSeeks];
        [NSNotificationCenter.defaultCenter removeObserver:state.endObserver];
        ((AVPlayerLayer *)state.view.layer).player = nil;
        [state.view removeFromSuperview];
        state.overlay.opaque = state.wasOpaque;
        state.overlay.hidden = state.wasHidden;
        [state.player replaceCurrentItemWithPlayerItem:nil];
        free(playback);
    }
}

void up_native_playback_layout(UpNativePlayback *playback, int fill, float radius)
{
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    AVPlayerLayer *layer = (AVPlayerLayer *)state.view.layer;
    layer.videoGravity = fill ? AVLayerVideoGravityResizeAspectFill : AVLayerVideoGravityResizeAspect;
    CGFloat scale = state.view.window.backingScaleFactor;
    layer.cornerRadius = scale > 0 ? radius / scale : radius;
    layer.masksToBounds = YES;
    [CATransaction commit];
}

void up_native_playback_pause(UpNativePlayback *playback, int paused)
{
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    if (state.paused == (paused != 0)) return;
    state.paused = paused != 0;
    if (state.paused) [state.player pause];
    else [state.player play];
}

void up_native_playback_overlay(UpNativePlayback *playback, int visible)
{
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    if (state.overlay.hidden == !visible) return;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    state.overlay.hidden = !visible;
    [CATransaction commit];
}

void up_native_playback_mute(UpNativePlayback *playback, int muted)
{
    ((__bridge UpNativePlayerState *)playback->state).player.muted = muted != 0;
}

void up_native_playback_seek(UpNativePlayback *playback, double seconds)
{
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    state.seeking = YES;
    state.ended = NO;
    NSUInteger serial = ++state.seekSerial;
    __weak UpNativePlayerState *weak = state;
    [state.player seekToTime:CMTimeMakeWithSeconds(fmax(0, seconds), 60000)
        toleranceBefore:kCMTimeZero toleranceAfter:kCMTimeZero completionHandler:^(BOOL finished) {
            dispatch_async(dispatch_get_main_queue(), ^{
                UpNativePlayerState *current = weak;
                if (current && current.seekSerial == serial) {
                    current.seeking = NO;
                    // EOF resets AVPlayer's rate independently of our requested
                    // pause state. A completed loop seek must resume playback.
                    if (finished && !current.paused) [current.player play];
                }
            });
        }];
}

double up_native_playback_time(UpNativePlayback *playback)
{
    double seconds = CMTimeGetSeconds(((__bridge UpNativePlayerState *)playback->state).player.currentTime);
    return isfinite(seconds) ? seconds : 0;
}

int up_native_playback_status(UpNativePlayback *playback)
{
    UpNativePlayerState *state = (__bridge UpNativePlayerState *)playback->state;
    if (state.player.status == AVPlayerStatusFailed || state.player.currentItem.status == AVPlayerItemStatusFailed) {
        NSError *error = state.player.error ?: state.player.currentItem.error;
        snprintf(playback->error, sizeof(playback->error), "%s", error.localizedDescription.UTF8String ?: "Native playback failed");
        return -1;
    }
    if (state.ended) return 2;
    return state.seeking || state.player.currentItem.status != AVPlayerItemStatusReadyToPlay ? 0 : 1;
}

const char *up_native_playback_error(UpNativePlayback *playback) { return playback->error; }
