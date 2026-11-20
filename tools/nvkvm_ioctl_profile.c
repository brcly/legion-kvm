/* Diagnostic only: preload into the owned Linux Vulkan workload, never games. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define BINS 512u
struct bin { _Atomic uint64_t key, count, wall_ns, max_ns; };
static struct bin bins[BINS];
static _Atomic uint64_t dropped;
static int output_fd = -1;
static _Thread_local int reentrant;
static _Atomic uint64_t poll_calls[2], poll_wall_ns[2];

static uint64_t now_ns(void)
{
    struct timespec time;
    if (clock_gettime(CLOCK_MONOTONIC, &time)) return 0;
    return (uint64_t)time.tv_sec*1000000000u+(uint64_t)time.tv_nsec;
}

__attribute__((constructor)) static void begin(void)
{
    const char *path = getenv("LEGION_IOCTL_PROFILE");
    if (path) output_fd = open(path, O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW, 0600);
}

int ioctl(int fd, unsigned long request, ...)
{
    va_list arguments;
    va_start(arguments, request);
    void *pointer = va_arg(arguments, void *);
    va_end(arguments);
    if (output_fd < 0 || reentrant) return (int)syscall(SYS_ioctl,fd,request,pointer);
    reentrant = 1;
    uint64_t start = now_ns();
    int result = (int)syscall(SYS_ioctl,fd,request,pointer);
    int saved_errno = errno;
    uint64_t elapsed = now_ns()-start;
    uint32_t inner = 0;
    /* Only inspect a successfully accessed NVOS54 header. */
    if (result >= 0 && pointer && request == 0xc020462aUL)
        memcpy(&inner, (const char *)pointer+8, sizeof(inner));
    uint64_t key = ((uint64_t)(uint32_t)request<<32)|inner;
    unsigned index = (unsigned)((key^(key>>32))*2654435761u)&(BINS-1);
    for (unsigned tried=0; tried<BINS; tried++, index=(index+1)&(BINS-1)) {
        uint64_t expected = 0;
        if (key && (atomic_load(&bins[index].key) == key ||
                    atomic_compare_exchange_strong(&bins[index].key,&expected,key) || expected == key)) {
            atomic_fetch_add(&bins[index].count,1);
            atomic_fetch_add(&bins[index].wall_ns,elapsed);
            uint64_t maximum = atomic_load(&bins[index].max_ns);
            while (maximum < elapsed && !atomic_compare_exchange_weak(&bins[index].max_ns,&maximum,elapsed)) {}
            reentrant = 0;
            errno = saved_errno;
            return result;
        }
    }
    atomic_fetch_add(&dropped,1);
    reentrant = 0;
    errno = saved_errno;
    return result;
}

int poll(struct pollfd *fds, nfds_t count, int timeout)
{
    uint64_t start = now_ns();
    int result = (int)syscall(SYS_poll, fds, count, timeout);
    int saved_errno = errno;
    unsigned blocking = timeout != 0;
    if (output_fd >= 0) {
        atomic_fetch_add(&poll_calls[blocking], 1);
        atomic_fetch_add(&poll_wall_ns[blocking], now_ns()-start);
    }
    errno = saved_errno;
    return result;
}

__attribute__((destructor)) static void finish(void)
{
    if (output_fd < 0) return;
    reentrant = 1;
    FILE *output = fdopen(output_fd,"w");
    if (!output) { close(output_fd); return; }
    fprintf(output,"{\"schema\":1,\"dropped\":%llu,\"commands\":[",(unsigned long long)atomic_load(&dropped));
    const char *separator = "";
    for (unsigned i=0;i<BINS;i++) {
        uint64_t key=atomic_load(&bins[i].key), count=atomic_load(&bins[i].count);
        if (!key || !count) continue;
        fprintf(output,"%s{\"ioctl\":%u,\"control\":%u,\"count\":%llu,\"wall_ns\":%llu,\"max_ns\":%llu}",separator,
                (uint32_t)(key>>32),(uint32_t)key,(unsigned long long)count,
                (unsigned long long)atomic_load(&bins[i].wall_ns),(unsigned long long)atomic_load(&bins[i].max_ns));
        separator=",";
    }
    fprintf(output,"],\"poll\":{\"nonblocking_calls\":%llu,\"blocking_calls\":%llu,\"nonblocking_wall_ns\":%llu,\"blocking_wall_ns\":%llu}}\n",
            (unsigned long long)atomic_load(&poll_calls[0]),
            (unsigned long long)atomic_load(&poll_calls[1]),
            (unsigned long long)atomic_load(&poll_wall_ns[0]),
            (unsigned long long)atomic_load(&poll_wall_ns[1]));
    fclose(output);
    output_fd=-1;
}
