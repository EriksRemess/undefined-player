#pragma once

#include <stdint.h>

typedef struct UpMpris UpMpris;

enum UpMprisCommand {
    UP_MPRIS_COMMAND_NONE = 0,
    UP_MPRIS_COMMAND_QUIT,
    UP_MPRIS_COMMAND_PLAY,
    UP_MPRIS_COMMAND_PAUSE,
    UP_MPRIS_COMMAND_PLAY_PAUSE,
    UP_MPRIS_COMMAND_STOP,
    UP_MPRIS_COMMAND_SEEK,
    UP_MPRIS_COMMAND_SET_POSITION,
};

enum UpMprisStatus {
    UP_MPRIS_STATUS_PLAYING = 0,
    UP_MPRIS_STATUS_PAUSED,
    UP_MPRIS_STATUS_STOPPED,
};

UpMpris *up_mpris_create(const char *title, const char *filename,
                         int64_t duration_us);
int up_mpris_active(const UpMpris *mpris);
const char *up_mpris_error(const UpMpris *mpris);
void up_mpris_dispatch(UpMpris *mpris);
enum UpMprisCommand up_mpris_take_command(UpMpris *mpris, int64_t *value);
void up_mpris_update(UpMpris *mpris, enum UpMprisStatus status,
                     int64_t position_us);
void up_mpris_seeked(UpMpris *mpris, int64_t position_us);
void up_mpris_destroy(UpMpris *mpris);
