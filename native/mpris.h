#pragma once

#include <stdbool.h>
#include <stdint.h>

typedef struct UpMpris UpMpris;

// Borrowed values from Rust, copied into a GVariant before the next callback.
enum UpMprisValueKind {
    UP_MPRIS_VALUE_NONE = 0,
    UP_MPRIS_VALUE_BOOL,
    UP_MPRIS_VALUE_INT64,
    UP_MPRIS_VALUE_DOUBLE,
    UP_MPRIS_VALUE_STRING,
    UP_MPRIS_VALUE_EMPTY_STRINGS,
    UP_MPRIS_VALUE_METADATA,
};

typedef struct UpMprisValue {
    uint32_t kind;
    int64_t integer;
    double real;
    const char *text;
    const char *track_id;
    const char *title;
    const char *uri;
    int64_t duration_us;
} UpMprisValue;

typedef struct UpMprisCallbacks {
    void *data;
    void (*command)(void *data, bool root, const char *method,
                    const char *track_id, int64_t value);
    bool (*property)(void *data, bool root, const char *name, UpMprisValue *value);
} UpMprisCallbacks;

UpMpris *up_mpris_create(const char *bus_name, const char *introspection_xml,
                         const UpMprisCallbacks *callbacks);
int up_mpris_active(const UpMpris *mpris);
const char *up_mpris_error(const UpMpris *mpris);
void up_mpris_dispatch(UpMpris *mpris);
void up_mpris_status_changed(UpMpris *mpris, const char *status);
void up_mpris_seeked(UpMpris *mpris, int64_t position_us);
void up_mpris_destroy(UpMpris *mpris);
