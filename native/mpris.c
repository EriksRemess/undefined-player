#include "mpris.h"

#include <gio/gio.h>
#include <glib.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define COMMAND_QUEUE_CAPACITY 16
#define MPRIS_OBJECT_PATH "/org/mpris/MediaPlayer2"
#define TRACK_OBJECT_PATH "/com/github/undefined_player/track/1"

struct queued_command {
    enum UpMprisCommand command;
    int64_t value;
};

struct UpMpris {
    GDBusConnection *connection;
    GDBusNodeInfo *node_info;
    guint root_registration;
    guint player_registration;
    char bus_name[128];
    char error[256];
    char *title;
    char *uri;
    int64_t duration_us;
    int64_t position_us;
    enum UpMprisStatus status;
    struct queued_command commands[COMMAND_QUEUE_CAPACITY];
    unsigned int command_read;
    unsigned int command_write;
};

static const char introspection_xml[] =
    "<node>"
    " <interface name='org.mpris.MediaPlayer2'>"
    "  <method name='Raise'/>"
    "  <method name='Quit'/>"
    "  <property name='CanQuit' type='b' access='read'/>"
    "  <property name='Fullscreen' type='b' access='readwrite'/>"
    "  <property name='CanSetFullscreen' type='b' access='read'/>"
    "  <property name='CanRaise' type='b' access='read'/>"
    "  <property name='HasTrackList' type='b' access='read'/>"
    "  <property name='Identity' type='s' access='read'/>"
    "  <property name='DesktopEntry' type='s' access='read'/>"
    "  <property name='SupportedUriSchemes' type='as' access='read'/>"
    "  <property name='SupportedMimeTypes' type='as' access='read'/>"
    " </interface>"
    " <interface name='org.mpris.MediaPlayer2.Player'>"
    "  <method name='Next'/>"
    "  <method name='Previous'/>"
    "  <method name='Pause'/>"
    "  <method name='PlayPause'/>"
    "  <method name='Stop'/>"
    "  <method name='Play'/>"
    "  <method name='Seek'><arg direction='in' type='x' name='Offset'/></method>"
    "  <method name='SetPosition'>"
    "   <arg direction='in' type='o' name='TrackId'/>"
    "   <arg direction='in' type='x' name='Position'/>"
    "  </method>"
    "  <method name='OpenUri'><arg direction='in' type='s' name='Uri'/></method>"
    "  <signal name='Seeked'><arg type='x' name='Position'/></signal>"
    "  <property name='PlaybackStatus' type='s' access='read'/>"
    "  <property name='LoopStatus' type='s' access='readwrite'/>"
    "  <property name='Rate' type='d' access='readwrite'/>"
    "  <property name='Shuffle' type='b' access='readwrite'/>"
    "  <property name='Metadata' type='a{sv}' access='read'/>"
    "  <property name='Volume' type='d' access='readwrite'/>"
    "  <property name='Position' type='x' access='read'/>"
    "  <property name='MinimumRate' type='d' access='read'/>"
    "  <property name='MaximumRate' type='d' access='read'/>"
    "  <property name='CanGoNext' type='b' access='read'/>"
    "  <property name='CanGoPrevious' type='b' access='read'/>"
    "  <property name='CanPlay' type='b' access='read'/>"
    "  <property name='CanPause' type='b' access='read'/>"
    "  <property name='CanSeek' type='b' access='read'/>"
    "  <property name='CanControl' type='b' access='read'/>"
    " </interface>"
    "</node>";

static const char *status_name(enum UpMprisStatus status)
{
    switch (status) {
    case UP_MPRIS_STATUS_PAUSED:
        return "Paused";
    case UP_MPRIS_STATUS_STOPPED:
        return "Stopped";
    default:
        return "Playing";
    }
}

static void set_error(UpMpris *mpris, const char *message)
{
    snprintf(mpris->error, sizeof(mpris->error), "%s",
             message ? message : "unknown MPRIS error");
}

static void set_gerror(UpMpris *mpris, const char *context, GError *error)
{
    snprintf(mpris->error, sizeof(mpris->error), "%s: %s", context,
             error ? error->message : "unknown error");
    g_clear_error(&error);
}

