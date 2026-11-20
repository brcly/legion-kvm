/* Owned disposable-guest test: race poll registration with file closure. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <unistd.h>

static _Atomic int failed;
static void *worker(void *unused)
{
    (void)unused;
    for (unsigned i=0; i<100; i++) {
        int fd=open("/dev/nvidiactl",O_RDWR|O_CLOEXEC);
        if (fd<0) { atomic_store(&failed,1); break; }
        struct pollfd event={ .fd=fd, .events=POLLIN };
        if (poll(&event,1,0)<0 || (event.revents&POLLNVAL)) atomic_store(&failed,1);
        close(fd);
    }
    return NULL;
}

int main(void)
{
    struct utsname system;
    if (getuid()!=0 || uname(&system) || !strstr(system.release,"-legion") ||
        access("/sys/module/nvkvm_guest/parameters/poll_async",F_OK)) {
        fputs("This probe runs only inside the disposable Legion research guest.\n",stderr);
        return 1;
    }
    pthread_t threads[4];
    unsigned created=0;
    for (;created<4;created++) if (pthread_create(&threads[created],NULL,worker,NULL)) break;
    for (unsigned i=0;i<created;i++) pthread_join(threads[i],NULL);
    if (created!=4 || atomic_load(&failed)) return 1;
    /* No force-unload: a retained file/module reference must drain normally. */
    for (unsigned attempt=0;attempt<100;attempt++) {
        if (!syscall(SYS_delete_module,"nvkvm_guest",O_NONBLOCK)) {
            puts("{\"passed\":true,\"poll_close_races\":400,\"module_unloaded\":true}");
            return 0;
        }
        if (errno!=EWOULDBLOCK && errno!=EBUSY && errno!=EAGAIN) { perror("delete_module"); return 1; }
        usleep(20000);
    }
    fputs("poll work retained a module reference after all user FDs closed\n",stderr);
    return 1;
}
