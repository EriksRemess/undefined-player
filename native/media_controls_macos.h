#pragma once
#include <stdbool.h>
#include <stdint.h>

enum UpMacMediaCommand {
    UP_MAC_MEDIA_PLAY = 1, UP_MAC_MEDIA_PAUSE, UP_MAC_MEDIA_TOGGLE,
    UP_MAC_MEDIA_STOP, UP_MAC_MEDIA_SEEK, UP_MAC_MEDIA_POSITION,
    UP_MAC_MEDIA_NEXT, UP_MAC_MEDIA_PREVIOUS,
};

void *up_mac_media_create(const char *title, const char *artist, double duration, const char *artwork);
void up_mac_media_navigation(void *media, bool previous, bool next);
int up_mac_media_poll(void *media, int64_t *value);
// status: 0 = playing, 1 = paused, 2 = stopped, -1 = retain state; positions are seconds.
void up_mac_media_update(void *media, int status, double position, bool force);
void up_mac_media_destroy(void *media);
