/* Interposes getaddrinfo for mcp-stdio regression tests.
 *
 * PP_MCP_STDIO_DNS=offbox: localhost resolves to 127.0.0.2, a loopback
 * address the bridge must not dial. PP_MCP_STDIO_DNS=slow: slow.test sleeps
 * inside the lookup so a runtime drop that waits on it misses the deadline.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <netdb.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints,
                struct addrinfo **res) {
    static int (*real_getaddrinfo)(const char *, const char *, const struct addrinfo *,
                                   struct addrinfo **) = NULL;
    if (!real_getaddrinfo) {
        real_getaddrinfo = dlsym(RTLD_NEXT, "getaddrinfo");
    }
    const char *mode = getenv("PP_MCP_STDIO_DNS");
    if (mode != NULL && node != NULL) {
        if (strcmp(mode, "offbox") == 0 && strcasecmp(node, "localhost") == 0) {
            return real_getaddrinfo("127.0.0.2", service, hints, res);
        }
        if (strcmp(mode, "slow") == 0 && strcmp(node, "slow.test") == 0) {
            sleep(20);
            return EAI_NONAME;
        }
    }
    return real_getaddrinfo(node, service, hints, res);
}
