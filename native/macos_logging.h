#ifndef UP_MACOS_LOGGING_H
#define UP_MACOS_LOGGING_H

#include <stdlib.h>

static inline void up_macos_configure_logging(int debug)
{
    // Set before the first Foundation log call. Apple's ML compiler emits
    // dozens of NSLog messages during startup. Keep them in the system log
    // without echoing them to stderr; player errors still use stderr directly.
    // Respect an explicit environment setting, and leave --debug unfiltered.
    if (!debug)
        setenv("CFLOG_FORCE_DISABLE_STDERR", "1", 0);
}

#endif
