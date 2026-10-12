/* Network HLS startup and bounded segment read-ahead. Included by the adapter. */
#include <libavutil/opt.h>
#include <pthread.h>
#include <stdatomic.h>
#include <glib.h>

extern int up_hls_select_variant(uint8_t *buffer, int size);
extern void up_hls_master_playlists(const uint8_t *buffer, int size, void *opaque,
                                    void (*callback)(void *, const char *, int));
extern void up_hls_subtitle_segments(const uint8_t *buffer, int size,
                                void *opaque, void (*callback)(void *, const char *));

static void network_options(AVDictionary **options)
{
    av_dict_set(options, "rw_timeout", "15000000", AV_DICT_DONT_OVERWRITE);
    av_dict_set(options, "reconnect", "1", AV_DICT_DONT_OVERWRITE);
    av_dict_set(options, "reconnect_on_network_error", "1", AV_DICT_DONT_OVERWRITE);
    av_dict_set(options, "reconnect_on_http_error", "503,504", AV_DICT_DONT_OVERWRITE);
    av_dict_set(options, "reconnect_delay_max", "2", AV_DICT_DONT_OVERWRITE);
    av_dict_set(options, "reconnect_max_retries", "3", AV_DICT_DONT_OVERWRITE);
}

struct HlsInput {
    uint8_t *manifest;
    int size;
    int position;
    int async_available;
    int opening;
    atomic_int http_error;
    AVDictionary *options;
    AVDictionary *subtitle_urls;
    AVDictionary *playlist_urls;
};

static void hls_record_http_error(struct HlsInput *input, int result)
{
    if (result == AVERROR_HTTP_UNAUTHORIZED || result == AVERROR_HTTP_FORBIDDEN ||
        result == AVERROR_HTTP_NOT_FOUND || result == AVERROR_HTTP_OTHER_4XX)
        atomic_store(&input->http_error, result);
}

static int hls_manifest_read(void *opaque, uint8_t *buffer, int size)
{
    struct HlsInput *input = opaque;
    int count = FFMIN(size, input->size - input->position);
    if (!count)
        return AVERROR_EOF;
    memcpy(buffer, input->manifest + input->position, count);
    input->position += count;
    return count;
}

static void hls_input_free(struct HlsInput *input)
{
    if (!input)
        return;
    av_free(input->manifest);
    av_dict_free(&input->options);
    av_dict_free(&input->subtitle_urls);
    av_dict_free(&input->playlist_urls);
    av_free(input);
}

struct HlsSubtitleRegistration {
    struct HlsInput *input;
    const char *base;
    const char *kind;
};

static void hls_register_subtitle(void *opaque, const char *uri)
{
    struct HlsSubtitleRegistration *registration = opaque;
    /* GLib is already used by the player. Preserve escaping in signed URLs. */
    char *url = g_uri_resolve_relative(registration->base, uri,
                                      G_URI_FLAGS_ENCODED, NULL);
    if (url) {
        av_dict_set(&registration->input->subtitle_urls, url, registration->kind,
                    AV_DICT_MATCH_CASE);
        g_free(url);
    }
}

static void hls_register_playlist(void *opaque, const char *uri, int subtitle)
{
    struct HlsSubtitleRegistration *registration = opaque;
    char *url = g_uri_resolve_relative(registration->base, uri,
                                      G_URI_FLAGS_ENCODED, NULL);
    if (url) {
        av_dict_set(&registration->input->playlist_urls, url, "1", AV_DICT_MATCH_CASE);
        if (subtitle)
            av_dict_set(&registration->input->subtitle_urls, url, "playlist", AV_DICT_MATCH_CASE);
        g_free(url);
    }
}

struct HlsSubtitleData {
    uint8_t *data;
    int size, position;
};

