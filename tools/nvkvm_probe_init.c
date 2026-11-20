/* Disposable research guest only; never install this as the production init. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/klog.h>
#include <sys/wait.h>
#include <unistd.h>

static void stop(int failed)
{
    printf("LEGION_NVKVM_DONE failed=%d\n", failed);
    fflush(NULL);
    sync();
    reboot(RB_POWER_OFF);
    _exit(1);
}

static void mount_at(const char *source, const char *target, const char *type,
                     unsigned long flags)
{
    mkdir(target, 0755);
    if (mount(source, target, type, flags, NULL)) {
        perror(target);
        stop(1);
    }
}

int main(void)
{
    if (getpid() != 1) {
        fputs("This init must run as PID 1 in the disposable VM.\n", stderr);
        return 1;
    }
    setvbuf(stdout, NULL, _IONBF, 0);
    mount_at("proc", "/proc", "proc", 0);
    mount_at("sysfs", "/sys", "sysfs", 0);
    mount_at("devtmpfs", "/dev", "devtmpfs", 0);
    mount_at("tmpfs", "/tmp", "tmpfs", 0);
    mount_at("usr", "/usr", "virtiofs", MS_RDONLY);
    mount_at("nvidia", "/nvidia", "virtiofs", MS_RDONLY);
    mount_at("abi", "/abi", "virtiofs", MS_RDONLY);
    mount_at("research", "/research", "virtiofs", 0);
    if (access("/research/proton-enabled", F_OK) == 0)
        mount_at("proton", "/proton", "virtiofs", MS_RDONLY);
    if (access("/research/application-enabled", F_OK) == 0) {
        if (access("/research/application-ram-enabled", F_OK) == 0) {
            mount_at("application", "/application-source", "virtiofs", MS_RDONLY);
            mount_at("tmpfs", "/application", "tmpfs", 0);
        } else {
            mount_at("application", "/application", "virtiofs", 0);
        }
    }
    symlink("/usr/lib", "/lib");
    symlink("/abi", "/lib64");
    symlink("/usr/bin", "/bin");
    symlink("/research", "/frames");
    mkdir("/run", 0755);
    mkdir("/root", 0700);
    mkdir("/etc", 0755);
    mkdir("/dev/shm", 01777);
    mount("tmpfs", "/dev/shm", "tmpfs", 0, "mode=1777");
    for (int i = 0; i < 16; i++) {
        char path[64];
        snprintf(path, sizeof(path), "/modules/%d.ko", i);
        int fd = open(path, O_RDONLY);
        if (fd < 0 && errno == ENOENT && i > 0) break;
        char params[256] = {0};
        char param_path[64];
        snprintf(param_path, sizeof(param_path), "/modules/%d.params", i);
        int parameters = open(param_path, O_RDONLY);
        if (parameters >= 0) {
            ssize_t length = read(parameters, params, sizeof(params) - 1);
            close(parameters);
            if (length < 0) { perror(param_path); stop(1); }
        }
        if (fd < 0 || syscall(SYS_finit_module, fd, params, 0)) {
            perror(path);
            stop(1);
        }
        close(fd);
    }
    pid_t child = fork();
    if (child < 0) stop(1);
    if (child == 0) {
        clearenv();
        setenv("PATH", "/usr/bin:/bin", 1);
        setenv("HOME", "/root", 1);
        setenv("USER", "root", 1);
        setenv("LANG", "C", 1);
        setenv("LD_LIBRARY_PATH", "/abi:/usr/lib/x86_64-linux-gnu:/nvidia", 1);
        setenv("VK_DRIVER_FILES", "/nvidia/icd.json", 1);
        if (access("/nvidia/config/glvnd/egl_vendor.d/10_nvidia.json", F_OK) == 0) {
            setenv("__EGL_VENDOR_LIBRARY_FILENAMES",
                   "/nvidia/config/glvnd/egl_vendor.d/10_nvidia.json", 1);
            setenv("GBM_BACKENDS_PATH", "/nvidia/gbm", 1);
        }
        setenv("VK_LOADER_LAYERS_DISABLE", "~implicit~", 1);
        setenv("XDG_RUNTIME_DIR", "/run", 1);
        execl("/usr/bin/bash", "bash", "/research/guest.sh", (char *)NULL);
        perror("guest script");
        _exit(127);
    }
    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) stop(1);
    }
    char kernel_log[65536];
    int length = klogctl(3, kernel_log, sizeof(kernel_log));
    int log = open("/research/kernel.log", O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (log >= 0) {
        if (length > 0 && write(log, kernel_log, (size_t)length) != length)
            perror("kernel log write");
        close(log);
    }
    stop(!WIFEXITED(status) || WEXITSTATUS(status) != 0);
}
