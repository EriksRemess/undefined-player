#include "mpris.h"

#include <gio/gio.h>
#include <glib.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

#define MPRIS_OBJECT_PATH "/org/mpris/MediaPlayer2"

struct UpMpris {
    GDBusConnection *connection;
    GMainContext *context;
    GDBusNodeInfo *node_info;
    guint root_registration;
    guint player_registration;
    char *bus_name;
    char error[256];
    UpMprisCallbacks callbacks;
};

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

static GVariant *value_variant(const UpMprisValue *value)
{
    switch (value->kind) {
    case UP_MPRIS_VALUE_BOOL: return g_variant_new_boolean(value->integer != 0);
    case UP_MPRIS_VALUE_INT64: return g_variant_new_int64(value->integer);
    case UP_MPRIS_VALUE_DOUBLE: return g_variant_new_double(value->real);
    case UP_MPRIS_VALUE_STRING: return g_variant_new_string(value->text);
    case UP_MPRIS_VALUE_EMPTY_STRINGS: return g_variant_new_strv(NULL, 0);
    case UP_MPRIS_VALUE_METADATA: {
        GVariantBuilder metadata;
        g_variant_builder_init(&metadata, G_VARIANT_TYPE("a{sv}"));
        g_variant_builder_add(&metadata, "{sv}", "mpris:trackid",
                              g_variant_new_object_path(value->track_id));
        g_variant_builder_add(&metadata, "{sv}", "xesam:title",
                              g_variant_new_string(value->title));
        if (value->artist)
            g_variant_builder_add(&metadata, "{sv}", "xesam:artist",
                                  g_variant_new_strv(&value->artist, 1));
        if (value->art_uri)
            g_variant_builder_add(&metadata, "{sv}", "mpris:artUrl",
                                  g_variant_new_string(value->art_uri));
        if (value->uri)
            g_variant_builder_add(&metadata, "{sv}", "xesam:url",
                                  g_variant_new_string(value->uri));
        if (value->duration_us > 0)
            g_variant_builder_add(&metadata, "{sv}", "mpris:length",
                                  g_variant_new_int64(value->duration_us));
        return g_variant_builder_end(&metadata);
    }
    default: return NULL;
    }
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
    int64_t value = 0;
    const char *track_id = NULL;
    if (g_variant_is_of_type(parameters, G_VARIANT_TYPE("(x)")))
        g_variant_get(parameters, "(x)", &value);
    else if (g_variant_is_of_type(parameters, G_VARIANT_TYPE("(ox)")))
        g_variant_get(parameters, "(&ox)", &track_id, &value);
    mpris->callbacks.command(mpris->callbacks.data,
                              !strcmp(interface_name, "org.mpris.MediaPlayer2"),
                              method_name, track_id, value);
    g_dbus_method_invocation_return_value(invocation, NULL);
}

static GVariant *get_property(GDBusConnection *connection,
                              const char *sender, const char *object_path,
                              const char *interface_name,
                              const char *property_name, GError **error,
                              void *user_data)
{
    (void) connection; (void) sender; (void) object_path; (void) error;
    UpMpris *mpris = user_data;
    UpMprisValue value = {0};
    if (!mpris->callbacks.property(mpris->callbacks.data,
                                   !strcmp(interface_name, "org.mpris.MediaPlayer2"),
                                   property_name, &value))
        return NULL;
    return value_variant(&value);
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

UpMpris *up_mpris_create(const char *bus_name, const char *introspection_xml,
                         const UpMprisCallbacks *callbacks)
{
    UpMpris *mpris = g_new0(UpMpris, 1);
    mpris->bus_name = g_strdup(bus_name);
    mpris->callbacks = *callbacks;
    mpris->context = g_main_context_new();

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
    g_main_context_push_thread_default(mpris->context);
    mpris->root_registration = g_dbus_connection_register_object(
        mpris->connection, MPRIS_OBJECT_PATH,
        mpris->node_info->interfaces[0], &interface_vtable, mpris, NULL, &error);
    if (mpris->root_registration)
        mpris->player_registration = g_dbus_connection_register_object(
            mpris->connection, MPRIS_OBJECT_PATH,
            mpris->node_info->interfaces[1], &interface_vtable, mpris, NULL, &error);
    g_main_context_pop_thread_default(mpris->context);
    if (!mpris->root_registration) {
        set_gerror(mpris, "could not export the MPRIS root interface", error);
        return mpris;
    }
    if (!mpris->player_registration) {
        set_gerror(mpris, "could not export the MPRIS player interface", error);
        return mpris;
    }

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
    while (g_main_context_iteration(mpris->context, false)) {}
}

void up_mpris_status_changed(UpMpris *mpris, const char *status)
{
    if (up_mpris_active(mpris))
        emit_player_property(mpris, "PlaybackStatus", g_variant_new_string(status));
}

void up_mpris_seeked(UpMpris *mpris, int64_t position_us)
{
    if (!up_mpris_active(mpris))
        return;
    g_dbus_connection_emit_signal(
        mpris->connection, NULL, MPRIS_OBJECT_PATH,
        "org.mpris.MediaPlayer2.Player", "Seeked",
        g_variant_new("(x)", position_us), NULL);
}

void up_mpris_navigation_changed(UpMpris *mpris, bool previous, bool next)
{
    if (!up_mpris_active(mpris))
        return;
    emit_player_property(mpris, "CanGoPrevious", g_variant_new_boolean(previous));
    emit_player_property(mpris, "CanGoNext", g_variant_new_boolean(next));
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
    g_main_context_unref(mpris->context);
    g_free(mpris->bus_name);
    g_free(mpris);
}