static int hls_subtitle_manifest(struct HlsInput *input, const char *url,
                                  const char *base, uint8_t **data, int size)
{
    /* Forget the previous live window rather than retaining every segment URL
     * ever advertised. Existing subtitle IO already owns its buffered bytes. */
    const AVDictionaryEntry *entry = NULL;
    while ((entry = av_dict_get(input->subtitle_urls, "", entry, AV_DICT_IGNORE_SUFFIX))) {
        if (!strcmp(entry->value, url)) {
            av_dict_set(&input->subtitle_urls, entry->key, NULL, AV_DICT_MATCH_CASE);
            entry = NULL;
        }
    }

    /* Custom memory IO cannot expose HTTP's redirect location to HLS. Make
     * segment references absolute before returning the subtitle playlist. */
    GString *manifest = g_string_new(NULL);
    const char *cursor = (const char *) *data;
    const char *end = cursor + size;
    while (cursor < end) {
        const char *newline = memchr(cursor, '\n', end - cursor);
        const char *line_end = newline ? newline : end;
        char *line = g_strndup(cursor, line_end - cursor);
        g_strstrip(line);
        char *absolute = *line && *line != '#' ?
            g_uri_resolve_relative(base, line, G_URI_FLAGS_ENCODED, NULL) : NULL;
        g_string_append(manifest, absolute ? absolute : line);
        g_string_append_c(manifest, '\n');
        g_free(absolute);
        g_free(line);
        if (manifest->len > 8 * 1024 * 1024)
            break;
        cursor = newline ? newline + 1 : end;
    }
    int result = manifest->len > 8 * 1024 * 1024 ? AVERROR(EFBIG) : (int) manifest->len;
    uint8_t *replacement = result > 0 ? av_memdup(manifest->str, manifest->len) : NULL;
    g_string_free(manifest, TRUE);
    if (result > 0 && !replacement)
        return AVERROR(ENOMEM);
    if (replacement) {
        av_free(*data);
        *data = replacement;
        struct HlsSubtitleRegistration registration = { input, base, url };
        up_hls_subtitle_segments(*data, result, &registration, hls_register_subtitle);
    }
    return result;
}

static int hls_subtitle_read(void *opaque, uint8_t *buffer, int size)
{
    struct HlsSubtitleData *data = opaque;
    int count = FFMIN(size, data->size - data->position);
    if (!count)
        return AVERROR_EOF;
    memcpy(buffer, data->data + data->position, count);
    data->position += count;
    return count;
}

/* Caption failures must not become fatal video/audio failures. Buffer each
 * declared subtitle resource before handing it to HLS, so even a read failure
 * after successful HTTP headers can be replaced with an empty caption file. */
static int hls_subtitle_open(AVFormatContext *format, AVIOContext **io,
                              const char *url, int playlist, AVDictionary **options)
{
    AVIOContext *remote = NULL;
    const int limit = 8 * 1024 * 1024;
    struct HlsSubtitleData *data = av_mallocz(sizeof(*data));
    if (!data)
        return AVERROR(ENOMEM);
    data->data = av_malloc(limit);
    if (!data->data) {
        av_free(data);
        return AVERROR(ENOMEM);
    }
    int result = avio_open2(&remote, url, AVIO_FLAG_READ,
                            &format->interrupt_callback, options);
    if (result >= 0) {
        result = avio_read(remote, data->data, limit);
        if (remote->error < 0 && remote->error != AVERROR_EOF)
            result = remote->error;
        if (result == limit)
            result = AVERROR(EFBIG);
    }
    if (result >= 0 && playlist) {
        uint8_t *location = NULL;
        av_opt_get(remote, "location", AV_OPT_SEARCH_CHILDREN, &location);
        result = hls_subtitle_manifest(format->opaque, url,
                    location && *location ? (char *) location : url, &data->data, result);
        av_free(location);
    }
    avio_closep(&remote);
    if (result == AVERROR_EXIT) {
        av_free(data->data);
        av_free(data);
        return result;
    }
    if (result <= 0) {
        av_log(format, AV_LOG_WARNING, "Skipping unavailable HLS subtitles\n");
        const char *empty = playlist ?
            "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXT-X-ENDLIST\n" : "WEBVTT\n\n";
        result = (int) strlen(empty);
        memcpy(data->data, empty, result);
    }
    data->size = result;
    uint8_t *compact = av_realloc(data->data, data->size);
    if (compact)
        data->data = compact;
    uint8_t *buffer = av_malloc(32768);
    *io = buffer ? avio_alloc_context(buffer, 32768, 0, data,
                                      hls_subtitle_read, NULL, NULL) : NULL;
    if (!*io) {
        av_free(buffer);
        av_free(data->data);
        av_free(data);
        return AVERROR(ENOMEM);
    }
    return 0;
}

