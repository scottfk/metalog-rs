/*
 * faketime.c - LD_PRELOAD clock for the metalog-rs differential tests
 *
 * 20261111 - Created by Claude (claude.ai, Anthropic) - scottfk
 *
 * If $METALOG_FAKETIME names a readable file holding "SECONDS NANOSECONDS",
 * time(), gettimeofday() and clock_gettime(CLOCK_REALTIME) return that
 * instant.  The file is re-read on every call, so a test steps the clock by
 * rewriting it.  Other clocks (CLOCK_MONOTONIC: remote DNS refresh) pass
 * through.  Both the C reference and metalog-rs call these through the PLT,
 * so one shim controls both.  errno is preserved: metalog's warnp() prints
 * it after unrelated calls.
 *
 * Build: cc -shared -fPIC -O2 -o faketime.so faketime.c -ldl
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static int fake(struct timespec *ts)
{
    const char *f = getenv("METALOG_FAKETIME");
    char buf[64];
    long long s = 0, ns = 0;
    int fd, n, saved = errno, i = 0, neg = 0;

    if (!f || (fd = open(f, O_RDONLY | O_CLOEXEC)) < 0) {
        errno = saved;
        return 0;
    }
    n = (int) read(fd, buf, sizeof buf - 1);
    close(fd);
    errno = saved;
    if (n <= 0)
        return 0;
    buf[n] = 0;
    /* No stdio here: metalog-rs emulates stdio buffering, and the C
     * reference's FILE list should look the same with or without us. */
    if (buf[i] == '-') {
        neg = 1;
        i++;
    }
    while (buf[i] >= '0' && buf[i] <= '9')
        s = s * 10 + (buf[i++] - '0');
    while (buf[i] == ' ')
        i++;
    while (buf[i] >= '0' && buf[i] <= '9')
        ns = ns * 10 + (buf[i++] - '0');
    ts->tv_sec = neg ? -s : s;
    ts->tv_nsec = ns;
    return 1;
}

int clock_gettime(clockid_t id, struct timespec *ts)
{
    static int (*real)(clockid_t, struct timespec *);

    if (id == CLOCK_REALTIME && fake(ts))
        return 0;
    if (!real)
        real = (int (*)(clockid_t, struct timespec *)) dlsym(RTLD_NEXT, "clock_gettime");
    return real(id, ts);
}

time_t time(time_t *t)
{
    struct timespec ts;

    if (!fake(&ts))
        clock_gettime(CLOCK_REALTIME, &ts);
    if (t)
        *t = ts.tv_sec;
    return ts.tv_sec;
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz)
{
    struct timespec ts;

    (void) tz;
    if (!fake(&ts))
        clock_gettime(CLOCK_REALTIME, &ts);
    tv->tv_sec = ts.tv_sec;
    tv->tv_usec = ts.tv_nsec / 1000;
    return 0;
}