static void queue_command(UpMpris *mpris, enum UpMprisCommand command,
                          int64_t value)
{
    const unsigned int next =
        (mpris->command_write + 1) % COMMAND_QUEUE_CAPACITY;
    if (next == mpris->command_read)
        mpris->command_read =
            (mpris->command_read + 1) % COMMAND_QUEUE_CAPACITY;
    mpris->commands[mpris->command_write] = (struct queued_command) {
        .command = command,
        .value = value,
    };
    mpris->command_write = next;
}

static GVariant *metadata_variant(const UpMpris *mpris)
{
    GVariantBuilder metadata;
    g_variant_builder_init(&metadata, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&metadata, "{sv}", "mpris:trackid",
                          g_variant_new_object_path(TRACK_OBJECT_PATH));
    g_variant_builder_add(&metadata, "{sv}", "xesam:title",
                          g_variant_new_string(mpris->title));
    if (mpris->uri && *mpris->uri)
        g_variant_builder_add(&metadata, "{sv}", "xesam:url",
                              g_variant_new_string(mpris->uri));
    if (mpris->duration_us > 0)
        g_variant_builder_add(&metadata, "{sv}", "mpris:length",
                              g_variant_new_int64(mpris->duration_us));
    return g_variant_builder_end(&metadata);
}

static void emit_player_property(UpMpris *mpris, const char *name,
                                 GVariant *value)
{
    if (!mpris->connection)
        return;
    GVariantBuilder changed;
    GVariantBuilder invalidated;
    g_variant_builder_init(&changed, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&changed, "{sv}", name, value);
    g_variant_builder_init(&invalidated, G_VARIANT_TYPE("as"));
    g_dbus_connection_emit_signal(
        mpris->connection, NULL, MPRIS_OBJECT_PATH,
        "org.freedesktop.DBus.Properties", "PropertiesChanged",
        g_variant_new("(sa{sv}as)", "org.mpris.MediaPlayer2.Player",
                      &changed, &invalidated),
        NULL);
}

static void method_call(GDBusConnection *connection,
                        const char *sender, const char *object_path,
                        const char *interface_name, const char *method_name,
                        GVariant *parameters,
                        GDBusMethodInvocation *invocation, void *user_data)
{
    (void) connection;
    (void) sender;
    (void) object_path;
    UpMpris *mpris = user_data;
    if (!strcmp(interface_name, "org.mpris.MediaPlayer2")) {
        if (!strcmp(method_name, "Quit"))
            queue_command(mpris, UP_MPRIS_COMMAND_QUIT, 0);
        g_dbus_method_invocation_return_value(invocation, NULL);
        return;
    }

    enum UpMprisCommand command = UP_MPRIS_COMMAND_NONE;
    int64_t value = 0;
    if (!strcmp(method_name, "Play"))
        command = UP_MPRIS_COMMAND_PLAY;
    else if (!strcmp(method_name, "Pause"))
        command = UP_MPRIS_COMMAND_PAUSE;
    else if (!strcmp(method_name, "PlayPause"))
        command = UP_MPRIS_COMMAND_PLAY_PAUSE;
    else if (!strcmp(method_name, "Stop"))
        command = UP_MPRIS_COMMAND_STOP;
    else if (!strcmp(method_name, "Seek")) {
        command = UP_MPRIS_COMMAND_SEEK;
        g_variant_get(parameters, "(x)", &value);
    } else if (!strcmp(method_name, "SetPosition")) {
        const char *track_id;
        g_variant_get(parameters, "(&ox)", &track_id, &value);
        if (strcmp(track_id, TRACK_OBJECT_PATH)) {
            g_dbus_method_invocation_return_value(invocation, NULL);
            return;
        }
        command = UP_MPRIS_COMMAND_SET_POSITION;
    }
    if (command != UP_MPRIS_COMMAND_NONE)
        queue_command(mpris, command, value);
    g_dbus_method_invocation_return_value(invocation, NULL);
}

