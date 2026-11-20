/* Owned Vulkan test: export a genuinely pending GPU fence to a sync_file.
 * A separate CPU thread opens a timeline gate after 300 ms. No shaders,
 * memory imports, presentation, or changes to other processes/devices. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/sync_file.h>
#include <poll.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

struct gate {
    VkDevice device;
    VkSemaphore semaphore;
    VkResult result;
    double signal_ms;
    uint64_t final_value;
};

static double now_ms(void)
{
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return time.tv_sec * 1000.0 + time.tv_nsec / 1000000.0;
}

static VkResult open_gate(struct gate *gate)
{
    VkSemaphoreSignalInfo signal = {
        .sType = VK_STRUCTURE_TYPE_SEMAPHORE_SIGNAL_INFO,
        .semaphore = gate->semaphore, .value = gate->final_value,
    };
    return vkSignalSemaphore(gate->device, &signal);
}

static void *release_gate(void *argument)
{
    struct gate *gate = argument;
    struct timespec delay = { .tv_nsec = gate->final_value == 2 ? 150000000 : 300000000 };
    while (nanosleep(&delay, &delay) && errno == EINTR) {}
    if (gate->final_value == 2) {
        VkSemaphoreSignalInfo first = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_SIGNAL_INFO,
            .semaphore = gate->semaphore, .value = 1 };
        gate->result = vkSignalSemaphore(gate->device, &first);
        if (gate->result != VK_SUCCESS) return NULL;
        delay = (struct timespec){ .tv_nsec = 150000000 };
        while (nanosleep(&delay, &delay) && errno == EINTR) {}
    }
    gate->signal_ms = now_ms();
    gate->result = open_gate(gate);
    return NULL;
}

static bool owned_guest(void)
{
    struct utsname system;
    return getuid() == 0 && !uname(&system) && strstr(system.release, "-legion") &&
        !access("/sys/module/nvkvm_guest/parameters/poll_async", F_OK);
}

static int lifetime(const char *executable)
{
    if (!owned_guest()) return 2;
    /* Exec before creating any Vulkan objects/threads. Only the disposable
     * child abandons work; the parent verifies normal, non-forced unload. */
    pid_t child = fork();
    if (child < 0) return 1;
    if (!child) {
        execl(executable, executable, "--abandon-merged", NULL);
        _exit(2);
    }
    int status;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status)) return 1;
    for (unsigned attempt = 0; attempt < 160; attempt++) {
        if (!syscall(SYS_delete_module, "nvkvm_guest", O_NONBLOCK)) {
            puts("{\"schema\":1,\"passed\":true,\"abandoned_merged_import\":true,\"module_unloaded\":true}");
            return 0;
        }
        if (errno != EWOULDBLOCK && errno != EBUSY && errno != EAGAIN) return 1;
        struct timespec delay = { .tv_nsec = 50000000 };
        nanosleep(&delay, NULL);
    }
    fputs("sync-file: abandoned import retained a module reference\n", stderr);
    return 1;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--lifetime") == 0) return lifetime(argv[0]);
    bool abandon = argc == 2 && strcmp(argv[1], "--abandon-merged") == 0;
    if (abandon && !owned_guest()) return 2;
    bool unwrap = argc == 2 && strcmp(argv[1], "--import-unwrapped") == 0;
    bool merge_fences = abandon || (argc == 2 && strcmp(argv[1], "--merge") == 0);
    bool test_import = merge_fences || unwrap || (argc == 2 && strcmp(argv[1], "--import") == 0);
    if (argc != 1 && !test_import) return 2;
    VkInstance instance = VK_NULL_HANDLE;
    VkDevice device = VK_NULL_HANDLE;
    VkFence fence = VK_NULL_HANDLE;
    VkFence second = VK_NULL_HANDLE;
    VkFence imported_done = VK_NULL_HANDLE;
    VkSemaphore imported = VK_NULL_HANDLE;
    struct gate gate = { .final_value = merge_fences ? 2 : 1 };
    pthread_t thread;
    bool started = false, submitted = false, passed = false, import_timeout = false;
    int fd = -1, pending = -2, ready = -2, initial_status = -2, final_status = -2;
    int imported_pending = -1;
    int partial_fd = -1, partial_pending = -1;
    double export_ms = -1, observed_ms = -1;
    const char *stage = "instance";
    VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "legion-pending-sync-file", .apiVersion = VK_API_VERSION_1_3 };
    VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app };
    if (vkCreateInstance(&ici, NULL, &instance) != VK_SUCCESS) goto done;
    VkPhysicalDevice physical;
    uint32_t count = 1;
    stage = "physical-device";
    if (vkEnumeratePhysicalDevices(instance, &count, &physical) != VK_SUCCESS || count != 1) goto done;
    VkQueueFamilyProperties queues[64];
    count = 64;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &count, queues);
    uint32_t family;
    for (family = 0; family < count; family++)
        if (queues[family].queueFlags & VK_QUEUE_GRAPHICS_BIT) break;
    if (family == count) goto done;
    if (test_import && queues[family].queueCount < 2) goto done;
    float priority[] = {1, 1};
    VkDeviceQueueCreateInfo qci = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family, .queueCount = test_import ? 2 : 1, .pQueuePriorities = priority };
    const char *extensions[] = { VK_KHR_EXTERNAL_FENCE_FD_EXTENSION_NAME,
        VK_KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION_NAME };
    VkPhysicalDeviceTimelineSemaphoreFeatures timeline = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES, .timelineSemaphore = VK_TRUE };
    VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .pNext = &timeline,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci,
        .enabledExtensionCount = test_import ? 2 : 1, .ppEnabledExtensionNames = extensions };
    stage = "device";
    if (vkCreateDevice(physical, &dci, NULL, &device) != VK_SUCCESS) goto done;
    gate.device = device;
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);
    VkQueue import_queue = queue;
    if (test_import) vkGetDeviceQueue(device, family, 1, &import_queue);
    VkSemaphoreTypeCreateInfo type = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO,
        .semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE, .initialValue = 0 };
    VkSemaphoreCreateInfo sci = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO, .pNext = &type };
    stage = "semaphore";
    if (vkCreateSemaphore(device, &sci, NULL, &gate.semaphore) != VK_SUCCESS) goto done;
    VkExportFenceCreateInfo export = { .sType = VK_STRUCTURE_TYPE_EXPORT_FENCE_CREATE_INFO,
        .handleTypes = VK_EXTERNAL_FENCE_HANDLE_TYPE_SYNC_FD_BIT };
    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO, .pNext = &export };
    stage = "fence";
    if (vkCreateFence(device, &fci, NULL, &fence) != VK_SUCCESS) goto done;
    uint64_t value = 1;
    VkTimelineSemaphoreSubmitInfo tsi = { .sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
        .waitSemaphoreValueCount = 1, .pWaitSemaphoreValues = &value };
    VkPipelineStageFlags wait_stage = VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
    VkSubmitInfo submit = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .pNext = &tsi,
        .waitSemaphoreCount = 1, .pWaitSemaphores = &gate.semaphore, .pWaitDstStageMask = &wait_stage };
    stage = "submit";
    if (vkQueueSubmit(queue, 1, &submit, fence) != VK_SUCCESS) goto done;
    submitted = true;
    if (merge_fences) {
        if (vkCreateFence(device, &fci, NULL, &second) != VK_SUCCESS) goto done;
        value = 2;
        if (vkQueueSubmit(queue, 1, &submit, second) != VK_SUCCESS) goto done;
    }
    stage = "thread";
    if (pthread_create(&thread, NULL, release_gate, &gate)) goto done;
    started = true;
    PFN_vkGetFenceFdKHR get_fd = (PFN_vkGetFenceFdKHR)vkGetDeviceProcAddr(device, "vkGetFenceFdKHR");
    if (!get_fd) goto done;
    VkFenceGetFdInfoKHR get = { .sType = VK_STRUCTURE_TYPE_FENCE_GET_FD_INFO_KHR,
        .fence = fence, .handleType = VK_EXTERNAL_FENCE_HANDLE_TYPE_SYNC_FD_BIT };
    stage = "export";
    double start = now_ms();
    if (get_fd(device, &get, &fd) != VK_SUCCESS) goto done;
    export_ms = now_ms() - start;
    if (fd < 0) goto done; /* Already-complete sentinel cannot prove pending semantics. */
    if (merge_fences) {
        VkFenceGetFdInfoKHR second_get = get;
        second_get.fence = second;
        int second_fd = -1;
        if (get_fd(device, &second_get, &second_fd) != VK_SUCCESS || second_fd < 0) goto done;
        struct sync_merge_data merge = { .name = "legion-two-fences", .fd2 = second_fd, .fence = -1 };
        int result = ioctl(fd, SYNC_IOC_MERGE, &merge);
        close(second_fd);
        if (result || merge.fence < 0) goto done;
        partial_fd = fd;
        fd = merge.fence;
    }
    if (unwrap) {
        /* The kernel unwraps chains/arrays and deduplicates a merge with
         * itself. Exercise the bridge's direct native-descriptor path. */
        struct sync_merge_data merge = { .name = "legion-unwrap", .fd2 = fd, .fence = -1 };
        if (ioctl(fd, SYNC_IOC_MERGE, &merge) || merge.fence < 0) goto done;
        close(fd);
        fd = merge.fence;
    }
    struct sync_file_info info = {0};
    stage = "pending-status";
    if (ioctl(fd, SYNC_IOC_FILE_INFO, &info)) goto done;
    initial_status = info.status;
    struct pollfd poll_fd = { .fd = fd, .events = POLLIN };
    pending = poll(&poll_fd, 1, 20);
    if (pending != 0 || initial_status != 0) goto done;
    if (test_import) {
        stage = "import-semaphore";
        fprintf(stderr, "sync-file: begin pending semaphore import\n");
        VkSemaphoreCreateInfo binary = { .sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO };
        if (vkCreateSemaphore(device, &binary, NULL, &imported) != VK_SUCCESS) goto done;
        PFN_vkImportSemaphoreFdKHR import_fd = (PFN_vkImportSemaphoreFdKHR)
            vkGetDeviceProcAddr(device, "vkImportSemaphoreFdKHR");
        if (!import_fd) goto done;
        int copy = dup(fd);
        if (copy < 0) goto done;
        VkImportSemaphoreFdInfoKHR import = {
            .sType = VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_FD_INFO_KHR,
            .semaphore = imported, .flags = VK_SEMAPHORE_IMPORT_TEMPORARY_BIT,
            .handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT, .fd = copy,
        };
        VkResult imported_result = import_fd(device, &import);
        fprintf(stderr, "sync-file: semaphore import returned %d\n", imported_result);
        if (imported_result != VK_SUCCESS) {
            close(copy);
            goto done;
        }
        VkFenceCreateInfo plain = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
        if (vkCreateFence(device, &plain, NULL, &imported_done) != VK_SUCCESS) goto done;
        VkSubmitInfo wait = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
            .waitSemaphoreCount = 1, .pWaitSemaphores = &imported, .pWaitDstStageMask = &wait_stage };
        stage = "import-submit";
        fprintf(stderr, "sync-file: begin imported semaphore submission\n");
        VkResult submit_result = vkQueueSubmit(import_queue, 1, &wait, imported_done);
        fprintf(stderr, "sync-file: imported semaphore submission returned %d\n", submit_result);
        if (submit_result != VK_SUCCESS) goto done;
        stage = "import-pending";
        VkResult before_gate = vkWaitForFences(device, 1, &imported_done, VK_TRUE, 20000000ULL);
        imported_pending = before_gate == VK_TIMEOUT;
        if (!imported_pending) goto done;
        if (abandon) {
            puts("{\"schema\":1,\"passed\":true,\"scope\":\"intentional owned-child cancellation\",\"import_pending\":true}");
            fflush(NULL);
            _exit(0);
        }
        if (merge_fences) {
            stage = "merge-partial";
            struct pollfd first_ready = { .fd = partial_fd, .events = POLLIN };
            if (poll(&first_ready, 1, 500) != 1) goto done;
            struct sync_file_info partial = {0};
            if (ioctl(fd, SYNC_IOC_FILE_INFO, &partial) || partial.status != 0) goto done;
            partial_pending = vkWaitForFences(device, 1, &imported_done, VK_TRUE, 30000000ULL) == VK_TIMEOUT;
            if (!partial_pending) goto done;
        }
    }
    stage = "completion";
    ready = poll(&poll_fd, 1, 3000);
    observed_ms = now_ms();
    info = (struct sync_file_info){0};
    if (ioctl(fd, SYNC_IOC_FILE_INFO, &info)) goto done;
    final_status = info.status;
    passed = ready == 1 && (poll_fd.revents & POLLIN) && final_status == 1;
    if (passed && test_import) {
        stage = "import-completion";
        VkResult completion = vkWaitForFences(device, 1, &imported_done, VK_TRUE, 3000000000ULL);
        import_timeout = completion == VK_TIMEOUT;
        passed = completion == VK_SUCCESS;
    }
