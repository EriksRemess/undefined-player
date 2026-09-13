#pragma once

#include <stdint.h>

/* Rust owns the file, buffer and worker. Read returns zero at EOF and negative
 * errno on failure; this adapter translates EOF to FFmpeg's convention. */
void *up_read_ahead_open(const char *path, int *error);
int up_read_ahead_read(void *reader, uint8_t *buffer, int size);
int64_t up_read_ahead_seek(void *reader, int64_t offset, int whence);
int64_t up_read_ahead_size(void *reader);
void up_read_ahead_close(void *reader);