static GVariant *get_property(GDBusConnection *connection,
                              const char *sender, const char *object_path,
                              const char *interface_name,
                              const char *property_name, GError **error,
                              void *user_data)
{
    (void) connection;
    (void) sender;
    (void) object_path;
    (void) error;
    UpMpris *mpris = user_data;
    if (!strcmp(interface_name, "org.mpris.MediaPlayer2")) {
        if (!strcmp(property_name, "CanQuit"))
            return g_variant_new_boolean(true);
        if (!strcmp(property_name, "Fullscreen") ||
            !strcmp(property_name, "CanSetFullscreen") ||
            !strcmp(property_name, "CanRaise") ||
            !strcmp(property_name, "HasTrackList"))
            return g_variant_new_boolean(false);
        if (!strcmp(property_name, "Identity"))
            return g_variant_new_string("Undefined Player");
        if (!strcmp(property_name, "DesktopEntry"))
            return g_variant_new_string("undefined-player");
        if (!strcmp(property_name, "SupportedUriSchemes") ||
            !strcmp(property_name, "SupportedMimeTypes"))
            return g_variant_new_strv(NULL, 0);
    } else if (!strcmp(interface_name, "org.mpris.MediaPlayer2.Player")) {
        if (!strcmp(property_name, "PlaybackStatus"))
            return g_variant_new_string(status_name(mpris->status));
        if (!strcmp(property_name, "LoopStatus"))
            return g_variant_new_string("None");
        if (!strcmp(property_name, "Rate") ||
            !strcmp(property_name, "Volume") ||
            !strcmp(property_name, "MinimumRate") ||
            !strcmp(property_name, "MaximumRate"))
            return g_variant_new_double(1.0);
        if (!strcmp(property_name, "Shuffle") ||
            !strcmp(property_name, "CanGoNext") ||
            !strcmp(property_name, "CanGoPrevious"))
            return g_variant_new_boolean(false);
        if (!strcmp(property_name, "Metadata"))
            return metadata_variant(mpris);
        if (!strcmp(property_name, "Position"))
            return g_variant_new_int64(mpris->position_us);
        if (!strcmp(property_name, "CanPlay") ||
            !strcmp(property_name, "CanPause") ||
            !strcmp(property_name, "CanSeek") ||
            !strcmp(property_name, "CanControl"))
            return g_variant_new_boolean(true);
    }
    return NULL;
}

static gboolean set_property(GDBusConnection *connection,
                             const char *sender, const char *object_path,
                             const char *interface_name,
                             const char *property_name, GVariant *value,
                             GError **error, void *user_data)
{
    (void) connection;
    (void) sender;
    (void) object_path;
    (void) interface_name;
    (void) value;
    (void) user_data;
    g_set_error(error, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                "%s is not supported", property_name);
    return false;
}

static const GDBusInterfaceVTable interface_vtable = {
    .method_call = method_call,
    .get_property = get_property,
    .set_property = set_property,
};

UpMpris *up_mpris_create(const char *title, const char *filename,
                         int64_t duration_us)
{
    UpMpris *mpris = g_new0(UpMpris, 1);
    mpris->title = g_strdup(title && *title ? title : "Unknown media");
    mpris->duration_us = duration_us > 0 ? duration_us : 0;
    mpris->status = UP_MPRIS_STATUS_PLAYING;
    if (filename && *filename) {
        char *absolute = g_canonicalize_filename(filename, NULL);
        GError *uri_error = NULL;
        mpris->uri = g_filename_to_uri(absolute, NULL, &uri_error);
        g_free(absolute);
        g_clear_error(&uri_error);
    }

    GError *error = NULL;
    mpris->connection = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!mpris->connection) {
        set_gerror(mpris, "could not connect to the user D-Bus", error);
        return mpris;
    }
    mpris->node_info = g_dbus_node_info_new_for_xml(introspection_xml, &error);
    if (!mpris->node_info) {
        set_gerror(mpris, "could not parse MPRIS interface data", error);
        return mpris;
    }
    mpris->root_registration = g_dbus_connection_register_object(
        mpris->connection, MPRIS_OBJECT_PATH,
        mpris->node_info->interfaces[0], &interface_vtable, mpris, NULL, &error);
    if (!mpris->root_registration) {
        set_gerror(mpris, "could not export the MPRIS root interface", error);
        return mpris;
    }
    mpris->player_registration = g_dbus_connection_register_object(
        mpris->connection, MPRIS_OBJECT_PATH,
        mpris->node_info->interfaces[1], &interface_vtable, mpris, NULL, &error);
    if (!mpris->player_registration) {
        set_gerror(mpris, "could not export the MPRIS player interface", error);
        return mpris;
    }

    snprintf(mpris->bus_name, sizeof(mpris->bus_name),
             "org.mpris.MediaPlayer2.undefined_player.instance%ld",
             (long) getpid());
    GVariant *reply = g_dbus_connection_call_sync(
        mpris->connection, "org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus", "RequestName",
        g_variant_new("(su)", mpris->bus_name, 0u),
        G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, -1, NULL, &error);
    guint32 result = 0;
    if (reply) {
        g_variant_get(reply, "(u)", &result);
        g_variant_unref(reply);
    }
    if (!reply || (result != 1 && result != 4)) {
        if (error)
            set_gerror(mpris, "could not own the MPRIS bus name", error);
        else
            set_error(mpris, "could not own the MPRIS bus name");
    }
    return mpris;
}

