#include "media_controls_macos.h"
#import <AppKit/AppKit.h>
#import <MediaPlayer/MediaPlayer.h>
#include <math.h>

@interface UpMacMedia : NSObject
@property(nonatomic, strong) NSMutableArray<NSArray *> *registrations;
@property(nonatomic, strong) NSMutableArray<NSArray<NSNumber *> *> *commands;
@property(nonatomic, strong) NSMutableDictionary *info;
@property(nonatomic) bool active, previous, next, seekable;
@property(nonatomic) int status;
@property(nonatomic) double duration, position, publishedAt;
- (MPRemoteCommandHandlerStatus)enqueue:(int)command value:(double)value;
- (void)bind:(MPRemoteCommand *)command kind:(int)kind;
@end

@implementation UpMacMedia
- (MPRemoteCommandHandlerStatus)enqueue:(int)command value:(double)value
{
    @synchronized(self) {
        if (!self.active || !isfinite(value)) return MPRemoteCommandHandlerStatusCommandFailed;
        if (command == UP_MAC_MEDIA_NEXT && !self.next) {
            if (!self.seekable) return MPRemoteCommandHandlerStatusNoSuchContent;
            command = UP_MAC_MEDIA_SEEK; value = 10;
        } else if (command == UP_MAC_MEDIA_PREVIOUS && !self.previous) {
            if (!self.seekable) return MPRemoteCommandHandlerStatusNoSuchContent;
            command = UP_MAC_MEDIA_SEEK; value = -10;
        }
        // Convert while still floating point, with a limit safely inside i64.
        const int64_t micros = (int64_t) (fmax(-9e12, fmin(9e12, value)) * 1000000);
        if (self.commands.count == 16) [self.commands removeObjectAtIndex:0];
        [self.commands addObject:@[@(command), @(micros)]];
        return MPRemoteCommandHandlerStatusSuccess;
    }
}

- (void)bind:(MPRemoteCommand *)command kind:(int)kind
{
    __weak UpMacMedia *weakSelf = self;
    id token = [command addTargetWithHandler:^MPRemoteCommandHandlerStatus(MPRemoteCommandEvent *event) {
        UpMacMedia *media = weakSelf;
        if (!media) return MPRemoteCommandHandlerStatusCommandFailed;
        double value = 0;
        if ([event isKindOfClass:MPChangePlaybackPositionCommandEvent.class])
            value = ((MPChangePlaybackPositionCommandEvent *) event).positionTime;
        else if ([event isKindOfClass:MPSkipIntervalCommandEvent.class])
            value = ((MPSkipIntervalCommandEvent *) event).interval;
        if (command == MPRemoteCommandCenter.sharedCommandCenter.skipBackwardCommand) value = -value;
        return [media enqueue:kind value:value];
    }];
    [self.registrations addObject:@[command, token]];
    command.enabled = YES;
}
@end

