#pragma once
#include <stdbool.h>

bool up_mac_app_is_bundle(void);
void up_mac_app_init(void);
void up_mac_app_quit(void);
// Returned paths are malloc-owned; free with up_mac_app_free_path.
char *up_mac_app_take_file(void);
char *up_mac_app_choose_file(void);
void up_mac_app_free_path(char *path);
void up_mac_app_show_error(const char *message);
