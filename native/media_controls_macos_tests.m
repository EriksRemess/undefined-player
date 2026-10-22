#include "media_controls_macos.m"
#include <assert.h>

static int poll(void *media, int64_t expected)
{
    int64_t value = INT64_MIN;
    int command = up_mac_media_poll(media, &value);
    assert(value == expected);
    return command;
}

int main(void)
{
    @autoreleasepool {
        [NSApplication sharedApplication];
        void *pointer = up_mac_media_create("Test title", "Artist", 120, NULL);
        UpMacMedia *media = (__bridge UpMacMedia *) pointer;
        assert(media && media.registrations.count == 9);
        assert(MPRemoteCommandCenter.sharedCommandCenter.togglePlayPauseCommand.enabled);
        assert([media.info[MPMediaItemPropertyTitle] isEqual:@"Test title"]);
        assert([media.info[MPMediaItemPropertyArtist] isEqual:@"Artist"]);
        assert([media.info[MPMediaItemPropertyPlaybackDuration] doubleValue] == 120);
        assert([media enqueue:UP_MAC_MEDIA_TOGGLE value:0] == MPRemoteCommandHandlerStatusSuccess);
        assert(poll(pointer, 0) == UP_MAC_MEDIA_TOGGLE);
        assert([media enqueue:UP_MAC_MEDIA_NEXT value:0] == MPRemoteCommandHandlerStatusSuccess);
        assert(poll(pointer, 10000000) == UP_MAC_MEDIA_SEEK);
        assert([media enqueue:UP_MAC_MEDIA_PREVIOUS value:0] == MPRemoteCommandHandlerStatusSuccess);
        assert(poll(pointer, -10000000) == UP_MAC_MEDIA_SEEK);
        up_mac_media_navigation(pointer, true, true);
        [media enqueue:UP_MAC_MEDIA_NEXT value:0];
        assert(poll(pointer, 0) == UP_MAC_MEDIA_NEXT);
        [media enqueue:UP_MAC_MEDIA_PREVIOUS value:0];
        assert(poll(pointer, 0) == UP_MAC_MEDIA_PREVIOUS);
        [media enqueue:UP_MAC_MEDIA_POSITION value:31.5];
        assert(poll(pointer, 31500000) == UP_MAC_MEDIA_POSITION);
        assert([media enqueue:UP_MAC_MEDIA_POSITION value:NAN] == MPRemoteCommandHandlerStatusCommandFailed);
        int64_t value = 0;
        assert(up_mac_media_poll(pointer, &value) == 0);
        for (int i = 0; i < 24; i++) [media enqueue:UP_MAC_MEDIA_POSITION value:i];
        for (int i = 8; i < 24; i++) assert(poll(pointer, i * 1000000) == UP_MAC_MEDIA_POSITION);
        assert(up_mac_media_poll(pointer, &value) == 0);
        up_mac_media_update(pointer, 1, 25, false);
        assert(media.status == 1 && [media.info[MPNowPlayingInfoPropertyPlaybackRate] doubleValue] == 0);
        up_mac_media_update(pointer, -1, 50, true);
        assert(media.status == 1 && media.position == 50);
        up_mac_media_update(pointer, 0, 50, false);
        assert([media.info[MPNowPlayingInfoPropertyPlaybackRate] doubleValue] == 1);
        const double published = media.publishedAt;
        up_mac_media_update(pointer, 0, 50.1, false);
        assert(media.publishedAt == published); // do not publish 60 dictionaries/s
        up_mac_media_update(pointer, 2, 999, true);
        assert(media.position == 120 && media.status == 2);
        up_mac_media_destroy(pointer);
        assert(!media.active);
        assert([media enqueue:UP_MAC_MEDIA_TOGGLE value:0] == MPRemoteCommandHandlerStatusCommandFailed);
        assert(!MPNowPlayingInfoCenter.defaultCenter.nowPlayingInfo);
        pointer = up_mac_media_create("Stream", NULL, 0, NULL);
        media = (__bridge UpMacMedia *) pointer;
        assert(!MPRemoteCommandCenter.sharedCommandCenter.changePlaybackPositionCommand.enabled);
        assert([media enqueue:UP_MAC_MEDIA_NEXT value:0] == MPRemoteCommandHandlerStatusNoSuchContent);
        up_mac_media_destroy(pointer);
    }
    puts("macOS media controls: registration, queue, chapter/seek policy, playback state and cleanup passed");
    return 0;
}