/* FFmpeg's async protocol buffers bytes, but establishes HTTP synchronously.
 * Start that connection off-thread too, otherwise opening the next audio and
 * video segments can block decoding for several round trips. */
struct HlsConnection {
    pthread_t thread;
    struct HlsInput *input;
    int joined;
    atomic_int cancelled;
    AVIOInterruptCB interrupt;
    AVIOContext *io;
    AVDictionary *options;
    char *url;
    int result;
};

static int hls_connection_interrupted(void *opaque)
{
    struct HlsConnection *connection = opaque;
    return atomic_load(&connection->cancelled) ||
           (connection->interrupt.callback &&
            connection->interrupt.callback(connection->interrupt.opaque));
}

static void *hls_connection_open(void *opaque)
{
    struct HlsConnection *connection = opaque;
    AVIOInterruptCB interrupt = { hls_connection_interrupted, connection };
    connection->result = avio_open2(&connection->io, connection->url,
                                    AVIO_FLAG_READ, &interrupt,
                                    &connection->options);
    /* A speculative connection may never be consumed (for example after a
     * seek). Publish its error only when the demuxer actually reads it. */
    return NULL;
}

static void hls_connection_join(struct HlsConnection *connection)
{
    if (!connection->joined) {
        pthread_join(connection->thread, NULL);
        connection->joined = 1;
    }
}

static int hls_connection_read(void *opaque, uint8_t *buffer, int size)
{
    struct HlsConnection *connection = opaque;
    hls_connection_join(connection);
    int result = connection->result < 0 ? connection->result :
                 avio_read(connection->io, buffer, size);
    hls_record_http_error(connection->input, result);
    return result;
}

static int64_t hls_connection_seek(void *opaque, int64_t offset, int whence)
{
    struct HlsConnection *connection = opaque;
    hls_connection_join(connection);
    if (connection->result < 0)
        return connection->result;
    return whence == AVSEEK_SIZE ? avio_size(connection->io) :
           avio_seek(connection->io, offset, whence);
}

static int hls_io_close(AVFormatContext *format, AVIOContext *io)
{
    (void) format;
    if (io->read_packet == hls_subtitle_read) {
        struct HlsSubtitleData *data = io->opaque;
        av_free(data->data);
        av_free(data);
        av_freep(&io->buffer);
        avio_context_free(&io);
        return 0;
    }
    if (io->read_packet != hls_connection_read)
        return avio_close(io);
    struct HlsConnection *connection = io->opaque;
    atomic_store(&connection->cancelled, 1);
    hls_connection_join(connection);
    avio_closep(&connection->io);
    av_dict_free(&connection->options);
    av_free(connection->url);
    av_free(connection);
    av_freep(&io->buffer);
    avio_context_free(&io);
    return 0;
}