void *up_mac_media_create(const char *title, const char *artist, double duration, const char *artwork)
{
    @autoreleasepool {
        UpMacMedia *media = [UpMacMedia new];
        media.registrations = [NSMutableArray new];
        media.commands = [NSMutableArray new];
        media.active = true;
        media.status = -1;
        media.duration = isfinite(duration) ? fmax(0, duration) : 0;
        media.seekable = media.duration > 0;
        media.info = [@{MPMediaItemPropertyTitle: title && *title ? @(title) : @"Unknown media",
            MPNowPlayingInfoPropertyMediaType: @(MPNowPlayingInfoMediaTypeVideo),
            MPNowPlayingInfoPropertyDefaultPlaybackRate: @1.0,
            MPNowPlayingInfoPropertyIsLiveStream: @(!media.seekable)} mutableCopy];
        if (artist && *artist) media.info[MPMediaItemPropertyArtist] = @(artist);
        if (media.duration > 0) media.info[MPMediaItemPropertyPlaybackDuration] = @(media.duration);
        NSImage *image = artwork ? [[NSImage alloc] initWithContentsOfFile:@(artwork)] : nil;
        if (image) {
            media.info[MPMediaItemPropertyArtwork] = [[MPMediaItemArtwork alloc] initWithBoundsSize:image.size
                requestHandler:^NSImage *(CGSize size) { (void) size; return image; }];
        }
        MPRemoteCommandCenter *center = MPRemoteCommandCenter.sharedCommandCenter;
        [media bind:center.playCommand kind:UP_MAC_MEDIA_PLAY];
        [media bind:center.pauseCommand kind:UP_MAC_MEDIA_PAUSE];
        [media bind:center.togglePlayPauseCommand kind:UP_MAC_MEDIA_TOGGLE];
        [media bind:center.stopCommand kind:UP_MAC_MEDIA_STOP];
        [media bind:center.nextTrackCommand kind:UP_MAC_MEDIA_NEXT];
        [media bind:center.previousTrackCommand kind:UP_MAC_MEDIA_PREVIOUS];
        [media bind:center.changePlaybackPositionCommand kind:UP_MAC_MEDIA_POSITION];
        center.changePlaybackPositionCommand.enabled = media.seekable;
        [media bind:center.skipForwardCommand kind:UP_MAC_MEDIA_SEEK];
        [media bind:center.skipBackwardCommand kind:UP_MAC_MEDIA_SEEK];
        center.skipForwardCommand.preferredIntervals = @[@10];
        center.skipBackwardCommand.preferredIntervals = @[@10];
        center.skipForwardCommand.enabled = center.skipBackwardCommand.enabled = media.seekable;
        center.nextTrackCommand.enabled = center.previousTrackCommand.enabled = media.seekable;
        void *result = (void *) CFBridgingRetain(media);
        up_mac_media_update(result, 0, 0, true);
        return result;
    }
}

void up_mac_media_navigation(void *pointer, bool previous, bool next)
{
    @autoreleasepool {
        UpMacMedia *media = (__bridge UpMacMedia *) pointer;
        @synchronized(media) {
            if (media.previous == previous && media.next == next) return;
            media.previous = previous; media.next = next;
            MPRemoteCommandCenter.sharedCommandCenter.previousTrackCommand.enabled = previous || media.seekable;
            MPRemoteCommandCenter.sharedCommandCenter.nextTrackCommand.enabled = next || media.seekable;
        }
    }
}

int up_mac_media_poll(void *pointer, int64_t *value)
{
    @autoreleasepool {
        UpMacMedia *media = (__bridge UpMacMedia *) pointer;
        @synchronized(media) {
            NSArray<NSNumber *> *command = media.commands.firstObject;
            if (!command) return 0;
            const int kind = command[0].intValue;
            *value = command[1].longLongValue;
            [media.commands removeObjectAtIndex:0];
            return kind;
        }
    }
}

void up_mac_media_update(void *pointer, int status, double position, bool force)
{
    @autoreleasepool {
        UpMacMedia *media = (__bridge UpMacMedia *) pointer;
        if (!isfinite(position)) return;
        if (status < 0) status = media.status;
        position = fmax(0, position);
        if (media.duration > 0) position = fmin(position, media.duration);
        const double now = NSProcessInfo.processInfo.systemUptime;
        const double expected = media.position + (media.status == 0 ? now - media.publishedAt : 0);
        if (!force && status == media.status && fabs(position - expected) < 0.5) return;
        media.status = status; media.position = position; media.publishedAt = now;
        media.info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = @(position);
        media.info[MPNowPlayingInfoPropertyPlaybackRate] = @(status == 0 ? 1.0 : 0.0);
        MPNowPlayingInfoCenter *center = MPNowPlayingInfoCenter.defaultCenter;
        center.nowPlayingInfo = media.info;
        center.playbackState = status == 0 ? MPNowPlayingPlaybackStatePlaying :
            status == 1 ? MPNowPlayingPlaybackStatePaused : MPNowPlayingPlaybackStateStopped;
    }
}

void up_mac_media_destroy(void *pointer)
{
    @autoreleasepool {
        UpMacMedia *media = CFBridgingRelease(pointer);
        @synchronized(media) {
            media.active = false;
            [media.commands removeAllObjects];
        }
        for (NSArray *registration in media.registrations)
            [(MPRemoteCommand *) registration[0] removeTarget:registration[1]];
        MPNowPlayingInfoCenter.defaultCenter.playbackState = MPNowPlayingPlaybackStateStopped;
        MPNowPlayingInfoCenter.defaultCenter.nowPlayingInfo = nil;
    }
}