done:
    if (test_import) fprintf(stderr, "sync-file: cleanup at %s, passed=%d\n", stage, passed);
    if (started) pthread_join(thread, NULL);
    else if (submitted) gate.result = open_gate(&gate);
    if (import_timeout) {
        /* An accepted GPU wait cannot be cancelled through Vulkan. End only
         * this owned test process instead of hanging in device teardown or
         * destroying resources that the pending queue could still access. */
        fprintf(stderr, "sync-file: imported GPU wait timed out; aborting owned test process\n");
    } else if (device && submitted) {
        if (test_import) fprintf(stderr, "sync-file: waiting for device idle\n");
        vkDeviceWaitIdle(device);
    }
    if (fd >= 0) close(fd);
    if (partial_fd >= 0) close(partial_fd);
    if (!import_timeout) {
        if (fence) vkDestroyFence(device, fence, NULL);
        if (second) vkDestroyFence(device, second, NULL);
        if (imported_done) vkDestroyFence(device, imported_done, NULL);
        if (imported) vkDestroySemaphore(device, imported, NULL);
        if (gate.semaphore) vkDestroySemaphore(device, gate.semaphore, NULL);
        if (device) vkDestroyDevice(device, NULL);
        if (instance) vkDestroyInstance(instance, NULL);
    }
    passed = passed && gate.result == VK_SUCCESS;
    printf("{\"schema\":1,\"passed\":%s,\"stage\":\"%s\",\"import_test\":%s,\"unwrapped\":%s,"
           "\"merged\":%s,\"partial_pending\":%d,\"import_pending\":%d,\"export_ms\":%.3f,"
           "\"initial_status\":%d,\"pending_poll\":%d,\"completion_poll\":%d,\"final_status\":%d,"
           "\"signal_to_observation_ms\":%.3f}\n",
           passed ? "true" : "false", stage, test_import ? "true" : "false",
           unwrap ? "true" : "false", merge_fences ? "true" : "false", partial_pending, imported_pending, export_ms,
           initial_status, pending, ready, final_status,
           observed_ms >= 0 && gate.signal_ms > 0 ? observed_ms - gate.signal_ms : -1);
    if (import_timeout) {
        fflush(NULL);
        _exit(1);
    }
    return passed ? 0 : 1;
}