static int hls_prefetch_open(AVFormatContext *format, AVIOContext **io,
                              const char *url, AVDictionary *options)
{
    struct HlsConnection *connection = av_mallocz(sizeof(*connection));
    if (!connection)
        return AVERROR(ENOMEM);
    atomic_init(&connection->cancelled, 0);
    connection->input = format->opaque;
    connection->interrupt = format->interrupt_callback;
    connection->url = av_asprintf("async:%s", url);
    av_dict_copy(&connection->options, options, 0);
    uint8_t *buffer = av_malloc(32768);
    *io = buffer ? avio_alloc_context(buffer, 32768, 0, connection,
                hls_connection_read, NULL, hls_connection_seek) : NULL;
    int error = !connection->url || !*io ? ENOMEM :
                pthread_create(&connection->thread, NULL,
                                hls_connection_open, connection);
    if (error) {
        av_free(buffer);
        avio_context_free(io);
        av_dict_free(&connection->options);
        av_free(connection->url);
        av_free(connection);
        return AVERROR(error);
    }
    return 0;
}

static int hls_media_resource(const char *url)
{
    /* Manifest requests stay native so FFmpeg sees redirects and cookies.
     * Unknown resource types also retain normal protocol handling. */
    char *path = av_strndup(url, strcspn(url, "?#"));
    int media = path && av_match_ext(path, "ts,m4s,mp4,m4a,aac,mp3,ac3,ec3");
    av_free(path);
    return media;
}

static int hls_io_open(AVFormatContext *format, AVIOContext **io,
                       const char *url, int flags, AVDictionary **options)
{
    struct HlsInput *input = format->opaque;
    AVDictionary *request = NULL;
    av_dict_copy(&request, input->options, 0);
    av_dict_copy(&request, *options, 0);
    network_options(&request);
    av_dict_set(&request, "protocol_whitelist",
                "http,https,tcp,tls,crypto,data,async", 0);
    const AVDictionaryEntry *subtitle = av_dict_get(input->subtitle_urls, url, NULL,
                                                    AV_DICT_MATCH_CASE);
    if (subtitle && flags == AVIO_FLAG_READ) {
        int result = hls_subtitle_open(format, io, url,
                                       !strcmp(subtitle->value, "playlist"), &request);
        av_dict_free(&request);
        return result;
    }
    /* Byte-range requests retain native HTTP offsets. Encrypted resources
     * also retain FFmpeg's normal crypto protocol handling. */
    int media = hls_media_resource(url);
    int prefetch = input->async_available && media &&
                   flags == AVIO_FLAG_READ &&
                   (av_strstart(url, "https://", NULL) ||
                    av_strstart(url, "http://", NULL)) &&
                   !av_dict_get(request, "offset", NULL, 0) &&
                   !av_dict_get(request, "end_offset", NULL, 0);
    int result = prefetch ? hls_prefetch_open(format, io, url, request) :
        avio_open2(io, url, flags, &format->interrupt_callback, &request);
    /* Error capture is independent of the prefetch extension list. HLS also
     * accepts extensionless media, keys and playlist refresh requests.
     * Deferred requests publish failures only when consumed. */
    /* The demuxer can ignore unavailable renditions during master discovery.
     * Their manifest errors must not survive into the selected rendition. */
    if (!input->opening ||
        !av_dict_get(input->playlist_urls, url, NULL, AV_DICT_MATCH_CASE))
        hls_record_http_error(input, result);
    av_dict_free(&request);
    return result;
}

/* Probe through the original HTTP connection, so ordinary media does not
 * incur a second request. Only HLS replaces its top-level IO with a manifest. */