int up_mpris_active(const UpMpris *mpris)
{
    return mpris && mpris->connection && mpris->root_registration &&
        mpris->player_registration && !mpris->error[0];
}

const char *up_mpris_error(const UpMpris *mpris)
{
    return mpris && mpris->error[0] ? mpris->error : "unknown MPRIS error";
}

void up_mpris_dispatch(UpMpris *mpris)
{
    if (!up_mpris_active(mpris))
        return;
    while (g_main_context_iteration(NULL, false)) {}
}

enum UpMprisCommand up_mpris_take_command(UpMpris *mpris, int64_t *value)
{
    if (!mpris || mpris->command_read == mpris->command_write)
        return UP_MPRIS_COMMAND_NONE;
    const struct queued_command command = mpris->commands[mpris->command_read];
    mpris->command_read =
        (mpris->command_read + 1) % COMMAND_QUEUE_CAPACITY;
    if (value)
        *value = command.value;
    return command.command;
}

void up_mpris_update(UpMpris *mpris, enum UpMprisStatus status,
                     int64_t position_us)
{
    if (!up_mpris_active(mpris))
        return;
    mpris->position_us = position_us > 0 ? position_us : 0;
    if (mpris->status == status)
        return;
    mpris->status = status;
    emit_player_property(mpris, "PlaybackStatus",
                         g_variant_new_string(status_name(status)));
}

void up_mpris_seeked(UpMpris *mpris, int64_t position_us)
{
    if (!up_mpris_active(mpris))
        return;
    mpris->position_us = position_us > 0 ? position_us : 0;
    g_dbus_connection_emit_signal(
        mpris->connection, NULL, MPRIS_OBJECT_PATH,
        "org.mpris.MediaPlayer2.Player", "Seeked",
        g_variant_new("(x)", mpris->position_us), NULL);
}

void up_mpris_destroy(UpMpris *mpris)
{
    if (!mpris)
        return;
    if (mpris->connection && mpris->bus_name[0]) {
        GError *error = NULL;
        GVariant *reply = g_dbus_connection_call_sync(
            mpris->connection, "org.freedesktop.DBus",
            "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
            g_variant_new("(s)", mpris->bus_name), G_VARIANT_TYPE("(u)"),
            G_DBUS_CALL_FLAGS_NONE, -1, NULL, &error);
        if (reply)
            g_variant_unref(reply);
        g_clear_error(&error);
    }
    if (mpris->connection && mpris->player_registration)
        g_dbus_connection_unregister_object(mpris->connection,
                                            mpris->player_registration);
    if (mpris->connection && mpris->root_registration)
        g_dbus_connection_unregister_object(mpris->connection,
                                            mpris->root_registration);
    g_clear_pointer(&mpris->node_info, g_dbus_node_info_unref);
    g_clear_object(&mpris->connection);
    g_free(mpris->title);
    g_free(mpris->uri);
    g_free(mpris);
}
