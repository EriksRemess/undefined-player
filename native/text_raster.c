#include "text_raster.h"
#include <pango/pangocairo.h>
#include <math.h>

char *up_text_normalize(const char *text, size_t length)
{
    return g_utf8_normalize(text, (gssize) length, G_NORMALIZE_DEFAULT_COMPOSE);
}

size_t up_text_decompose(uint32_t character, uint32_t *result, size_t capacity)
{
    return g_unichar_fully_decompose(character, false, result, capacity);
}

void up_text_free(char *text)
{
    g_free(text);
}

bool up_text_mask_create(const char *text, size_t length, UpTextMask *mask)
{
    *mask = (UpTextMask) {0};
    // Record without a clip or a scale transform: scaling vector text changes
    // font hinting and can put pixels beyond Pango's measured ink rectangle.
    cairo_surface_t *recording = cairo_recording_surface_create(CAIRO_CONTENT_ALPHA, NULL);
    cairo_t *context = cairo_create(recording);
    cairo_font_options_t *options = cairo_font_options_create();
    cairo_font_options_set_antialias(options, CAIRO_ANTIALIAS_GRAY);
    cairo_font_options_set_hint_style(options, CAIRO_HINT_STYLE_FULL);
    cairo_font_options_set_hint_metrics(options, CAIRO_HINT_METRICS_ON);
    cairo_set_font_options(context, options);
    PangoLayout *layout = pango_cairo_create_layout(context);
    PangoFontDescription *font = pango_font_description_new();
    pango_font_description_set_family(font, "DejaVu Sans Mono");
    pango_font_description_set_absolute_size(font, 18 * PANGO_SCALE);
    pango_layout_set_font_description(layout, font);
    pango_layout_set_single_paragraph_mode(layout, true);
    pango_layout_set_text(layout, text, (int) length);
    cairo_set_source_rgba(context, 1, 1, 1, 1);
    pango_cairo_show_layout(context, layout);

    double ink_x, ink_y, ink_width, ink_height;
    cairo_recording_surface_ink_extents(recording, &ink_x, &ink_y, &ink_width, &ink_height);
    bool success = cairo_status(context) == CAIRO_STATUS_SUCCESS;
    if (success && ink_width > 0 && ink_height > 0) {
        // Leave room for rasterization at the integer pixel boundaries.
        const int left = (int) floor(ink_x) - 2;
        const int top = (int) floor(ink_y) - 2;
        const int mask_width = (int) ceil(ink_x + ink_width) - left + 2;
        const int mask_height = (int) ceil(ink_y + ink_height) - top + 2;
        cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_A8, mask_width, mask_height);
        cairo_t *mask_context = cairo_create(surface);
        cairo_set_source_surface(mask_context, recording, -left, -top);
        cairo_paint(mask_context);
        cairo_surface_flush(surface);
        success = cairo_surface_status(surface) == CAIRO_STATUS_SUCCESS &&
                  cairo_status(mask_context) == CAIRO_STATUS_SUCCESS;
        if (success) {
            *mask = (UpTextMask) {
                .pixels = cairo_image_surface_get_data(surface),
                .surface = surface,
                .width = mask_width, .height = mask_height,
                .stride = cairo_image_surface_get_stride(surface),
            };
        }
        cairo_destroy(mask_context);
        if (!success)
            cairo_surface_destroy(surface);
    }
    pango_font_description_free(font);
    g_object_unref(layout);
    cairo_font_options_destroy(options);
    cairo_destroy(context);
    cairo_surface_destroy(recording);
    return success;
}

void up_text_mask_free(UpTextMask *mask)
{
    if (mask->surface)
        cairo_surface_destroy(mask->surface);
    *mask = (UpTextMask) {0};
}