static int network_open(AVFormatContext **context, const char *source,
                         int select_hls, int *selected_hls,
                         AVDictionary **options)
{
    AVFormatContext *native = *context;

    AVIOContext *io = NULL;
    const AVInputFormat *demuxer = NULL;
    AVDictionary *request = NULL;
    av_dict_copy(&request, *options, 0);
    int result = avio_open2(&io, source, AVIO_FLAG_READ,
                            &native->interrupt_callback, &request);
    av_dict_free(&request);
    if (result < 0)
        goto fail;
    result = av_probe_input_buffer2(io, &demuxer, source, NULL, 0, 0);
    if (result < 0)
        goto fail;
    if (!av_match_name("hls", demuxer->name)) {
        native->pb = io;
        result = avformat_open_input(context, source, demuxer, options);
        if (result < 0) {
            avio_closep(&io);
        } else {
            /* avformat_open_input marks a supplied pb as caller-owned. This
             * is ordinary protocol IO, owned by the format after startup. */
            (*context)->flags &= ~AVFMT_FLAG_CUSTOM_IO;
        }
        return result;
    }

    struct HlsInput *input = av_mallocz(sizeof(*input));
    if (!input) {
        result = AVERROR(ENOMEM);
        goto fail;
    }
    atomic_init(&input->http_error, 0);
    const int limit = 8 * 1024 * 1024;
    input->manifest = av_malloc(limit);
    if (!input->manifest) {
        hls_input_free(input);
        result = AVERROR(ENOMEM);
        goto fail;
    }
    result = avio_read(io, input->manifest, limit);
    if (result <= 0 || result == limit ||
        (io->error < 0 && io->error != AVERROR_EOF)) {
        hls_input_free(input);
        result = result == limit ? AVERROR(EFBIG) :
                 io->error < 0 ? io->error : result;
        if (!result)
            result = AVERROR_INVALIDDATA;
        goto fail;
    }
    input->size = select_hls ? up_hls_select_variant(input->manifest, result) : result;
    *selected_hls = input->size != result;
    uint8_t *compact = av_realloc(input->manifest, input->size);
    if (compact)
        input->manifest = compact;
    /* Capture the redirect target before closing the original connection;
     * FFmpeg resolves relative playlist paths against this URL. */
    uint8_t *location = NULL;
    av_opt_get(io, "location", AV_OPT_SEARCH_CHILDREN, &location);
    const char *effective_url = location && *location ? (char *) location : source;
    struct HlsSubtitleRegistration registration = { input, effective_url, "playlist" };
    up_hls_master_playlists(input->manifest, input->size, &registration, hls_register_playlist);
    const char *keys[] = { "cookies", "user_agent", "headers" };
    for (unsigned int i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) {
        uint8_t *value = NULL;
        if (av_opt_get(io, keys[i], AV_OPT_SEARCH_CHILDREN, &value) >= 0 && value && *value)
            av_dict_set(&input->options, keys[i], (char *) value, 0);
        av_free(value);
    }
    avio_closep(&io);
    void *protocols = NULL;
    const char *protocol;
    while ((protocol = avio_enum_protocols(&protocols, 0)))
        if (!strcmp(protocol, "async"))
            input->async_available = 1;

    uint8_t *buffer = av_malloc(32768);
    io = buffer ? avio_alloc_context(buffer, 32768, 0, input,
                                      hls_manifest_read, NULL, NULL) : NULL;
    if (!io) {
        av_free(buffer);
        av_free(location);
        hls_input_free(input);
        result = AVERROR(ENOMEM);
        goto fail;
    }
    native->pb = io;
    native->opaque = input;
    native->io_open = hls_io_open;
    native->io_close2 = hls_io_close;
    native->flags |= AVFMT_FLAG_CUSTOM_IO;
    /* Async IO is not a native HTTP context: HLS cannot reuse it through
     * ff_http_do_new_request. Explicitly enable its next-segment connection. */
    av_dict_set(options, "http_persistent", "0", 0);
    av_dict_set(options, "http_multiple", "1", 0);
    /* HTTP HLS resources need not have file extensions (YouTube timedtext is
     * WebVTT). hls_io_open still restricts protocols; local files retain the
     * normal demuxer checks. */
    av_dict_set(options, "extension_picky", "0", 0);
    input->opening = 1;
    result = avformat_open_input(context, effective_url, demuxer, options);
    input->opening = 0;
    av_free(location);
    if (result < 0) {
        av_freep(&io->buffer);
        avio_context_free(&io);
        hls_input_free(input);
    }
    return result;

fail:
    avio_closep(&io);
    avformat_free_context(native);
    *context = NULL;
    return result;
}
