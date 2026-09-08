#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Pango/Cairo own the natural-size mask until up_text_mask_free is called.
typedef struct UpTextMask {
    const uint8_t *pixels;
    void *surface;
    int width, height, stride;
} UpTextMask;

char *up_text_normalize(const char *text, size_t length);
size_t up_text_decompose(uint32_t character, uint32_t *result, size_t capacity);
void up_text_free(char *text);
bool up_text_mask_create(const char *text, size_t length, UpTextMask *mask);
void up_text_mask_free(UpTextMask *mask);
